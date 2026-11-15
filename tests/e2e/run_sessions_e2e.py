"""End-to-end test of run budgets and guaranteed session cleanup, on the real
DeepSeek Harness binary with real processes and real signals (model faked by
fake_deepseek.py, which can stall to keep dsh sessions open).

  * a goldfish round over budget is refused with a message naming the limit; set-budget raises it
  * model calls running out mid-round: reviewers fail, the round is incomplete, nothing stays open
  * Ctrl-C (SIGINT) while dsh sessions run: egm exits 130, every dsh process group is gone, registry empty
  * SIGTERM: same
  * SIGKILL of egm itself: the kernel kills its dsh children (PR_SET_PDEATHSIG); the next egm-dsh
    command clears the leftover registry and reports it

usage: python3 run_sessions_e2e.py EGM_BINARY DSH_COMMAND...
"""
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
SAMPLE = HERE.parent.parent / "sample"
EGM, DSH = sys.argv[1], sys.argv[2:]

DOC = """# Design: Tags

## Problem

People cannot list only work notes.

## Goals and non-goals

Goals: tag notes. Non-goals: renaming.

## Technical plan

Notes gain `tags`; old notes read as empty.

## Alternatives considered

**None.** Rejected.

## Risks and mitigations

**Small.** Tests.

## Implementation map

- `jot/cli.py` — tags

## Acceptance criteria

- Tags filter. Check: `tests/test_tags.py`
"""
CLEAN = {"comprehension": {"problem": "p", "current_mechanism": "m", "proposed_change": "c", "unclear": []},
         "critic": {"findings": []}, "zoom_out": {"findings": []},
         "readiness": {"ready": True, "blocking_questions": []},
         "invariants": {"criteria": [{"criterion": "c", "predicate": "p", "check": "tests"}]}}
G = "Complete five separate tasks"
rules = [
    {"match": G, "text": json.dumps(CLEAN)},                 # round 1 (economy: one combined session)
    # every later round stalls (the rule is kept, so a dsh retry cannot use it up)
    {"match": G, "delay": 60, "keep": True, "text": json.dumps(CLEAN)},
]


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


def group_alive(pgid):
    """A running (non-zombie) process in the group. A zombie has exited and runs no code;
    whether it is reaped depends on the container's init, not on egm-dsh."""
    for d in Path("/proc").iterdir():
        if not d.name.isdigit():
            continue
        try:
            stat = (d / "stat").read_text()
        except OSError:
            continue
        f = stat[stat.rfind(")") + 2:].split()
        if int(f[2]) == pgid and f[0] not in ("Z", "X"):
            return True
    return False


