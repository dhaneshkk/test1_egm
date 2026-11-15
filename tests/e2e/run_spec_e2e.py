"""End-to-end test of the executable spec on the real DeepSeek Harness binary
(model faked by fake_deepseek.py, no API key).

  1. tests first: build refused until tests are frozen
  2. a vacuous test (passes before any code) is refused: red required
  3. real tests: red, reviewed by a goldfish, frozen
  4. a builder that weakens a frozen test is rejected and the test restored
  5. a correct build turns every check green: each test on its own, the
     doc's examples, and (with Lean available) the Lean theorem
  6. `egm-dsh check` runs the spec; changing the Lean statement is caught

usage: python3 run_spec_e2e.py EGM_BINARY DSH_COMMAND...    (LEAN_BIN=... enables the Lean criterion)
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
EGM, DSH = sys.argv[1], sys.argv[2:]
LEAN = os.environ.get("LEAN_BIN") or shutil.which("lean")

TESTS = (SAMPLE / "after" / "tests" / "test_tags.py").read_text()
VACUOUS = '''import unittest


class TagTest(unittest.TestCase):
    def test_add_stores_tags_without_duplicates(self):
        self.assertTrue(True)

    def test_filter_returns_only_tagged_notes(self):
        self.assertTrue(True)

    def test_notes_saved_before_tags_still_load_and_never_match(self):
        self.assertTrue(True)
'''
WEAKENED = TESTS.replace('self.assertEqual(note["tags"], ["work", "daily"])', "self.assertTrue(True)")
LEAN_SPEC = '''/-- A note: its id and its tags. -/
abbrev Note := Nat × List String

/-- Model of `filter_by_tag`: keep the notes carrying `tag`. -/
def filterByTag (notes : List Note) (tag : String) : List Note :=
  notes.filter (fun n => decide (tag ∈ n.2))

/-- Criterion: every note `list --tag T` shows carries T. -/
theorem filter_sound (notes : List Note) (tag : String) :
    ∀ n ∈ filterByTag notes tag, tag ∈ n.2 := by
  sorry
'''
LEAN_PROVED = LEAN_SPEC.replace("  sorry\n", "  intro n hn\n  simp [filterByTag] at hn\n  exact hn.2\n")
LEAN_WEAKENED = LEAN_PROVED.replace("∀ n ∈ filterByTag notes tag, tag ∈ n.2", "True").replace("  intro n hn\n  simp [filterByTag] at hn\n  exact hn.2\n", "  trivial\n")

CRITERIA = [
    "- `jot add \"standup\" --tag work --tag work` stores `tags: [\"work\"]`. Check: `tests/test_tags.py::TagTest::test_add_stores_tags_without_duplicates`",
    "- `filter_by_tag` returns only notes carrying the tag. Check: `tests/test_tags.py::TagTest::test_filter_returns_only_tagged_notes`",
    "- Notes saved before tags existed still load and never match a tag. Check: `tests/test_tags.py::TagTest::test_notes_saved_before_tags_still_load_and_never_match`",
]
CHECKS = ["tests/test_tags.py::TagTest::test_add_stores_tags_without_duplicates",
          "tests/test_tags.py::TagTest::test_filter_returns_only_tagged_notes",
          "tests/test_tags.py::TagTest::test_notes_saved_before_tags_still_load_and_never_match"]
MAP = ["- `jot/cli.py` — tags on notes, `--tag` on add and list, `filter_by_tag`", "- `tests/test_tags.py` — acceptance tests"]
if LEAN:
    CRITERIA.append("- Every note a tag filter returns carries the tag. Check: `lean proofs/Tags.lean#filter_sound`")
    CHECKS.append("lean proofs/Tags.lean#filter_sound")
    MAP.append("- `proofs/Tags.lean` — model of the filter and its soundness theorem")

DOC = f"""# Design: Tags

## Problem

People cannot list only their work notes.

## Goals and non-goals

Goals: tag notes; list notes by tag.

Non-goals: renaming tags.

## Technical plan

