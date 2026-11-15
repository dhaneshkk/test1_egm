"""End-to-end safety test on a real git repository and the real DeepSeek
Harness binary (model faked by fake_deepseek.py, no API key).

  * model-written commands: scratch copy, no secrets, no network, read-only $HOME
  * a rejected build is reverted exactly from git (changed files restored, created removed)
  * an accepted build is committed on its own branch; main is untouched until a human merges
  * a step whose dependency is not merged into HEAD is refused; after the merge it builds
  * one command at a time per project (lock), stale locks taken over
  * `egm-dsh doctor` reports the sandbox probe

usage: python3 run_safety_e2e.py EGM_BINARY DSH_COMMAND...
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
HOME_PROBE = Path(os.environ.get("HOME", "/root")) / ".egm-leak-probe"

BEFORE_CLI = (SAMPLE / "before" / "jot" / "cli.py").read_text()
TAGS_CLI = (SAMPLE / "after" / "jot" / "cli.py").read_text()
SEARCH_CLI = TAGS_CLI.replace(
    "\ndef main(", '\ndef search(notes, word):\n    """Notes whose text contains word, ignoring case."""\n'
    '    return [n for n in notes if word.casefold() in n["text"].casefold()]\n\n\ndef main(', 1)
SEARCH_TEST = "import unittest\n\nfrom jot.cli import search\n\n\nclass SearchTest(unittest.TestCase):\n" \
              "    def test_ignores_case(self):\n        self.assertEqual(len(search([{'id': 1, 'text': 'Milk'}], 'milk')), 1)\n"

# The acceptance command doubles as a probe of what model-written code could do.
PROBE = ("python3 -m unittest discover -s tests -t . -q; rc=$?; "
         "echo key=${DEEPSEEK_API_KEY:-none}; "
         f"touch {HOME_PROBE} 2>/dev/null && echo home-writable; "
         "echo leaked > leaked.txt; "
         "(exec 3<>/dev/tcp/1.1.1.1/80) 2>/dev/null && echo net-open; "
         "echo probe-done; exit $rc")


def doc(title, plan, imap, crit):
    return (f"# Design: {title}\n\n## Problem\n\nA need.\n\n## Goals and non-goals\n\nGoals: {title}.\n\nNon-goals: more.\n\n"
            f"## Technical plan\n\n{plan}\n\n## Alternatives considered\n\n**Nothing.** Rejected.\n\n"
            f"## Risks and mitigations\n\n**Small.** Tests.\n\n## Implementation map\n\n{imap}\n\n## Acceptance criteria\n\n{crit}\n")


DOCS = {
    "jot-tags": doc("Tags", "Notes gain `tags`; old notes read as empty. `filter_by_tag` in `jot/cli.py`.",
                    "- `jot/cli.py` — tags\n- `tests/test_tags.py` — tests", "- Tags filter. Check: `tests/test_tags.py`"),
    "jot-search": doc("Search", "Builds on [tags](jot-tags.md). `search(notes, word)` in `jot/cli.py`.",
                      "- `jot/cli.py` — search\n- `tests/test_search.py` — tests", "- Search ignores case. Check: `tests/test_search.py`"),
}
CLEAN = {"comprehension": {"problem": "p", "current_mechanism": "m", "proposed_change": "c", "unclear": []},
         "critic": {"findings": []}, "zoom_out": {"findings": []},
         "readiness": {"ready": True, "blocking_questions": []},
         "invariants": {"criteria": [{"criterion": "c", "predicate": "p", "check": "tests"}]}}
B = "Implement the feature"


def w(path, content):
    return {"match": B, "tool": "write", "args": {"file_path": path, "content": content}}


def r(path):
    return {"match": B, "tool": "read", "args": {"file_path": path}}


def gold(slug):
    return [{"match": "Complete five separate tasks", "tool": "read", "args": {"file_path": f"docs/designs/{slug}.md"}},
            {"match": "Complete five separate tasks", "text": json.dumps(CLEAN)}]


rules = [
    *gold("jot-tags"), *gold("jot-search"),
    # build 1 of jot-tags: also writes a file outside the map -> rejected, reverted
    r("jot/cli.py"), w("jot/cli.py", TAGS_CLI), w("jot/extra.py", "# not in the map\n"), {"match": B, "text": "Done, plus a helper."},
    # build 2 of jot-tags: correct
    r("jot/cli.py"), w("jot/cli.py", TAGS_CLI), w("tests/test_tags.py", (SAMPLE / "after/tests/test_tags.py").read_text()),
    {"match": B, "text": "Tags done."},
    # jot-search, after the human merge
    r("jot/cli.py"), w("jot/cli.py", SEARCH_CLI), w("tests/test_search.py", SEARCH_TEST), {"match": B, "text": "Search done."},
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
    HOME_PROBE.unlink(missing_ok=True)
    work = Path(tempfile.mkdtemp(prefix="egm-dsh-safety-"))
    proj = work / "jot"
    shutil.copytree(SAMPLE / "before", proj, ignore=shutil.ignore_patterns("__pycache__"))
    git = lambda *a: subprocess.run(["git", "-c", "user.name=human", "-c", "user.email=h@example.com", *a], cwd=proj,
                                    text=True, capture_output=True)
    git("init", "-q", "-b", "main")
    git("add", "-A")
    git("commit", "-q", "-m", "jot before")
    (proj / "docs" / "designs").mkdir(parents=True)
    for slug, text in DOCS.items():
        (proj / "docs" / "designs" / f"{slug}.md").write_text(text)
    (work / "rules.json").write_text(json.dumps(rules))
    port = free_port()
    server = subprocess.Popen([sys.executable, "-I", str(HERE / "fake_deepseek.py"), str(port), str(work / "rules.json"), str(work)])
    wait_port(port)
    env = dict(os.environ, DEEPSEEK_API_KEY="fake-key-must-not-leak", DEEPSEEK_BASE_URL=f"http://127.0.0.1:{port}", NO_COLOR="1")

    def egm(*args):
        print(f"\n$ egm-dsh {' '.join(args)}", flush=True)
        p = subprocess.run([EGM, *args], cwd=proj, env=env, text=True, capture_output=True)
        out = (p.stdout + p.stderr).rstrip()
        print(out)
        return p.returncode, out

    try:
        egm("init", "--agent", "dsh", "--preset", "economy", "--dsh-command", *DSH, "--acceptance", "bash", "-c", PROBE)
        doctor = egm("doctor")
        egm("adopt", "jot-tags", "--doc", "docs/designs/jot-tags.md")
        egm("adopt", "jot-search", "--doc", "docs/designs/jot-search.md", "--after", "jot-tags")
        egm("goldfish", "jot-tags")
        egm("goldfish", "jot-search")
        rejected = egm("build", "jot-tags")
        after_reject = {"cli": (proj / "jot/cli.py").read_text(), "extra": (proj / "jot/extra.py").exists(),
                        "branch": git("rev-parse", "--abbrev-ref", "HEAD").stdout.strip()}
        accepted = egm("build", "jot-tags")
        log_tags = git("log", "--format=%an|%s", "egm-dsh/jot-tags").stdout
        main_cli = git("show", "main:jot/cli.py").stdout
        dirty_map = git("status", "--porcelain", "--", "jot/cli.py", "tests/test_tags.py").stdout
        git("switch", "-q", "main")
        unmerged = egm("build", "jot-search")
        git("merge", "-q", "--no-ff", "--no-edit", "egm-dsh/jot-tags")
        merged = egm("build", "jot-search")
        search_branch = git("rev-parse", "--abbrev-ref", "HEAD").stdout.strip()
        # lock: a live holder blocks, a dead one is stale
        (proj / ".egm-dsh/lock").write_text(str(server.pid))
        locked = egm("status")
        (proj / ".egm-dsh/lock").write_text("999999999")
        stale = egm("status")
    finally:
        server.terminate()

    # The sandbox is in use only if doctor's probe line says so. (The "no sandbox: bubblewrap is
    # not available" warning also contains "sandbox: bubblewrap", so match the probe line itself.)
    bwrap = "sandbox: bubblewrap (probe:" in doctor[1]
    # What the acceptance command printed (the echoed command line itself contains the probe words).
    printed = "\n".join(l for l in accepted[1].splitlines() if "acceptance:" not in l)
    checks = {
        "doctor: sandbox probe passes (read-only root, no network, no secrets)" if bwrap else "doctor: reports that there is no sandbox":
            ("probe: read-only root, no network, no secrets" in doctor[1]) if bwrap else "sandbox: none" in doctor[1],
        "rejected build (file outside the map) is refused": rejected[0] != 0 and "`jot/extra.py` changed but is not in" in rejected[1],
        "rejected build reverted: changed file restored from git": after_reject["cli"] == BEFORE_CLI,
        "rejected build reverted: created file removed": not after_reject["extra"],
        "build ran on its own branch": after_reject["branch"] == "egm-dsh/jot-tags",
        "accepted build committed on the branch, design committed first": accepted[0] == 0
            and "egm-dsh|egm-dsh build jot-tags: build" in log_tags and "egm-dsh|egm-dsh design jot-tags" in log_tags,
        "main untouched until a human merges": main_cli == BEFORE_CLI,
        "nothing left uncommitted in the map after the build": dirty_map.strip() == "",
        "dependency not merged into HEAD: build refused": unmerged[0] != 0 and "not merged into `main`" in unmerged[1],
        "after the human merge the dependent step builds on its own branch": merged[0] == 0 and search_branch == "egm-dsh/jot-search",
        "acceptance ran without the API key": "key=none" in printed and "fake-key-must-not-leak" not in accepted[1],
        "acceptance could not write into the repo": not (proj / "leaked.txt").exists(),
        "a live lock blocks a second command": locked[0] != 0 and "another egm-dsh command" in locked[1],
        "a stale lock is taken over": stale[0] == 0,
    }
    if bwrap:
        checks["acceptance had no network"] = "net-open" not in printed and "probe-done" in printed
        checks["acceptance could not write to $HOME"] = "home-writable" not in printed and not HOME_PROBE.exists()
    else:
        # No working bubblewrap (e.g. Colab): egm-dsh must say so on every run, not pretend.
        checks["without a working sandbox, egm-dsh warns that commands have network and file access"] = \
            "no sandbox: bubblewrap is not available" in accepted[1] and "probe-done" in printed
    print()
    for name, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + name)
    print("SUMMARY " + json.dumps({"passed": sum(checks.values()), "failed": sum(not v for v in checks.values()), "bwrap": bwrap}))
    HOME_PROBE.unlink(missing_ok=True)
    if not all(checks.values()):
        sys.exit(1)
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
