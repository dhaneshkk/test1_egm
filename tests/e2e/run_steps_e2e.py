"""End-to-end test of iterative development: a chain of small steps on the
real DeepSeek Harness binary (model faked by fake_deepseek.py, no API key).

  jot-tags     two build phases (storage+filter, then tests)
  jot-search   --after jot-tags; also changes jot/cli.py
  jot-huge     seven files: over the step-size limit

Checks: dependency refusal and cycle refusal, phases in order, a phase that
leaves the feature `ready`, handover of the shared file to the later build,
drift reported under the latest owner, the size alarm, and the chain status.

usage: python3 run_steps_e2e.py EGM_BINARY DSH_COMMAND...
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

TAGS_CLI = (SAMPLE / "after" / "jot" / "cli.py").read_text()
SEARCH_CLI = TAGS_CLI.replace(
    "\ndef main(", '\ndef search(notes, word):\n    """Notes whose text contains word, ignoring case."""\n'
    '    return [n for n in notes if word.casefold() in n["text"].casefold()]\n\n\ndef main(', 1)
SEARCH_TEST = '''import unittest

from jot.cli import search


class SearchTest(unittest.TestCase):
    def test_ignores_case(self):
        notes = [{"id": 1, "text": "Buy Milk"}, {"id": 2, "text": "call bob"}]
        self.assertEqual([n["id"] for n in search(notes, "milk")], [1])
'''
COMMON = """## Goals and non-goals

Goals: {goal}.

Non-goals: anything else.

## Technical plan

{plan}

## Alternatives considered

**Do nothing.** Rejected: the need is real.

## Risks and mitigations

