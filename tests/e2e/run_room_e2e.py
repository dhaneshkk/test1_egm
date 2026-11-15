"""End-to-end test of the goldfish reading room on the real DeepSeek Harness binary
(model faked by fake_deepseek.py; full request bodies captured).

A real DeepSeek run showed a goldfish reading unlinked files (README, other tests) and
an elephant globbing into `.egm-dsh/`. The reading room makes "a goldfish sees only the
doc and its links" a property of where the session runs, checked on every tool call:

  * a goldfish session runs in a stage holding only the doc and its links/map files;
    an unlinked README is simply not there, and neither is `.egm-dsh/`
  * a goldfish that reads the previous round's review by absolute path is discarded:
    the round is INCOMPLETE, never READY
  * a goldfish that globs the filesystem root is discarded the same way
  * a verifier that leaves the room is discarded and drops no alarm
  * a goldfish that stays inside passes, and the review records what it could read

usage: python3 run_room_e2e.py EGM_BINARY DSH_COMMAND...
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

DOC = """# Design: Tags

## Problem

People cannot list only work notes.

## Goals and non-goals

Goals: tag notes. Non-goals: renaming.

## Technical plan

Notes gain `tags`; old notes read as having none. Matching is exact.

## Alternatives considered

**None.** Rejected.

## Risks and mitigations

**Small.** Tests.

## Implementation map

- `jot/cli.py` — tags

## Acceptance criteria

