"""End-to-end test of the fix lane (`egm-dsh fix`) on the real DeepSeek Harness binary
(model faked by fake_deepseek.py; every request and its reported usage recorded).

The bug: `jot add` numbers a note len(notes)+1, so after a gap in the ids (a note
removed by hand) two notes share an id. The fix is one line.

  1. your test: you wrote the failing test; `fix` runs one goldfish, checks red,
     one builder, green with the whole suite, one diff review -> hardened, 3 sessions
  2. no test yet: one tester writes it and one reviewer judges it first -> 5 sessions
  3. too big: the builder changes 60+ lines -> reverted, promoted to the full lane
  4. refused before any request: a protected path, a manifest, too many files
  5. the full lane on the same bug with the same tester script as case 2 (adopt,
     5-stage goldfish, tests, build, mean review); no elephant turns counted
  6. --manual: no test can show the fix; still goldfish, builder, suite, diff review

usage: python3 run_fix_e2e.py EGM_BINARY DSH_COMMAND...
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
SAMPLE = HERE.parent.parent / "sample"
EGM, DSH = str(Path(sys.argv[1]).resolve()), sys.argv[2:]

ORIGINAL = (SAMPLE / "before" / "jot" / "cli.py").read_text()
BUGGY = 'note = {"id": len(notes) + 1, "text": text}'
FIXED = ORIGINAL.replace(BUGGY, 'note = {"id": max((n["id"] for n in notes), default=0) + 1, "text": text}')
assert FIXED != ORIGINAL
TOO_BIG = FIXED + "\n" + "\n".join(f"# reformatted line {i}" for i in range(60)) + "\n"
GAP_TEST = '''

class GapTest(unittest.TestCase):
    def test_add_after_gap(self):
        notes = [{"id": 1, "text": "a"}, {"id": 3, "text": "c"}]
        self.assertEqual(cli.add(notes, "d")["id"], 4)
'''
CHECK = "tests/test_jot.py::GapTest::test_add_after_gap"
PROBLEM = "After a note is removed by hand from notes.json, `jot add` can reuse an id that is still taken, so two notes share it."

GOLD = "Complete five separate tasks"
BUILD = "Implement the feature described in the design document"
TESTER = "Write the acceptance tests"
TJUDGE = "Judge whether each acceptance test"
DIFF = "Assume this diff"
MEAN = "Assume the code written for the design document"
STAGES = {"In your own words, state the problem": {"problem": "p", "current_mechanism": "m", "proposed_change": "c", "unclear": []},
          "Act as an expert reviewer": {"findings": []}, "Zoom out and judge": {"findings": []},
          "You are the engineer who must implement": {"ready": True, "blocking_questions": []},
          "For each item under 'Acceptance criteria'": {"criteria": [{"criterion": "c", "predicate": "p", "check": CHECK}]}}
CLEAN_ALL = {"comprehension": STAGES["In your own words, state the problem"], "critic": {"findings": []}, "zoom_out": {"findings": []},
             "readiness": {"ready": True, "blocking_questions": []}, "invariants": {"criteria": [{"criterion": "c", "predicate": "p", "check": CHECK}]}}
NO_DECISIONS = "Fixed the id.\n\n" + json.dumps({"assumptions": []})
MINOR = json.dumps({"findings": [{"severity": "minor", "file": "jot/cli.py", "issue": "a comment would help"}]})


def wait_port(port, timeout=60):
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


def builder(content):
    return [{"match": BUILD, "tool": "read", "args": {"file_path": "jot/cli.py"}},
            {"match": BUILD, "tool": "write", "args": {"file_path": "jot/cli.py", "content": content}},
            {"match": BUILD, "text": NO_DECISIONS}]


class Project:
    def __init__(self, name, rules, with_test):
        self.secs = 0.0
        self.work = Path(tempfile.mkdtemp(prefix=f"egm-dsh-fix-{name}-"))
        self.proj = self.work / "jot"
        shutil.copytree(SAMPLE / "before", self.proj, ignore=shutil.ignore_patterns("__pycache__"))
        if with_test:   # a person wrote the failing test and committed it
            with (self.proj / "tests" / "test_jot.py").open("a") as f:
                f.write(GAP_TEST)
        for c in (["init", "-q", "-b", "main"], ["add", "-A"], ["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "start"]):
            subprocess.run(["git", *c], cwd=self.proj, check=True)
        subprocess.run(["git", "config", "user.email", "t@t"], cwd=self.proj, check=True)
        subprocess.run(["git", "config", "user.name", "t"], cwd=self.proj, check=True)
        (self.work / "rules.json").write_text(json.dumps(rules))
        (self.work / "DUMP").write_text("")
        port = free_port()
        self.server = subprocess.Popen([sys.executable, "-I", str(HERE / "fake_deepseek.py"), str(port), str(self.work / "rules.json"), str(self.work)])
        wait_port(port)
        self.env = dict(os.environ, DEEPSEEK_API_KEY="fake", DEEPSEEK_BASE_URL=f"http://127.0.0.1:{port}", NO_COLOR="1")
        self.egm("init", "--agent", "dsh", "--preset", "economy", "--dsh-command", *DSH,
                 "--acceptance", "python3", "-m", "unittest", "discover", "-s", "tests", "-t", ".", "-q")
        self.egm("set-spec", "--check-cmd", "python3", "-m", "unittest", "-q", "{unittest}")

    def egm(self, *args):
        r0, t0 = self.requests(), time.time()
        p = subprocess.run([EGM, *args], cwd=self.proj, env=self.env, text=True, capture_output=True, timeout=900)
        out = (p.stdout + p.stderr).rstrip()
        print(f"$ egm-dsh {' '.join(args)}\n{out[-3000:]}\n", flush=True)
        if args and args[0] in ("fix", "adopt", "goldfish", "tests", "build", "review"):
            self.secs += time.time() - t0
        return p.returncode, out, self.requests() - r0

    def requests(self):
        f = self.work / "requests.jsonl"
        return len(f.read_text().splitlines()) if f.exists() else 0

    def feature(self, slug):
        return json.loads((self.proj / ".egm-dsh" / "features" / f"{slug}.json").read_text())

    def usage(self, slug):
        f = self.proj / ".egm-dsh" / "usage.jsonl"
        rows = [json.loads(l) for l in f.read_text().splitlines()] if f.exists() else []
        rows = [r for r in rows if r["feature"] == slug]
        return len(rows), sum(r["cache_hit"] + r["cache_miss"] + r["output"] for r in rows), sorted({r["role"] for r in rows})

    def ledger(self, kind):
        f = self.proj / ".egm-dsh" / "ledger.jsonl"
        return [json.loads(l) for l in f.read_text().splitlines() if f'"{kind}"' in l] if f.exists() else []

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.proj, text=True, capture_output=True).stdout

    def close(self):
        self.server.terminate()


def main():
    results, rows, keep = {}, {}, []
    gold = [{"match": GOLD, "keep": True, "text": json.dumps(CLEAN_ALL)}]

    # 1. your test
    p1 = Project("own-test", gold + builder(FIXED) + [{"match": DIFF, "text": MINOR}], with_test=True)
    keep.append(p1)
    r1 = p1.egm("fix", "gap-ids", PROBLEM, "--files", "jot/cli.py", "--check", CHECK)
    f1, (s1, t1, roles1) = p1.feature("gap-ids"), p1.usage("gap-ids")
    rows["fix lane, your test"] = (s1, r1[2], t1, round(p1.secs))
    diff_bodies = [l for l in (p1.work / "bodies.jsonl").read_text().splitlines() if DIFF in l]

    # 2. no test yet: the tester writes it, a reviewer judges it
    tester = [{"match": TESTER, "tool": "read", "args": {"file_path": "tests/test_jot.py"}},
              {"match": TESTER, "tool": "write", "args": {"file_path": "tests/test_jot.py",
                                                          "content": (SAMPLE / "before" / "tests" / "test_jot.py").read_text() + GAP_TEST}},
              {"match": TESTER, "text": "Wrote GapTest."},
              {"match": TJUDGE, "text": json.dumps({"criteria": [{"check": CHECK, "tests_it": True, "reason": "fails on a reused id"}]})}]
    p2 = Project("tester", gold + tester + builder(FIXED) + [{"match": DIFF, "text": MINOR}], with_test=False)
    keep.append(p2)
    r2 = p2.egm("fix", "gap-ids", PROBLEM, "--files", "jot/cli.py", "--check", CHECK)
    f2, (s2, t2, roles2) = p2.feature("gap-ids"), p2.usage("gap-ids")
    rows["fix lane, test written for you"] = (s2, r2[2], t2, round(p2.secs))

    # 3. too big: reverted and promoted
    p3 = Project("too-big", gold + builder(TOO_BIG), with_test=True)
    keep.append(p3)
    r3 = p3.egm("fix", "gap-ids", PROBLEM, "--files", "jot/cli.py", "--check", CHECK)
    f3 = p3.feature("gap-ids")
    cli3 = (p3.proj / "jot" / "cli.py").read_text()
    again3 = p3.egm("fix", "gap-ids")

    # 4. refused before any request
    p4 = Project("refused", [], with_test=True)
    keep.append(p4)
    p4.egm("set-fix-limits", "--protect", "jot/")
    r4a = p4.egm("fix", "a", PROBLEM, "--files", "jot/cli.py", "--check", CHECK)
    r4b = p4.egm("fix", "b", PROBLEM, "--files", "pyproject.toml", "--check", CHECK)
    r4c = p4.egm("fix", "c", PROBLEM, "--files", "x.py,y.py,z.py", "--check", CHECK)
    refused_requests = r4a[2] + r4b[2] + r4c[2]

    # 6. no test can show it: --manual, still one goldfish, one builder, the suite, the diff review
    p6 = Project("manual", gold + builder(FIXED) + [{"match": DIFF, "text": MINOR}], with_test=False)
    keep.append(p6)
    r6 = p6.egm("fix", "gap-ids", PROBLEM, "--files", "jot/cli.py", "--manual", "add notes 1-3, delete #2 from notes.json, add one more: it gets #4")
    f6, (s6, _, _) = p6.feature("gap-ids"), p6.usage("gap-ids")
    doc6 = (p6.proj / "docs" / "fixes" / "gap-ids.md").read_text()

    # 5. the full lane, same bug, same doc: adopt, 5-stage goldfish, tests, build, mean review
    full_rules = [{"match": m, "keep": True, "text": json.dumps(a)} for m, a in STAGES.items()]
    full_rules += tester + builder(FIXED) + [{"match": MEAN, "text": MINOR}]
    p5 = Project("full", full_rules, with_test=False)   # same script as case 2: the tester writes the test
    keep.append(p5)
    cfg = json.loads((p5.proj / ".egm-dsh" / "config.json").read_text())
    cfg["goldfish_mode"] = "parallel"
    (p5.proj / ".egm-dsh" / "config.json").write_text(json.dumps(cfg, indent=1))
    from_fix = (p1.proj / "docs" / "fixes" / "gap-ids.md").read_text()
    (p5.proj / "docs" / "designs").mkdir(parents=True, exist_ok=True)
    (p5.proj / "docs" / "designs" / "gap-ids.md").write_text(from_fix)
    full_req = 0
    for cmd in (["adopt", "gap-ids", "--doc", "docs/designs/gap-ids.md"], ["goldfish", "gap-ids", "--no-verify"], ["tests", "gap-ids"],
                ["build", "gap-ids"], ["review", "gap-ids"]):
        full_req += p5.egm(*cmd)[2]
    f5, (s5, t5, _) = p5.feature("gap-ids"), p5.usage("gap-ids")
    rows["full lane (no elephant turns)"] = (s5, full_req, t5, round(p5.secs))
    for p in keep:
        p.close()

    checks = {
        "your test: hardened in one command": r1[0] == 0 and f1["gate"]["phase"] == "hardened" and "is hardened" in r1[1],
        "your test: red first, then green with the whole suite": "✗ red" in r1[1] and "green:" in r1[1] and "acceptance:" in r1[1],
        "your test: 3 sessions (goldfish, builder, diff reviewer), no tester": s1 == 3 and roles1 == ["builder", "goldfish", "reviewer"],
        "your test: one combined goldfish session, no verifier": r1[1].count("1 fresh reviewer") == 1 and "goldfish-verifier" not in roles1,
        "your test: the reviewer saw the diff, not whole files": bool(diff_bodies) and all("+++ b/jot/cli.py" in b and "def main(" not in b for b in diff_bodies),
        "your test: committed on its own branch, the bug fixed": "egm-dsh/gap-ids" in p1.git("branch") and "max((n[" in p1.git("show", "egm-dsh/gap-ids:jot/cli.py"),
        "no test yet: tester writes it, a reviewer judges it, then the fix -> hardened in 5 sessions": r2[0] == 0
            and f2["gate"]["phase"] == "hardened" and s2 == 5 and "tester" in roles2,
        "too big: reverted, promoted to the full lane, design reopened": r3[0] != 0 and "not a tiny fix" in r3[1]
            and f3["lane"] is None and f3["gate"]["phase"] == "designing" and cli3 == ORIGINAL and p3.ledger("fix-promoted"),
        "too big: the line count was egm-dsh's (61+ lines over 40)": "fix size:" in r3[1] and "changed lines (the fix lane allows 40)" in r3[1],
        "too big: `fix` will not continue it; the full lane does": again3[0] != 0 and "full design lane" in again3[1],
        "refused before any request: protected path, manifest, three files": refused_requests == 0
            and "protected" in r4a[1] and "manifest" in r4b[1] and "3 files" in r4c[1] and not (p4.proj / "docs" / "fixes").exists(),
        "--manual: hardened in 3 sessions, the doc says how to verify, no red/green claimed": r6[0] == 0 and f6["gate"]["phase"] == "hardened"
            and s6 == 3 and "Check: manual — add notes 1-3" in doc6 and "✗ red" not in r6[1],
        "full lane on the same bug: hardened too": f5["gate"]["phase"] == "hardened",
        "the fix lane used fewer sessions and tokens than the full lane (same tester script)": s2 < s5 and t2 < t5 and s1 < s2 and t1 < t2,
    }
    print("\nsessions, model requests and tokens per lane (same bug, same short doc):")
    print(f"  {'lane':<34} {'sessions':>8} {'requests':>9} {'tokens':>8} {'seconds':>8}")
    for k, (s, r, t, sec) in rows.items():
        print(f"  {k:<34} {s:>8} {r:>9} {t:>8} {sec:>8}")
    print()
    for name, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + name)
    print("SUMMARY " + json.dumps({"passed": sum(map(bool, checks.values())), "failed": sum(not v for v in checks.values()), "lanes": rows}))
    if not all(checks.values()):
        sys.exit(1)
    for p in keep:
        shutil.rmtree(p.work, ignore_errors=True)


if __name__ == "__main__":
    main()
