"""End-to-end test of egm-team's audit path: egm-dsh (real DeepSeek Harness binary, model faked
by fake_deepseek.py) -> its `--events` file -> egm-audit -> two real Vesdb servers sharing one
bucket (high availability as merged on Vesdb's master) -> replay from Vesdb.

  A. events: `--events` writes JSON lines; the terminal output is the same with and without it;
     the domain events come in workflow order and replaying them gives the gate egm-dsh saved
  B. Vesdb: pushing them writes one event each under /egm/<project>/<feature>; replaying from
     Vesdb alone gives the same phase and doc hash; pushing again (same job) writes nothing new;
     every event has a signed receipt with an inclusion proof
  C. failover: the writer is killed; the next push goes to the standby, which takes over; every
     event lands exactly once
  D. tampering: vesdb-verify passes the bucket, and fails it after one log object is changed

usage: python3 run_audit_e2e.py EGM_BINARY EGM_AUDIT VESDB VESDB_VERIFY DSH_COMMAND...
"""
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
EGM_AUDIT, VESDB, VERIFY = sys.argv[2], sys.argv[3], sys.argv[4]
sys.argv = [sys.argv[0], sys.argv[1]] + sys.argv[5:]
import run_fix_e2e as fx  # noqa: E402  (same scenario, same scripted model)

TOKEN = "t" * 40


def health(port, path):
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=2) as r:
            return r.status
    except urllib.error.HTTPError as e:
        return e.code
    except Exception:
        return None


def start_vesdb(work, key, bucket, port, name):
    log = open(work / f"vesdb-{name}.log", "w")
    p = subprocess.Popen([VESDB, "run", "--data-directory-temporary", "--storage", f"file://{bucket}", "--role", "auto",
                          "--identity", name, "--api-token", TOKEN, "--signing-key-file", str(key),
                          "--listen", f"127.0.0.1:{port}",
                          "--heartbeat-interval-ms", "500", "--stale-after-ms", "2500", "--tail-interval-ms", "100"],
                         stdout=log, stderr=subprocess.STDOUT)
    for _ in range(200):
        if health(port, "/health/ready") is not None:
            return p
        if p.poll() is not None:
            break
        time.sleep(0.1)
    raise SystemExit(f"vesdb {name} did not start: " + (work / f"vesdb-{name}.log").read_text()[-2000:])


def audit(env, *args):
    r = subprocess.run([EGM_AUDIT, *args], capture_output=True, text=True, env=env, timeout=180)
    return r.returncode, r.stdout, r.stderr


