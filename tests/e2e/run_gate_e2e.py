"""End-to-end test of the evidence gate and the evidence bundle, on the real DeepSeek
Harness binary (model faked by fake_deepseek.py) and real git.

The change: the fix lane's one-line id fix (`max(ids) + 1` instead of `len + 1`),
built by egm-dsh with your failing test, committed on its own branch.

  A. good evidence: `gate` passes (design unchanged, no drift, the test untouched and
     committed before the code, the check passes, seeded bugs caught); `mutate` names
     the bug the test misses (an empty notes list); `evidence` writes the pull request
     description with coverage of the changed line; `risk` gives a tier and a reviewer;
     `log-time` adds human minutes; `ci-template` prints a workflow
  B. seeded attacks, each must fail the gate:
     1. the frozen test is edited after the build
     2. the code drifts after the build
     3. a suite too weak to pin the fix down: the new test only checks the id is not 3,
        and the project has no other id test. It goes red before the build and green
        after, so the red/green rule accepts it, but no seeded bug makes any test fail:
        mutation score 0%
     4. a suite slower than the seeded-bug timeout: the acceptance command sleeps 3 s
        and each seeded bug may run 2 s. Every seeded bug times out; a timeout used to
        count as caught, so the gate passed at 100% with nothing judged. Now the gate
        times the unmutated checks first and fails, naming the fix; with the timeout
        raised it passes again, every catch a real test failure

usage: python3 run_gate_e2e.py EGM_BINARY DSH_COMMAND...
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import run_fix_e2e as fx  # noqa: E402  (same scenario, same scripted model)

EGM, DSH = fx.EGM, fx.DSH
WEAK_TEST = '''

class GapTest(unittest.TestCase):
    def test_add_after_gap(self):
        notes = [{"id": 1, "text": "a"}, {"id": 3, "text": "c"}]
        self.assertNotEqual(cli.add(notes, "d")["id"], 3)
'''
COVERAGE = "python3 -m coverage run -m unittest discover -s tests -t . -q && python3 -m coverage lcov -q -o lcov.info"


def project(name, test):
    gold = [{"match": fx.GOLD, "keep": True, "text": json.dumps(fx.CLEAN_ALL)}]
    saved = fx.GAP_TEST
    fx.GAP_TEST = test
    try:
        p = fx.Project(name, gold + fx.builder(fx.FIXED) + [{"match": fx.DIFF, "text": fx.MINOR}], with_test=True)
    finally:
        fx.GAP_TEST = saved
    return p


def main():
    have_cov = subprocess.run([sys.executable, "-m", "coverage", "--version"], capture_output=True).returncode == 0
    a = project("gate-good", fx.GAP_TEST)
    w = project("gate-weak", WEAK_TEST)
    # the weak project has no other test of ids: drop the sample's one and commit
    t = w.proj / "tests" / "test_jot.py"
    src = t.read_text()
    start = src.index("    def test_add_assigns_increasing_ids")
    end = src.index("    def test_save_and_load_round_trip")
    t.write_text(src[:start] + src[end:])
    w.git("commit", "-qam", "no id test")
    out = {}
    try:
        out["fix"] = a.egm("fix", "gap-ids", fx.PROBLEM, "--files", "jot/cli.py", "--check", fx.CHECK)
        if have_cov:
            a.egm("set-gate", "--coverage-cmd", COVERAGE)
        out["gate"] = a.egm("gate", "gap-ids")
        out["mutate"] = a.egm("mutate", "gap-ids")
        time.sleep(1.2)
        out["log"] = a.egm("log-time", "gap-ids", "12", "--what", "read the pull request")
        pr = a.work / "PR.md"
        out["evidence"] = a.egm("evidence", "gap-ids", "--out", str(pr))
        bundle = pr.read_text() if pr.exists() else ""
        if os.environ.get("EGM_BUNDLE_OUT") and bundle:   # keep an example for the docs
            Path(os.environ["EGM_BUNDLE_OUT"]).write_text(bundle)
        out["risk"] = a.egm("risk", "gap-ids")
        out["ci"] = a.egm("ci-template")
        rec = json.loads((a.proj / ".egm-dsh" / "evidence" / "gap-ids.json").read_text())

        # B1: edit the frozen test after the build
        test = a.proj / "tests" / "test_jot.py"
        original = test.read_text()
        test.write_text(original.replace('["id"], 4)', '["id"] > 0, True)'))
        out["b1"] = a.egm("gate", "gap-ids", "--no-mutation")
        test.write_text(original)
        # B2: the code drifts after the build
        cli = a.proj / "jot" / "cli.py"
        code = cli.read_text()
        cli.write_text(code + "\n# a quick hack after the build\n")
        out["b2"] = a.egm("gate", "gap-ids", "--no-mutation")
        cli.write_text(code)
        out["back"] = a.egm("gate", "gap-ids", "--no-mutation")
        # B3: a weak test
        out["weak-fix"] = w.egm("fix", "gap-ids", fx.PROBLEM, "--files", "jot/cli.py", "--check", fx.CHECK)
        out["b3"] = w.egm("gate", "gap-ids")
        weak = json.loads((w.proj / ".egm-dsh" / "evidence" / "gap-ids.json").read_text())
        # B4: a suite slower than the seeded-bug timeout
        cfg_path = a.proj / ".egm-dsh" / "config.json"
        cfg_text = cfg_path.read_text()
        cfg = json.loads(cfg_text)
        cfg["acceptance_cmd"] = ["sh", "-c", "sleep 3 && " + " ".join(cfg["acceptance_cmd"])]
        cfg_path.write_text(json.dumps(cfg, indent=2))
        out["b4-set"] = a.egm("set-gate", "--mutant-timeout", "2")
        out["b4"] = a.egm("gate", "gap-ids")
        slow = json.loads((a.proj / ".egm-dsh" / "evidence" / "gap-ids.json").read_text())
        out["b4-raise"] = a.egm("set-gate", "--mutant-timeout", "30")
        out["b4-ok"] = a.egm("gate", "gap-ids")
        raised = json.loads((a.proj / ".egm-dsh" / "evidence" / "gap-ids.json").read_text())
        cfg_path.write_text(cfg_text)
    finally:
        a.close()
        w.close()

    def failed(key, name):
        rc, text, _ = out[key]
        return rc != 0 and any(name in l and "✖" in l for l in text.splitlines())

    m = rec.get("mutation") or {}
    survivors = [x["op"] for x in m.get("survivors", [])]
    checks = {
        "A: the fix is built (hardened)": a.feature("gap-ids")["gate"]["phase"] == "hardened",
        "A: the gate passes, exit 0": out["gate"][0] == 0 and "evidence gate passed" in out["gate"][1],
        "A: every check is there": {c["name"] for c in rec["checks"]} >= {"built", "design unchanged since the build", "no drift",
            "frozen tests and Lean statements untouched", "criteria committed before the code (git)", "every check passes now",
            "acceptance passes", "seeded bugs caught (mutation score)"},
        "A: the changed lines come from the design and build commits": rec["changed_from_git"] and list(rec["changed"]) == ["jot/cli.py"],
        "A: seeded bugs in the changed line: the suite catches every one": m.get("tried", 0) >= 3 and m["killed"] == m["tried"] and not survivors,
        "A: mutate lists every seeded bug and exits 0": out["mutate"][0] == 0 and "seeded bug 1/" in out["mutate"][1] and "caught" in out["mutate"][1],
        "A: the bundle has the gate, criteria, changed lines, seeded bugs, risk, reviewer, checklist and marker": all(s in bundle for s in (
            "### Gate: passed", "### Acceptance criteria", fx.CHECK, "### What changed", "### Seeded bugs", "made a test fail",
            "### Risk:", "Suggested reviewer:", "Is the evidence adequate", "<!-- egm-gate: pass -->")),
        "A: coverage of the changed line is measured (when coverage.py is installed)": (not have_cov) or (
            rec.get("coverage") is not None and rec["coverage"]["executable"] >= 1 and "Coverage of changed lines: " in bundle and "not measured" not in bundle),
        "A: risk is low (small, covered, every seeded bug caught): anyone may review, and the fix lane is suggested": rec["tier"] == "low"
            and "risk low" in out["risk"][1] and "new to this code" in out["risk"][1],
        "A: human minutes: logged 12, plus gaps between commands": rec["logged_minutes"] == 12 and rec["human_minutes"] >= 0
            and "logged 12 min" in out["log"][1],
        "A: ci-template prints a workflow that runs the gate": out["ci"][0] == 0 and "egm-dsh gate --all" in out["ci"][1] and "fetch-depth: 0" in out["ci"][1],
        "B1: an edited frozen test fails the gate": failed("b1", "frozen tests and Lean statements untouched"),
        "B2: drift after the build fails the gate": failed("b2", "no drift"),
        "B1/B2 restored: the gate passes again": out["back"][0] == 0,
        "B3: the weak test was accepted by red/green (built)": w.feature("gap-ids")["gate"]["phase"] in ("built", "hardened"),
        "B3: but no seeded bug fails it: mutation score 0%, gate fails": failed("b3", "seeded bugs caught")
            and (weak.get("mutation") or {}).get("killed", 1) == 0 and (weak.get("mutation") or {}).get("tried", 0) >= 2,
        "B3: the failed gate makes the risk high, and the survivors are listed": weak["tier"] == "high"
            and len((weak.get("mutation") or {}).get("survivors", [])) >= 2 and "survived:" in out["b3"][1],
        "B4: checks slower than the seeded-bug timeout fail the gate and name the fix": failed("b4", "seeded bugs caught")
            and "--mutant-timeout" in out["b4"][1] and (slow.get("mutation") or {}).get("problem") is not None
            and (slow.get("mutation") or {}).get("killed", 1) == 0,
        "B4: with the timeout raised the gate passes, every catch a real failure": out["b4-ok"][0] == 0
            and (raised.get("mutation") or {}).get("timed_out", 1) == 0
            and (raised.get("mutation") or {}).get("killed", 0) == (raised.get("mutation") or {}).get("tried", -1) >= 3
            and (raised.get("mutation") or {}).get("baseline_secs", 0) >= 3,
    }
    for name, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + name)
    print("SUMMARY " + json.dumps({"passed": sum(map(bool, checks.values())), "failed": sum(not v for v in checks.values()),
                                   "mutation": {"good": [m.get("killed"), m.get("tried")], "weak": [(weak.get("mutation") or {}).get("killed"), (weak.get("mutation") or {}).get("tried")]},
                                   "coverage_measured": have_cov}))
    if not all(checks.values()):
        sys.exit(1)
    for p in (a, w):
        shutil.rmtree(p.work, ignore_errors=True)


if __name__ == "__main__":
    main()