def main():
    work = Path(tempfile.mkdtemp(prefix="egm-dsh-sessions-"))
    proj = work / "jot"
    shutil.copytree(SAMPLE / "before", proj, ignore=shutil.ignore_patterns("__pycache__"))
    (proj / "docs" / "designs").mkdir(parents=True)
    (proj / "docs" / "designs" / "tags.md").write_text(DOC)
    (work / "rules.json").write_text(json.dumps(rules))
    port = free_port()
    server = subprocess.Popen([sys.executable, "-I", str(HERE / "fake_deepseek.py"), str(port), str(work / "rules.json"), str(work)])
    wait_port(port)
    env = dict(os.environ, DEEPSEEK_API_KEY="fake", DEEPSEEK_BASE_URL=f"http://127.0.0.1:{port}", NO_COLOR="1")
    reg = proj / ".egm-dsh" / "sessions.json"

    def egm(*args):
        p = subprocess.run([EGM, *args], cwd=proj, env=env, text=True, capture_output=True)
        out = (p.stdout + p.stderr).rstrip()
        print(f"$ egm-dsh {' '.join(args)}\n{out}\n", flush=True)
        return p.returncode, out

    def interrupted(sig):
        """Start a goldfish round that stalls, wait until dsh is running, send `sig` to egm."""
        p = subprocess.Popen([EGM, "goldfish", "tags", "--no-verify"], cwd=proj, env=env, text=True,
                             stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        pids = []
        for _ in range(300):
            try:
                pids = [e["pid"] for e in json.loads(reg.read_text()) if e.get("pid")]
            except (OSError, ValueError):
                pids = []
            if pids:
                break
            time.sleep(0.05)
        time.sleep(1.0)   # let dsh get into its model request
        alive_before = [pid for pid in pids if group_alive(pid)]
        p.send_signal(sig)
        try:
            out = p.communicate(timeout=20)[0]
        except subprocess.TimeoutExpired:
            p.kill()
            out = p.communicate()[0]
        time.sleep(0.5)
        left = [pid for pid in pids if group_alive(pid)]
        entries = json.loads(reg.read_text()) if reg.exists() else []
        print(f"--- {sig.name}: egm exit {p.returncode}; dsh groups before {alive_before}, after {left}; registry {len(entries)}\n{out[-600:]}\n", flush=True)
        return {"exit": p.returncode, "pids": pids, "alive_before": alive_before, "left": left, "registry": entries, "out": out}

    try:
        egm("init", "--agent", "dsh", "--preset", "economy", "--dsh-command", *DSH)
        egm("set-reviewers", "dsh")   # this suite tests dsh reading rooms and processes; run_reviewers_e2e.py covers the built-in client
        egm("adopt", "tags", "--doc", "docs/designs/tags.md")
        egm("set-budget", "--goldfish-rounds", "1")
        first = egm("goldfish", "tags", "--no-verify")
        over = egm("goldfish", "tags", "--no-verify")
        shown = egm("budget")
        egm("set-budget", "--feature", "tags", "--goldfish-rounds", "10", "--model-calls", "4")
        sigint = interrupted(signal.SIGINT)
        sigterm = interrupted(signal.SIGTERM)
        sigkill = interrupted(signal.SIGKILL)
        leftover_registry = json.loads(reg.read_text()) if reg.exists() else []
        after_crash = egm("status")
        registry_after = json.loads(reg.read_text()) if reg.exists() else []
        stages_left = sorted(Path(tempfile.gettempdir()).glob("egm-dsh-stage-*"))
        calls_out = egm("goldfish", "tags", "--no-verify")   # 4 calls allowed: round 1 + the 3 interrupted runs used them all
        final_budget = egm("budget", "tags")
        doctor = egm("doctor")
    finally:
        server.terminate()

    checks = {
        "round 1 ran": first[0] == 0 and "READY" in first[1],
        "round 2 over budget is refused, naming the limit and the fix": over[0] != 0 and "used 1 of 1 goldfish rounds" in over[1]
            and "egm-dsh set-budget --goldfish-rounds" in over[1],
        "`budget` shows what was used": "1/1" in shown[1],
        "Ctrl-C: dsh was really running when interrupted": bool(sigint["alive_before"]),
        "Ctrl-C: egm exits 130 and reports the closed sessions": sigint["exit"] == 130 and "closed" in sigint["out"],
        "Ctrl-C: no dsh process group survives": sigint["left"] == [],
        "Ctrl-C: registry empty": sigint["registry"] == [],
        "SIGTERM: egm exits 130, no dsh process group survives, registry empty": sigterm["exit"] == 130 and sigterm["left"] == [] and sigterm["registry"] == []
            and bool(sigterm["alive_before"]),
        "SIGKILL of egm: dsh children die with it (PDEATHSIG)": sigkill["exit"] == -9 and sigkill["left"] == [] and bool(sigkill["alive_before"]),
        "SIGKILL of egm: the registry survives the crash": len(leftover_registry) >= 1,
        "next command cleans up the crashed run's registry and says so": "cleaned up" in after_crash[1] and registry_after == [],
        "model calls running out: refused with the limit named, round incomplete": "model calls" in calls_out[1] and "INCOMPLETE" in calls_out[1],
        "nothing left open after the refused round": (json.loads(reg.read_text()) if reg.exists() else []) == [],
        "no goldfish reading-room copy survives Ctrl-C, SIGTERM or SIGKILL": stages_left == [],
        "doctor reports no open sessions": "agent sessions open: 0" in doctor[1],
        "final budget table": "tags" in final_budget[1],
    }
    print()
    for name, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + name)
    print("SUMMARY " + json.dumps({"passed": sum(checks.values()), "failed": sum(not v for v in checks.values())}))
    if not all(checks.values()):
        sys.exit(1)
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
