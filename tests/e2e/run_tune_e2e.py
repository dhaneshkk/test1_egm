"""End-to-end test of Harness² tuning on egm-dsh, offline (scripted reviewer + scripted improver).

  * Harness²'s unmodified engine runs on the training docs through the dsh adapter
  * the chosen setup beats the base on held-out docs, and is adopted only with approval
  * after adoption, `egm-dsh goldfish` in the project uses the setup (and catches a defect
    the base misses); removing the config line rolls it back
  * a setup with a file outside the prompt surface is refused by egm-dsh
  * the adoption rule refuses without approval and without a held-out gain

usage: python3 run_tune_e2e.py EGM_BINARY      (needs `harness2` importable)
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
TUNE = CRATE / "bench" / "tune"
EGM = str(Path(sys.argv[1]).resolve())
sys.path.insert(0, str(TUNE))
from tune import adopt_ok  # noqa: E402


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
    work = Path(tempfile.mkdtemp(prefix="egm-dsh-tune-"))
    proj = work / "jot"
    shutil.copytree(CRATE / "sample" / "before", proj, ignore=shutil.ignore_patterns("__pycache__"))
    (proj / "docs" / "designs").mkdir(parents=True)
    port = free_port()
    server = subprocess.Popen([sys.executable, "-I", str(TUNE / "persona_reviewer.py"), str(port), str(work / "reviewer.jsonl")])
    wait_port(port)
    env = dict(os.environ, NO_COLOR="1")
    env.pop("EGM_DSH_GOLDFISH_HARNESS", None)

    def egm(*args, extra=None):
        p = subprocess.run([EGM, *args], cwd=proj, env={**env, **(extra or {})}, text=True, capture_output=True)
        print(f"$ egm-dsh {' '.join(args)}\n{(p.stdout + p.stderr).rstrip()}\n", flush=True)
        return p.returncode, p.stdout + p.stderr

    def review(slug):
        egm("goldfish", slug, "--no-verify")
        runs = sorted((proj / ".egm-dsh" / "reviews").glob(f"{slug}-*.json"), key=lambda p: int(p.stem.rsplit("-", 1)[1]))
        return json.loads(runs[-1].read_text())

    try:
        egm("init", "--agent", "openai", "--preset", "balanced")
        cfg_path = proj / ".egm-dsh" / "config.json"
        cfg = json.loads(cfg_path.read_text())
        cfg["openai"] = {"base_url": f"http://127.0.0.1:{port}", "model": "deepseek-flash", "api_key_env": ""}
        cfg["roles"] = {}
        cfg_path.write_text(json.dumps(cfg, indent=1))
        shutil.copy2(CRATE / "bench" / "corpus" / "priority-no-old-data.md", proj / "docs" / "designs" / "priority.md")
        egm("adopt", "priority", "--doc", "docs/designs/priority.md")
        before = review("priority")

        t = subprocess.run([sys.executable, str(TUNE / "tune.py"), "--egm-dsh", EGM, "--out", str(work / "tune"),
                            "--approve", "--adopt-into", str(proj)], text=True, capture_output=True, env=env, timeout=3000)
        print(t.stdout[-3000:], t.stderr[-2000:])
        summary = json.loads(next(l for l in t.stdout.splitlines() if l.startswith("SUMMARY "))[8:])
        cfg = json.loads(cfg_path.read_text())
        setup_dir = proj / cfg.get("goldfish_harness", "missing")
        setup_files = sorted(p.relative_to(setup_dir).as_posix() for p in setup_dir.rglob("*") if p.is_file()) if setup_dir.is_dir() else []

        # the project now reviews with the adopted setup: a fresh doc revision, so a new review
        (proj / "docs" / "designs" / "priority.md").write_text((proj / "docs" / "designs" / "priority.md").read_text() + "\n")
        after = review("priority")
        # rollback: remove the config line
        cfg.pop("goldfish_harness", None)
        cfg_path.write_text(json.dumps(cfg, indent=1))
        (proj / "docs" / "designs" / "priority.md").write_text((proj / "docs" / "designs" / "priority.md").read_text() + "\n")
        rolled_back = review("priority")

        bad = work / "bad-setup"
        (bad / "scripts").mkdir(parents=True)
        (bad / "guardrails.md").write_text("Be thorough.\n")
        (bad / "scripts" / "hack.py").write_text("import os; os.system('curl evil')\n")
        refused = egm("status", extra={"EGM_DSH_GOLDFISH_HARNESS": str(bad)})
    finally:
        server.terminate()

    base = {"mcc": 0.447, "input_tokens": 1000}
    checks = {
        "Harness² engine ran every training task": summary["harness2_ok"] and summary["executions"] > 0,
        "a tuned setup was chosen on training docs": summary["chosen"] != "base",
        "held-out MCC improved over the base setup": summary["held_tuned"] is not None and summary["held_base"] is not None
                                                     and summary["held_tuned"] > summary["held_base"],
        "adopted only because --approve was given": summary["adopt"] and summary["adopted"],
        "adopted setup holds only prompt-surface files": bool(setup_files)
                                                         and all(f in {"guardrails.md", "systemprompt.md", "LongTermMEMORY.md", "tool_descriptions/tool_guidance.md"}
                                                                 or f.startswith("sub_agents/") for f in setup_files),
        "before adoption the base setup missed the defect": before["verdict"] == "ready" and not before.get("setup"),
        "after adoption egm-dsh used the setup and caught it": after["verdict"] == "not-ready" and bool(after.get("setup")),
        "removing the config line rolls back": rolled_back["verdict"] == "ready" and not rolled_back.get("setup"),
        "a setup with a script is refused by egm-dsh": refused[0] != 0 and "outside the tunable surface" in refused[1],
        "no adoption without approval": not adopt_ok(base, {"mcc": 1.0, "input_tokens": 1000}, True, False)[0],
        "no adoption without a held-out gain": not adopt_ok(base, {"mcc": 0.447, "input_tokens": 1000}, True, True)[0],
        "no adoption if prompts cost > 1.25x": not adopt_ok(base, {"mcc": 1.0, "input_tokens": 1300}, True, True)[0],
    }
    for name, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + name)
    print("SUMMARY " + json.dumps({"passed": sum(checks.values()), "failed": sum(not v for v in checks.values())}))
    if not all(checks.values()):
        sys.exit(1)
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
