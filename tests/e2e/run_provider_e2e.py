"""End-to-end test of a provider that refuses every request (HTTP 402, Insufficient
Balance), on the real DeepSeek Harness binary. A real Colab run hit exactly this: every
model call was refused, and the run spent minutes producing INCOMPLETE results that
did not say why.

  * `egm-dsh doctor` reads the balance and says it is not enough, with where to top up
  * a goldfish round says the account has no credit, not just "a reviewer failed"
  * the head-to-head stops starting docs after the first refusal and says why
  * the smoke run names the refusal as its failure
  * the notebook's preflight reads the balance and would skip the real-model step

usage: python3 run_provider_e2e.py EGM_BINARY DSH_COMMAND...
"""
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
CRATE = HERE.parent.parent
EGM, DSH = str(Path(sys.argv[1]).resolve()), sys.argv[2:]
NOTEBOOK_CELL = os.environ.get("EGM_NOTEBOOK_CELL")   # optional: also test the notebook's preflight
UPSTREAM = os.environ.get("EGM_UPSTREAM")             # optional: also test the head-to-head (needs google/elephant-goldfish-model)
HAVE_UPSTREAM = bool(UPSTREAM) and (Path(UPSTREAM) / "skills" / "egm_review" / "SKILL.md").exists()


def wait_port(port, timeout=60):
    """Wait until the local server on `port` accepts connections (a fixed sleep is not enough under load)."""
    end = time.time() + timeout
    while time.time() < end:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=1):
                return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError(f"the local server on port {port} did not start within {timeout}s")


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def main():
    work = Path(tempfile.mkdtemp(prefix="egm-dsh-provider-"))
    proj = work / "jot"
    shutil.copytree(CRATE / "sample" / "before", proj, ignore=shutil.ignore_patterns("__pycache__"))
    (proj / "docs" / "designs").mkdir(parents=True)
    shutil.copy2(CRATE / "bench" / "corpus" / "tags-complete.md", proj / "docs" / "designs" / "tags.md")
    (work / "rules.json").write_text(json.dumps([{"match": "", "keep": True, "http_error": 402, "message": "Insufficient Balance"}]))
    (work / "balance.json").write_text(json.dumps({"is_available": False, "balance_infos": [
        {"currency": "USD", "total_balance": "0.00", "granted_balance": "0.00", "topped_up_balance": "0.00"}]}))
    port = free_port()
    server = subprocess.Popen([sys.executable, "-I", str(HERE / "fake_deepseek.py"), str(port), str(work / "rules.json"), str(work)])
    wait_port(port)
    env = dict(os.environ, DEEPSEEK_API_KEY="fake", DEEPSEEK_BASE_URL=f"http://127.0.0.1:{port}", NO_COLOR="1")

    def run(cmd, cwd=proj, timeout=600):
        p = subprocess.run(cmd, cwd=cwd, env=env, text=True, capture_output=True, timeout=timeout)
        out = (p.stdout + p.stderr).rstrip()
        print(f"$ {' '.join(map(str, cmd))[:160]}\n{out[-2500:]}\n", flush=True)
        return p.returncode, out

    try:
        run([EGM, "init", "--agent", "dsh", "--preset", "economy", "--dsh-command", *DSH])
        run([EGM, "adopt", "tags", "--doc", "docs/designs/tags.md"])
        doctor_empty = run([EGM, "doctor"])
        gold = run([EGM, "goldfish", "tags", "--no-verify"])
        t0 = time.time()
        cmp = run([sys.executable, str(CRATE / "bench" / "compare.py"), "--egm-dsh", EGM, "--upstream", UPSTREAM,
                   "--jobs", "2", "--timeout", "120", "--out", str(work / "cmp.json"), "--dsh", *DSH], cwd=work, timeout=900) if HAVE_UPSTREAM else (0, "")
        cmp_secs = time.time() - t0
        summary = json.loads((work / "cmp.json").read_text())["summary"] if (work / "cmp.json").exists() else {}
        smoke = run([sys.executable, str(CRATE / "tests" / "smoke_real.py"), EGM, "--preset", "economy", "--out", str(work / "smoke.json"),
                     "--dsh-command", *DSH], cwd=work, timeout=900)
        smoke_sum = json.loads((work / "smoke.json").read_text()) if (work / "smoke.json").exists() else {}
        pre = None
        if NOTEBOOK_CELL:
            src = Path(NOTEBOOK_CELL).read_text()
            # the cell also embeds this crate's files as strings; the real code starts at a line start
            start = src.index("\ndef deepseek_balance():") + 1
            fn = src[start:src.index("\nif RUN_REAL_DSH and dsh:", start)]
            ns = {"os": os, "json": json, "urllib": __import__("urllib.request")}
            old = dict(os.environ)
            os.environ.update(env)
            try:
                exec(fn, ns)
                pre = ns["deepseek_balance"]()
            finally:
                os.environ.clear()
                os.environ.update(old)
        (work / "balance.json").write_text(json.dumps({"is_available": True, "balance_infos": [
            {"currency": "USD", "total_balance": "12.30", "granted_balance": "0.00", "topped_up_balance": "12.30"}]}))
        doctor_ok = run([EGM, "doctor"])
    finally:
        server.terminate()

    checks = {
        "doctor: an empty balance is shown as not enough, with where to top up": "DeepSeek balance: 0.00 USD — not enough" in doctor_empty[1]
            and "platform.deepseek.com/top_up" in doctor_empty[1],
        "doctor: a funded balance is shown": "DeepSeek balance: 12.30 USD" in doctor_ok[1] and "not enough" not in doctor_ok[1],
        "goldfish: the refusal is named in plain words, the round is incomplete": "no credit left" in gold[1] and "INCOMPLETE" in gold[1],
        "smoke run: names the refusal as the failure": any("provider refused" in f for f in smoke_sum.get("failures", []))
            and "usage was not recorded" not in smoke_sum.get("failures", []),
    }
    if HAVE_UPSTREAM:
        checks["head-to-head: stops starting docs after the refusal, and says so"] = bool(summary.get("not_run (provider refused)")) \
            and "STOPPED: the provider refused" in cmp[1]
        checks["head-to-head: the stop is quick, not minutes of refused docs"] = cmp_secs < 300
    if NOTEBOOK_CELL:
        checks["notebook preflight: reads the balance and would skip step 7"] = pre == (False, "0.00 USD")
    print()
    for name, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + name)
    print("SUMMARY " + json.dumps({"passed": sum(map(bool, checks.values())), "failed": sum(not v for v in checks.values())}))
    if not all(checks.values()):
        sys.exit(1)
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
