"""End-to-end test of the elephant's view and the draft word limit, on the real
DeepSeek Harness binary (model faked by fake_deepseek.py; full request bodies and
reported usage captured).

The same scripted elephant session (glob the repository, answer; a second turn;
draft the doc) runs twice on a repository padded with vendored code, a big data
file and node_modules:

  * A: no `.egmignore`, no scope (the elephant runs in the project, as before)
  * B: `.egmignore` excludes `vendor/`, the scope is `jot` (plus the doc), a 120-word limit

Checks: B's glob never sees vendor, node_modules, the big file or `.egm-dsh`; the
doc is readable in the view; the resumed session survives across turns (same
working directory); reading outside the view warns; the word limit reaches the
prompt and over-long sections are reported; views are removed; B uses fewer input
tokens than A. Prints both token totals.

usage: python3 run_view_e2e.py EGM_BINARY DSH_COMMAND...
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

START = "You are the elephant in an Elephant-Goldfish design session"
SAY = "Keep interrogating"
DRAFT = "Draft the whole design doc"
FIRST = "ELEPHANT-FIRST-REPLY: tags live in jot/cli.py; how should old notes behave?"
LONG_PLAN = " ".join(["Notes gain a tags list and the list command filters on it."] * 25)   # ~275 words
DRAFT_REPLY = "\n\n".join([
    "## Problem\n\nPeople cannot list only work notes.",
    "## Goals and non-goals\n\nGoals: tag notes. Non-goals: renaming.",
    f"## Technical plan\n\n{LONG_PLAN}",
    "## Alternatives considered\n\n**Folders.** Rejected: one note, many tags.",
    "## Risks and mitigations\n\n**Old notes.** Read as untagged.",
    "## Implementation map\n\n- `jot/cli.py` — tags",
    "## Acceptance criteria\n\n- Tags filter. Check: `tests/test_tags.py`",
])


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


def make_repo(proj):
    shutil.copytree(SAMPLE / "before", proj, ignore=shutil.ignore_patterns("__pycache__"))
    (proj / "README.md").write_text("# jot\n\nREADME-OUTSIDE-SCOPE\n")
    for i in range(150):
        d = proj / "vendor" / f"lib{i // 30}"
        d.mkdir(parents=True, exist_ok=True)
        (d / f"module_{i:03d}.py").write_text(f"# vendored module {i}\n" + "def f():\n    return 1\n" * 40)
    (proj / "node_modules" / "left-pad").mkdir(parents=True)
    (proj / "node_modules" / "left-pad" / "index.js").write_text("module.exports = 1;\n")
    (proj / "data").mkdir()
    (proj / "data" / "dump.json").write_text("[" + ",".join(["1"] * 400_000) + "]")   # > 512 KiB


def main():
    work = Path(tempfile.mkdtemp(prefix="egm-view-test-"))
    a, b, c = work / "A", work / "B", work / "C"
    rules = []
    for proj in (a, b, c):
        rules += [{"match": START, "tool": "glob", "args": {"pattern": "**/*"}}]
        # same script everywhere: try egm-dsh's own state (as a real elephant did); only A, with no view, gets it
        if True:
            rules += [{"match": START, "tool": "read", "args": {"file_path": ".egm-dsh/config.json"}}]
        rules += [{"match": START, "text": FIRST}]
        # second turn (same script for A and B): reads the doc, then the root README by absolute path
        if True:
            rules += [{"match": SAY, "tool": "read", "args": {"file_path": "docs/designs/tags.md"}},
                      {"match": SAY, "tool": "read", "args": {"file_path": str(proj / "README.md")}}]
        rules += [{"match": SAY, "text": "No more questions."}, {"match": DRAFT, "text": DRAFT_REPLY}]
    (work / "rules.json").write_text(json.dumps(rules))
    (work / "DUMP").write_text("")
    port = free_port()
    server = subprocess.Popen([sys.executable, "-I", str(HERE / "fake_deepseek.py"), str(port), str(work / "rules.json"), str(work)])
    wait_port(port)
    env = dict(os.environ, DEEPSEEK_API_KEY="fake", DEEPSEEK_BASE_URL=f"http://127.0.0.1:{port}", NO_COLOR="1")

    def egm(proj, *args):
        p = subprocess.run([EGM, *args], cwd=proj, env=env, text=True, capture_output=True, timeout=600)
        out = (p.stdout + p.stderr).rstrip()
        print(f"$ egm-dsh {' '.join(args)}\n{out[-2500:]}\n", flush=True)
        return p.returncode, out

    def bodies():
        f = work / "bodies.jsonl"
        return [json.loads(l) for l in f.read_text().splitlines()] if f.exists() else []

    def elephant_tokens(proj):
        rows = [json.loads(l) for l in (proj / ".egm-dsh" / "usage.jsonl").read_text().splitlines()]
        return sum(r["cache_hit"] + r["cache_miss"] for r in rows if r["role"] == "elephant")

    outs, spans = {}, {}
    try:
        for proj in (a, b, c):
            make_repo(proj)
            egm(proj, "init", "--agent", "dsh", "--preset", "economy", "--dsh-command", *DSH)
            if proj == a:   # the old behaviour, for the comparison: no view at all
                outs["no-view"] = egm(proj, "set-scope", "--no-view")
            if proj == b:
                (proj / ".egmignore").write_text("# vendored code the elephant never needs\nvendor/\n")
                outs["scope"] = egm(proj, "set-scope", "jot")
                outs["words"] = egm(proj, "set-draft-words", "120")
            n0 = len(bodies())
            outs[f"{proj.name}-start"] = egm(proj, "elephant", "tags", "--idea", "Let notes carry tags and filter the list by tag.")
            spans[f"{proj.name}-start"] = (n0, len(bodies()))
            outs[f"{proj.name}-say"] = egm(proj, "elephant", "tags", "--say", "Old notes have no tags.")
            outs[f"{proj.name}-draft"] = egm(proj, "draft", "tags")
            spans[proj.name] = (n0, len(bodies()))
        left = sorted(Path(tempfile.gettempdir()).glob("egm-dsh-view-*"))
    finally:
        server.terminate()

    allb = bodies()
    text = {k: json.dumps(allb[s:e]) for k, (s, e) in spans.items()}
    say_b = [x for x in allb[spans["B"][0]:spans["B"][1]] if SAY in json.dumps(x)]
    draft_b = [x for x in allb[spans["B"][0]:spans["B"][1]] if DRAFT in json.dumps(x)]
    tok_a, tok_b = elephant_tokens(a), elephant_tokens(b)
    doc_b = (b / "docs" / "designs" / "tags.md").read_text()
    checks = {
        "A (no view): the elephant's glob lists the vendored code": "module_042.py" in text["A"],
        "B: the glob never lists vendor/, node_modules/, the big data file or .egm-dsh/": all(
            s not in text["B"] for s in ("module_042.py", "left-pad", "dump.json", "dsh_command")),
        "B: the scope keeps jot/ and the doc; the root README is not listed": "jot/cli.py" in text["B-start"]
            and "docs/designs/tags.md" in text["B-start"] and "README.md" not in text["B-start"],
        "B: the view is announced with what it left out": "elephant view:" in outs["B-start"][1] and "left out by" in outs["B-start"][1],
        "B: the resumed session keeps its history across turns (same working directory)": bool(say_b) and FIRST in json.dumps(say_b[-1]),
        "B: the doc is readable inside the view": "Design: Tags" in json.dumps(say_b),
        "B: reading outside the view warns": "read outside its view" in outs["B-say"][1],
        "B: the word limit is in the draft prompt": bool(draft_b) and "at most 120 words per section" in json.dumps(draft_b[0]),
        "B: words per section are reported and the long one is flagged, text kept": "words per section:" in outs["B-draft"][1]
            and "over the 120-word limit: Technical plan" in outs["B-draft"][1] and LONG_PLAN[:60] in doc_b,
        "A: no word limit, no view": "words per section" not in text["A"] and "elephant view:" not in outs["A-start"][1],
        "A (`--no-view`): the elephant reads egm-dsh's own state": "dsh_command" in text["A-start"],
        "C (default, no scope, no .egmignore): the elephant runs in a view": "elephant view:" in outs["C-start"][1],
        "C: egm-dsh's state is not there (read fails), node_modules and the big file left out, the code kept": "dsh_command" not in text["C-start"]
            and "cannot read" in text["C-start"] and "config.json" in text["C-start"] and "not found" in text["C-start"]
            and "left-pad" not in text["C-start"] and "dump.json" not in text["C-start"]
            and "module_042.py" in text["C-start"] and "jot/cli.py" in text["C-start"],
        "every view is removed afterwards": left == [],
        "B uses fewer elephant input tokens than A": 0 < tok_b < tok_a,
    }
    print(f"\nelephant input tokens over 3 commands (start, say, draft): A {tok_a}, B {tok_b} "
          f"({(1 - tok_b / tok_a):.0%} fewer)" if tok_a else "")
    for name, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + name)
    print("SUMMARY " + json.dumps({"passed": sum(map(bool, checks.values())), "failed": sum(not v for v in checks.values()),
                                   "elephant_input_tokens": {"A": tok_a, "B": tok_b}}))
    if not all(checks.values()):
        sys.exit(1)
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