- Tags filter. Check: `tests/test_tags.py`
"""
SECRET_IN_README = "README-ONLY-FACT-7731"
G = "Complete five separate tasks"
V = "You are verifying alarms"
CLEAN = {"comprehension": {"problem": "p", "current_mechanism": "m", "proposed_change": "c", "unclear": []},
         "critic": {"findings": []}, "zoom_out": {"findings": []},
         "readiness": {"ready": True, "blocking_questions": []},
         "invariants": {"criteria": [{"criterion": "c", "predicate": "p", "check": "tests"}]}}
ASKS = dict(CLEAN, readiness={"ready": False, "blocking_questions": [
    {"question": "Is tag matching case-sensitive?", "section": "Technical plan", "search_terms": ["case"]}]})


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
    work = Path(tempfile.mkdtemp(prefix="egm-dsh-room-"))
    proj = work / "jot"
    shutil.copytree(SAMPLE / "before", proj, ignore=shutil.ignore_patterns("__pycache__"))
    (proj / "README.md").write_text(f"# jot\n\nUnlinked project notes: {SECRET_IN_README}\n")
    (proj / "docs" / "designs").mkdir(parents=True)
    (proj / "docs" / "designs" / "tags.md").write_text(DOC)
    review1 = proj / ".egm-dsh" / "reviews" / "tags-1.json"
    rules = [
        # round 1: reads the doc, the map file and an unlinked README (absent in the room), then asks a question
        {"match": G, "tool": "read", "args": {"file_path": "docs/designs/tags.md"}},
        {"match": G, "tool": "read", "args": {"file_path": "jot/cli.py"}},
        {"match": G, "tool": "read", "args": {"file_path": "README.md"}},
        {"match": G, "text": json.dumps(ASKS)},
        # round 2: peeks at round 1's review by absolute path, then claims READY
        {"match": G, "tool": "read", "args": {"file_path": str(review1)}},
        {"match": G, "text": json.dumps(CLEAN)},
        # round 3: globs the filesystem root, then claims READY
        {"match": G, "tool": "glob", "args": {"pattern": "**/reviews/*.json", "path": "/"}},
        {"match": G, "text": json.dumps(CLEAN)},
        # round 4 (verifier on): a question; the verifier reads egm-dsh's config by absolute path and "answers" it
        {"match": G, "text": json.dumps(ASKS)},
        {"match": V, "tool": "read", "args": {"file_path": str(proj / ".egm-dsh" / "config.json")}},
        {"match": V, "text": json.dumps({"items": [{"id": 1, "verdict": "answered", "quote": "Matching is exact", "reason": "r"}]})},
        # round 5: stays inside and passes
        {"match": G, "tool": "read", "args": {"file_path": "docs/designs/tags.md"}},
        {"match": G, "text": json.dumps(CLEAN)},
    ]
    (work / "rules.json").write_text(json.dumps(rules))
    (work / "DUMP").write_text("")
    port = free_port()
    server = subprocess.Popen([sys.executable, "-I", str(HERE / "fake_deepseek.py"), str(port), str(work / "rules.json"), str(work)])
    wait_port(port)
    env = dict(os.environ, DEEPSEEK_API_KEY="fake", DEEPSEEK_BASE_URL=f"http://127.0.0.1:{port}", NO_COLOR="1")

    def egm(*args):
        p = subprocess.run([EGM, *args], cwd=proj, env=env, text=True, capture_output=True)
        out = (p.stdout + p.stderr).rstrip()
        print(f"$ egm-dsh {' '.join(args)}\n{out}\n", flush=True)
        return p.returncode, out

    def review(n):
        return json.loads((proj / ".egm-dsh" / "reviews" / f"tags-{n}.json").read_text())

    def phase():
        return json.loads((proj / ".egm-dsh" / "features" / "tags.json").read_text())["gate"]["phase"]

    try:
        egm("init", "--agent", "dsh", "--preset", "economy", "--dsh-command", *DSH)
        egm("set-reviewers", "dsh")   # this suite tests dsh reading rooms and processes; run_reviewers_e2e.py covers the built-in client
        egm("adopt", "tags", "--doc", "docs/designs/tags.md")
        r1 = egm("goldfish", "tags", "--no-verify")
        r2 = egm("goldfish", "tags", "--no-verify")
        p2 = phase()
        r3 = egm("goldfish", "tags", "--no-verify")
        p3 = phase()
        r4 = egm("goldfish", "tags")
        r5 = egm("goldfish", "tags", "--no-verify")
        p5 = phase()
        stages_left = sorted(Path(tempfile.gettempdir()).glob(f"egm-dsh-stage-*"))
    finally:
        server.terminate()

    bodies = [json.loads(l) for l in (work / "bodies.jsonl").read_text().splitlines()]
    goldfish_text = json.dumps([b for b in bodies if G in json.dumps(b.get("messages", []))])
    rv = [review(n) for n in range(1, 6)]
    checks = {
        "round 1 ran in the room: doc and map file readable, README and .egm-dsh absent": r1[0] == 0
            and rv[0]["reading_room"] == ["docs/designs/tags.md", "jot/cli.py"] and rv[0]["escapes"] == [],
        "the unlinked README's content never reached the model": SECRET_IN_README not in goldfish_text,
        "the map file's content did reach the model (reading inside works)": "def add(notes, text)" in goldfish_text,
        "round 2: reading the last review by absolute path discards the goldfish": rv[1]["escapes"] and "reviews/tags-1.json" in rv[1]["escapes"][0]
            and rv[1]["verdict"] == "incomplete" and "left the doc" in r2[1],
        "round 2: the peek never makes the doc ready": p2 == "designing" and "READY —" not in r2[1],
        "round 3: globbing the filesystem root discards the goldfish": rv[2]["escapes"] and "`glob` /" in rv[2]["escapes"][0]
            and rv[2]["verdict"] == "incomplete" and p3 == "designing",
        "round 4: a verifier that leaves the room is discarded and drops no alarm": any("verifier" in e for e in rv[3]["escapes"])
            and rv[3]["verdict"] == "not-ready" and not any(a.get("dropped_by") == "verifier" for a in rv[3]["alarms"]),
        "round 5: a goldfish that stays inside passes": rv[4]["verdict"] == "ready" and rv[4]["escapes"] == [] and p5 == "ready",
        "every round's room is removed afterwards": stages_left == [],
    }
    print()
    for name, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + name)
    print("SUMMARY " + json.dumps({"passed": sum(map(bool, checks.values())), "failed": sum(not v for v in checks.values())}))
    if not all(checks.values()):
        sys.exit(1)
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