Each note gains a `tags` list in `notes.json`; readers treat a missing key as empty. `jot add --tag` (repeatable, duplicates dropped) and `jot list --tag` use `filter_by_tag(notes, tag)` in `jot/cli.py`. `render` appends `  [tag, ...]`.

## Alternatives considered

**A separate index file.** Rejected: two files can disagree.

## Risks and mitigations

**Typos split tags.** Mitigation: tags are printed.

## Implementation map

{chr(10).join(MAP)}

## Acceptance criteria

{chr(10).join(CRITERIA)}

## Examples

```console
$ python3 -m jot.cli add standup --tag work
added #1
$ python3 -m jot.cli add groceries --tag home
added #2
$ python3 -m jot.cli list --tag work
#1 standup  [work]
```
"""
D = "docs/designs/jot-tags.md"
CLEAN = {"comprehension": {"problem": "p", "current_mechanism": "m", "proposed_change": "c", "unclear": []},
         "critic": {"findings": []}, "zoom_out": {"findings": []},
         "readiness": {"ready": True, "blocking_questions": []},
         "invariants": {"criteria": [{"criterion": "c", "predicate": "p", "check": "tests"}]}}
T = "Write the acceptance tests for the design document"
B = "Implement the feature"


def write(path, content):
    return {"tool": "write", "args": {"file_path": path, "content": content}}


def read(path):
    return {"tool": "read", "args": {"file_path": path}}


rules = [
    {"match": "Complete five separate tasks", **read(D)},
    {"match": "Complete five separate tasks", "text": json.dumps(CLEAN)},
    # round 1: vacuous tests
    {"match": T, **write("tests/test_tags.py", VACUOUS)},
    *([{"match": T, **write("proofs/Tags.lean", LEAN_SPEC)}] if LEAN else []),
    {"match": T, "text": "Wrote the tests."},
    # round 2: real tests (dsh must read a file before overwriting it)
    {"match": T, **read("tests/test_tags.py")},
    {"match": T, **write("tests/test_tags.py", TESTS)},
    *([{"match": T, **read("proofs/Tags.lean")}, {"match": T, **write("proofs/Tags.lean", LEAN_SPEC)}] if LEAN else []),
    {"match": T, "text": "Wrote one test per criterion; they fail until tags exist."},
    {"match": "Judge whether each acceptance test", **read("tests/test_tags.py")},
    {"match": "Judge whether each acceptance test", "text": json.dumps({"criteria": [{"check": c, "tests_it": True, "reason": ""} for c in CHECKS]})},
    # build 1: weakens a frozen test
    {"match": B, **read("jot/cli.py")},
    {"match": B, **write("jot/cli.py", (SAMPLE / "after/jot/cli.py").read_text())},
    {"match": B, **read("tests/test_tags.py")},
    {"match": B, **write("tests/test_tags.py", WEAKENED)},
    {"match": B, "text": "Done (and simplified a test)."},
    # build 2: correct
    {"match": B, **read("jot/cli.py")},
    *([{"match": B, **read("proofs/Tags.lean")}, {"match": B, **write("proofs/Tags.lean", LEAN_PROVED)}] if LEAN else []),
    {"match": B, "text": "Tags implemented" + (" and filter_sound proved." if LEAN else ".")},
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


def main():
    work = Path(tempfile.mkdtemp(prefix="egm-dsh-spec-"))
    proj = work / "jot"
    shutil.copytree(SAMPLE / "before", proj, ignore=shutil.ignore_patterns("__pycache__"))
    (proj / "docs" / "designs").mkdir(parents=True)
    (proj / "proofs").mkdir()
    (proj / D).write_text(DOC)
    (work / "rules.json").write_text(json.dumps(rules))
    port = free_port()
    server = subprocess.Popen([sys.executable, "-I", str(HERE / "fake_deepseek.py"), str(port), str(work / "rules.json"), str(work)])
    wait_port(port)
    env = dict(os.environ, DEEPSEEK_API_KEY="fake", DEEPSEEK_BASE_URL=f"http://127.0.0.1:{port}", NO_COLOR="1")

    def egm(*args):
        print(f"\n$ egm-dsh {' '.join(args)}", flush=True)
        r = subprocess.run([EGM, *args], cwd=proj, env=env, text=True, capture_output=True)
        out = (r.stdout + r.stderr).rstrip()
        print(out)
        return r.returncode, out

    try:
        egm("init", "--agent", "dsh", "--preset", "economy", "--tests-first", "--dsh-command", *DSH,
            "--acceptance", "python3", "-m", "unittest", "discover", "-s", "tests", "-t", ".", "-q")
        egm("set-spec", "--check-cmd", "python3", "-m", "unittest", "-q", "{unittest}")
        if LEAN:
            egm("set-spec", "--lean-cmd", LEAN)
        egm("adopt", "jot-tags", "--doc", D)
        egm("goldfish", "jot-tags")
        not_first = egm("build", "jot-tags")
        vacuous = egm("tests", "jot-tags")
        frozen = egm("tests", "jot-tags")
        feature = json.loads((proj / ".egm-dsh/features/jot-tags.json").read_text())
        weakened = egm("build", "jot-tags")
        restored = (proj / "tests/test_tags.py").read_text() == TESTS
        built = egm("build", "jot-tags")
        green = egm("check", "jot-tags")
        if LEAN:
            egm("set-spec", "--lean-cmd", "/nonexistent/lean")
            no_lean = egm("check", "jot-tags")
            egm("set-spec", "--lean-cmd", LEAN)
            (proj / "proofs/Tags.lean").write_text(LEAN_WEAKENED)
            tampered = egm("check", "jot-tags")
    finally:
        server.terminate()

    final = json.loads((proj / ".egm-dsh/features/jot-tags.json").read_text())
    log = [json.loads(l) for l in (work / "requests.jsonl").read_text().splitlines()]
    spec = feature.get("spec") or {}
    checks = {
        "tests first: build refused before tests are frozen": not_first[0] != 0 and "tests come first" in not_first[1],
        "vacuous tests refused (they pass before any code)": vacuous[0] != 0 and "vacuous" in vacuous[1] and "spec frozen" not in vacuous[1],
        "real tests went red and were frozen after review": frozen[0] == 0 and "spec frozen" in frozen[1]
            and all(c["red"] == "red" for c in spec.get("criteria", [])) and len(spec.get("criteria", [])) == len(CHECKS),
        "the doc's example went red before the build": spec.get("examples", [[None, None]])[0][1] == "red",
        "a build that weakens a frozen test is rejected": weakened[0] != 0 and "frozen test `tests/test_tags.py` was changed" in weakened[1],
        "the frozen test was restored": restored,
        "correct build: green, each test on its own, example and proof": built[0] == 0 and final["gate"]["phase"] == "built"
            and "green: all" in built[1],
        "egm-dsh check runs the spec green": green[0] == 0 and "the spec runs green" in green[1] and "✔ example 1" in green[1],
        # the built-in reviewer skips the scripted file reads (its files are in the prompt), so fewer requests than rules
        "every model request matched a scripted reply": not any(r.get("unmatched") for r in log)
            and len([r for r in log if not r["title"]]) <= len(rules),
    }
    if LEAN:
        checks["Lean: sorry counted as red, real proof green"] = "✔ lean proofs/Tags.lean#filter_sound" in green[1]
        checks["a Lean that cannot run is an error, never a result"] = no_lean[0] == 2 and "could not run Lean" in no_lean[1] \
            and "still uses sorry" not in no_lean[1]
        checks["Lean: changing the statement is caught"] = tampered[0] == 2 and "changed outside the proof of `filter_sound`" in tampered[1]
    print()
    for name, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + name)
    print("SUMMARY " + json.dumps({"passed": sum(checks.values()), "failed": sum(not v for v in checks.values()), "lean": bool(LEAN)}))
    if not all(checks.values()):
        sys.exit(1)
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