def main():
    a = fx.Project("audit", [{"match": fx.GOLD, "keep": True, "text": json.dumps(fx.CLEAN_ALL)}] + fx.builder(fx.FIXED)
                   + [{"match": fx.DIFF, "text": fx.MINOR}], with_test=True)
    work = Path(tempfile.mkdtemp(prefix="egm-audit-"))
    ev = work / "events.jsonl"
    procs = {}
    out = {}
    try:
        out["fix"] = a.egm("--events", str(ev), "fix", "gap-ids", fx.PROBLEM, "--files", "jot/cli.py", "--check", fx.CHECK)
        out["gate"] = a.egm("--events", str(ev), "gate", "gap-ids", "--no-mutation")
        plain = a.egm("status")
        with_ev = a.egm("--events", str(work / "status.jsonl"), "status")
        feat = a.feature("gap-ids")
        lines = [json.loads(l) for l in ev.read_text().splitlines() if l.strip()]
        domain = [l for l in lines if l["type"] == "domain"]
        kinds = [d["kind"] for d in domain]
        rc, rep, _ = audit(os.environ, "replay", str(ev))
        out["replay"] = rep

        # B. two Vesdb servers on one bucket
        key = work / "log.key"
        subprocess.run([VESDB, "keygen", "--name", "egm-test.local/log", "--out", str(key)], check=True, capture_output=True)
        bucket = work / "bucket"
        bucket.mkdir()
        p1, p2 = fx.free_port(), fx.free_port()
        procs["a"] = start_vesdb(work, key, bucket, p1, "a")
        time.sleep(1.0)
        procs["b"] = start_vesdb(work, key, bucket, p2, "b")
        for _ in range(100):
            if health(p1, "/health/writer") == 200 or health(p2, "/health/writer") == 200:
                break
            time.sleep(0.1)
        writer = "a" if health(p1, "/health/writer") == 200 else "b"
        env = dict(os.environ, VESDB_URL=f"http://127.0.0.1:{p1},http://127.0.0.1:{p2}", VESDB_TOKEN=TOKEN)
        out["push"] = audit(env, "push", str(ev), "--project", "jot", "--job", "job1")
        out["replay-vesdb"] = audit(env, "replay-vesdb", "--project", "jot")
        out["push-again"] = audit(env, "push", str(ev), "--project", "jot", "--job", "job1")

        def count(project):
            req = urllib.request.Request(f"http://127.0.0.1:{p1 if writer == 'a' else p2}/api/v2/data/read-events", method="QUERY",
                                         data=json.dumps({"subject": f"/egm/{project}", "options": {"recursive": True}}).encode(),
                                         headers={"Authorization": f"Bearer {TOKEN}", "Content-Type": "application/json"})
            with urllib.request.urlopen(req, timeout=10) as r:
                return [json.loads(l)["data"] for l in r.read().decode().splitlines() if json.loads(l)["type"] == "event"]

        first = count("jot")
        out["receipt"] = audit(env, "receipt", first[0]["id"])

        # C. failover: kill the writer, push a second project
        procs[writer].send_signal(signal.SIGKILL)
        procs[writer].wait()
        other = "b" if writer == "a" else "a"
        t0 = time.time()
        out["push-failover"] = audit(env, "push", str(ev), "--project", "jot2", "--job", "job2")
        failover_s = time.time() - t0
        new_port = p2 if other == "b" else p1
        writer_now = health(new_port, "/health/writer")
        writer = other

        def count_on(port, project):
            req = urllib.request.Request(f"http://127.0.0.1:{port}/api/v2/data/read-events", method="QUERY",
                                         data=json.dumps({"subject": f"/egm/{project}", "options": {"recursive": True}}).encode(),
                                         headers={"Authorization": f"Bearer {TOKEN}", "Content-Type": "application/json"})
            with urllib.request.urlopen(req, timeout=10) as r:
                return [json.loads(l)["data"] for l in r.read().decode().splitlines() if json.loads(l)["type"] == "event"]

        after_jot = count_on(new_port, "jot")
        after_jot2 = count_on(new_port, "jot2")
        out["replay-jot2"] = audit(dict(env, VESDB_URL=f"http://127.0.0.1:{new_port}"), "replay-vesdb", "--project", "jot2")

        # D. tampering, with every server stopped
        for p in procs.values():
            if p.poll() is None:
                p.terminate()
                p.wait(timeout=10)
        good = subprocess.run([VERIFY, "bucket", f"file://{bucket}", "--log-key", str(key) + ".pub"], capture_output=True, text=True, timeout=120)
        logs = sorted((bucket / "log").glob("*"))
        victim = logs[len(logs) // 2]
        data = bytearray(victim.read_bytes())
        data[len(data) // 2] ^= 0x01
        os.chmod(victim, 0o644)
        victim.write_bytes(bytes(data))
        bad = subprocess.run([VERIFY, "bucket", f"file://{bucket}", "--log-key", str(key) + ".pub"], capture_output=True, text=True, timeout=120)
    finally:
        for p in procs.values():
            if p.poll() is None:
                p.kill()
        a.close()

    expect_phase = feat["gate"]["phase"]
    expect_hash = str(feat["gate"]["doc_hash"])
    n = len(domain)
    checks = {
        "A: --events writes JSON lines with terminal lines and domain events": n >= 4 and any(l["type"] == "line" for l in lines),
        "A: the terminal output is the same with and without --events": plain[1] == with_ev[1] and plain[0] == with_ev[0] == 0,
        "A: domain events in workflow order: created first, gate verdict last": kinds[0] == "feature.created" and kinds[-1] == "gate.verdict"
            and kinds.index("goldfish.verdict") < kinds.index("build.completed") < kinds.index("gate.verdict"),
        "A: domain events carry hashes, not source code": "def add" not in ev.read_text() and all("text" not in d["data"] for d in domain),
        "A: replaying the events file gives the gate egm-dsh saved": rep.strip() == f"gap-ids\t{expect_phase}\t{expect_hash}",
        "B: push writes one Vesdb event per domain event": out["push"][0] == 0 and len(first) == n,
        "B: events are stored under /egm/jot/gap-ids as io.egm.* types": all(e["subject"] == "/egm/jot/gap-ids" and e["type"].startswith("io.egm.") for e in first),
        "B: replaying from Vesdb alone gives the same phase and doc hash": out["replay-vesdb"][1].strip() == f"gap-ids\t{expect_phase}\t{expect_hash}",
        "B: pushing again with the same job writes nothing new": out["push-again"][0] == 0 and len(count_on(new_port, "jot")) == n,
        "B: every event has a signed receipt with an inclusion proof": out["receipt"][0] == 0 and '"receipt"' in out["receipt"][1] and '"inclusion"' in out["receipt"][1],
        "C: after the writer is killed the push still succeeds": out["push-failover"][0] == 0,
        "C: the standby took over as writer": writer_now == 200,
        "C: nothing lost or doubled: the first project intact, the second exactly once": len(after_jot) == n and len(after_jot2) == n
            and len({e["id"] for e in after_jot2}) == n,
        "C: replay of the project written during failover is right": out["replay-jot2"][1].strip() == f"gap-ids\t{expect_phase}\t{expect_hash}",
        "D: vesdb-verify passes the untouched bucket": good.returncode == 0,
        "D: vesdb-verify fails after one byte of one log object changes": bad.returncode != 0,
    }
    for name, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + name)
    print("SUMMARY " + json.dumps({"passed": sum(map(bool, checks.values())), "failed": sum(not v for v in checks.values()),
                                   "events": n, "failover_seconds": round(failover_s, 1), "kinds": kinds}))
    if not all(checks.values()):
        for k, v in out.items():
            print(f"--- {k}: {v}"[:1500])
        print("--- verify good:", good.stdout[-800:], good.stderr[-800:])
        print("--- verify bad:", bad.stdout[-800:], bad.stderr[-800:])
        sys.exit(1)
    shutil.rmtree(work, ignore_errors=True)
    shutil.rmtree(a.work, ignore_errors=True)


if __name__ == "__main__":
    main()