**Small risk.** Mitigation: tests.
"""


def doc(title, problem, goal, plan, imap, crit):
    return f"# Design: {title}\n\n## Problem\n\n{problem}\n\n" + COMMON.format(goal=goal, plan=plan) + \
        f"\n## Implementation map\n\n{imap}\n\n## Acceptance criteria\n\n{crit}\n"


DOCS = {
    "jot-tags": doc("Tags", "People cannot list only work notes.", "tag notes; filter by tag",
                    "Notes gain a `tags` list in `notes.json`; old notes without it read as empty. `filter_by_tag` in `jot/cli.py`.",
                    "### Phase 1: storage and filter\n\n- `jot/cli.py` — tags field, `--tag`, `filter_by_tag`\n\n"
                    "### Phase 2: tests\n\n- `tests/test_tags.py` — tagging and filtering tests",
                    "- `jot list --tag work` prints only work notes. Check: `tests/test_tags.py`"),
    "jot-search": doc("Search", "People scroll to find a note.", "search note text",
                      "Builds on [tags](jot-tags.md). `search(notes, word)` in `jot/cli.py` matches text case-insensitively.",
                      "- `jot/cli.py` — `search` helper\n- `tests/test_search.py` — search tests",
                      "- `search` ignores case. Check: `tests/test_search.py`"),
    "jot-huge": doc("Everything", "Many wishes at once.", "export, import, sync, themes, plugins",
                    "One big change.", "\n".join(f"- `jot/part{i}.py` — part {i}" for i in range(1, 8)),
                    "- It all works. Check: manual — demo"),
}
CLEAN = {"comprehension": {"problem": "p", "current_mechanism": "m", "proposed_change": "c", "unclear": []},
         "critic": {"findings": []}, "zoom_out": {"findings": []},
         "readiness": {"ready": True, "blocking_questions": []},
         "invariants": {"criteria": [{"criterion": "c", "predicate": "p", "check": "tests"}]}}


def goldfish(slug):
    d = f"docs/designs/{slug}.md"
    return [{"match": "Complete five separate tasks", "tool": "read", "args": {"file_path": d}},
            {"match": "Complete five separate tasks", "text": json.dumps(CLEAN)}]


rules = [
    *goldfish("jot-search"),
    *goldfish("jot-tags"),
    {"match": "Implement phase 1 of 2", "tool": "read", "args": {"file_path": "jot/cli.py"}},
    {"match": "Implement phase 1 of 2", "tool": "write", "args": {"file_path": "jot/cli.py", "content": TAGS_CLI}},
    {"match": "Implement phase 1 of 2", "text": "Phase 1: tags stored and filtered."},
    {"match": "Implement phase 2 of 2", "tool": "write", "args": {"file_path": "tests/test_tags.py", "content": (SAMPLE / "after/tests/test_tags.py").read_text()}},
    {"match": "Implement phase 2 of 2", "text": "Phase 2: tests added."},
    {"match": "Implement the feature", "tool": "read", "args": {"file_path": "jot/cli.py"}},
    {"match": "Implement the feature", "tool": "write", "args": {"file_path": "jot/cli.py", "content": SEARCH_CLI}},
    {"match": "Implement the feature", "tool": "write", "args": {"file_path": "tests/test_search.py", "content": SEARCH_TEST}},
    {"match": "Implement the feature", "text": "Added search and its test."},
    *goldfish("jot-huge"),
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
    work = Path(tempfile.mkdtemp(prefix="egm-dsh-steps-"))
    proj = work / "jot"
    shutil.copytree(SAMPLE / "before", proj, ignore=shutil.ignore_patterns("__pycache__"))
    (proj / "docs" / "designs").mkdir(parents=True)
    for slug, text in DOCS.items():
        (proj / "docs" / "designs" / f"{slug}.md").write_text(text)
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
        egm("init", "--agent", "dsh", "--preset", "economy", "--dsh-command", *DSH,
            "--acceptance", "python3", "-m", "unittest", "discover", "-s", "tests", "-t", ".", "-q")
        egm("adopt", "jot-tags", "--doc", "docs/designs/jot-tags.md")
        egm("adopt", "jot-search", "--doc", "docs/designs/jot-search.md", "--after", "jot-tags")
        cycle = egm("after", "jot-tags", "jot-search")
        egm("goldfish", "jot-search")
        dep_refused = egm("build", "jot-search")
        egm("goldfish", "jot-tags")
        out_of_order = egm("build", "jot-tags", "--phase", "2")
        phase1 = egm("build", "jot-tags", "--phase", "1")
        mid = json.loads((proj / ".egm-dsh/features/jot-tags.json").read_text())
        phase2 = egm("build", "jot-tags")
        search_built = egm("build", "jot-search")
        drift_clean = egm("drift")
        (proj / "jot" / "cli.py").write_text((proj / "jot" / "cli.py").read_text() + "\n# hand edit\n")
        drift_dirty = egm("drift")
        egm("adopt", "jot-huge", "--doc", "docs/designs/jot-huge.md")
        huge = egm("goldfish", "jot-huge")
        status = egm("status")
    finally:
        server.terminate()

    tags = json.loads((proj / ".egm-dsh/features/jot-tags.json").read_text())
    search = json.loads((proj / ".egm-dsh/features/jot-search.json").read_text())
    log = [json.loads(l) for l in (work / "requests.jsonl").read_text().splitlines()]
    lines = status[1].splitlines()
    row = lambda slug: next((i for i, l in enumerate(lines) if l.startswith(slug + " ")), -1)
    checks = {
        "cycle refused (jot-tags after jot-search after jot-tags)": cycle[0] != 0 and "cycle" in cycle[1],
        "build refused until the step it depends on is built": dep_refused[0] != 0 and "comes after `jot-tags`" in dep_refused[1],
        "phases build in order (phase 2 before 1 refused)": out_of_order[0] != 0 and "build phase 1 first" in out_of_order[1],
        "after phase 1 the feature is still ready, 1 phase done": phase1[0] == 0 and mid["gate"]["phase"] == "ready" and mid["phases_done"] == 1,
        "the final phase makes it built": phase2[0] == 0 and tags["gate"]["phase"] == "built" and "built in 2 phases" in phase2[1],
        "each phase was its own builder session": sum("Implement phase" in r["task"] for r in log if r["reply"] == "text") == 2,
        "later build takes over the shared file": search_built[0] == 0 and "`jot/cli.py` from `jot-tags`" in search_built[1]
                                                  and search["built_seq"] > tags["built_seq"],
        "no false drift on the earlier step after the later build": drift_clean[0] == 0 and "checked under `jot-search`" in drift_clean[1],
        "a hand edit is reported under the latest owner only": drift_dirty[0] == 2 and "jot-search: `jot/cli.py` changed" in drift_dirty[1]
                                                               and "jot-tags: `jot/cli.py` changed" not in drift_dirty[1],
        "oversized step gets the size alarm and is not ready": "7 files (limit 6)" in huge[1] and "NOT READY" in huge[1],
        "status lists the chain in dependency order": 0 <= row("jot-tags") < row("jot-search") and "2/2" in lines[row("jot-tags")],
        # the built-in reviewer skips the scripted file reads (its files are in the prompt), so fewer requests than rules
        "every model request matched a scripted reply": not any(r.get("unmatched") for r in log)
            and len([r for r in log if not r["title"]]) <= len(rules),
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
