"""End-to-end test of structured escalation on the real DeepSeek Harness binary
(model faked by fake_deepseek.py; full request bodies captured).

  * a NOT READY goldfish raises three questions
  * `escalate`: the elephant (read-only dsh session) answers one from the repository
    with a real quote (accepted), escalates one as a decision with labelled options,
    and "answers" one with an invented quote (egm-dsh escalates it instead)
  * the gate does not move; `redraft` and `build` are refused while decisions are open
  * `decide` records an option and a free-text answer; `redraft` writes them into the
    doc (`## Decisions` + the sections concerned) and leaves the feature in design
  * `build` is still refused; only a FRESH goldfish (new session, sees only the doc)
    passing the new revision unlocks it, and that goldfish never sees the decisions record

usage: python3 run_escalate_e2e.py EGM_BINARY DSH_COMMAND...
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

Notes gain `tags`. `jot list --tag T` filters.

## Alternatives considered

**None.** Rejected.

## Risks and mitigations

**Small.** Tests.

## Implementation map

- `jot/cli.py` — tags

## Acceptance criteria

- Tags filter. Check: `tests/test_tags.py`
"""
Q1 = "Where are notes persisted, and in what format?"
Q2 = "How should notes saved before this change, which have no tags field, behave under --tag?"
Q3 = "Which module owns the tag index?"
INVENTED = "class TagIndex(sqlite3.Connection)"
WHY_ONLY_IN_RECORD = "the person owns the product call on legacy notes"
NOT_READY = {"comprehension": {"problem": "p", "current_mechanism": "m", "proposed_change": "c", "unclear": []},
             "critic": {"findings": []}, "zoom_out": {"findings": []},
             "readiness": {"ready": False, "blocking_questions": [{"question": q, "section": "Technical plan"} for q in (Q1, Q2, Q3)]},
             "invariants": {"criteria": [{"criterion": "c", "predicate": "p", "check": "tests"}]}}
CLEAN = {"comprehension": {"problem": "p", "current_mechanism": "m", "proposed_change": "c", "unclear": []},
         "critic": {"findings": []}, "zoom_out": {"findings": []},
         "readiness": {"ready": True, "blocking_questions": []},
         "invariants": {"criteria": [{"criterion": "c", "predicate": "p", "check": "tests"}]}}
ESCALATION = {"items": [
    {"alarm": 1, "answer": "In notes.json in the current directory, as a JSON list.", "file": "jot/cli.py",
     "quote": 'STORE = Path("notes.json")'},
    {"alarm": 2, "question": "Should untagged legacy notes match a --tag filter?", "why": WHY_ONLY_IN_RECORD,
     "options": [{"label": "Never match: legacy notes read as having no tags", "consequence": "no migration; old notes vanish from filtered lists"},
                 {"label": "Migrate: add an empty tags list on first save", "consequence": "one-time rewrite of notes.json"}],
     "recommend": 1},
    {"alarm": 3, "answer": "The TagIndex class in jot/cli.py.", "file": "jot/cli.py", "quote": INVENTED},
]}
REDRAFTED = ("## Technical plan\n\nNotes live in `notes.json` (a JSON list). Each note gains an optional `tags` list. "
             "Notes saved before this change have no `tags` and are read as having no tags, so `jot list --tag T` never shows them. "
             "Tag filtering lives in `jot/cli.py`; there is no separate index.\n")
G = "Complete five separate tasks"
rules = [
    {"match": G, "text": json.dumps(NOT_READY)},                       # goldfish round 1
    {"match": "could not proceed. Its open questions", "text": json.dumps(ESCALATION)},
    {"match": "These questions are now settled", "text": REDRAFTED},
    {"match": G, "text": json.dumps(CLEAN)},                           # goldfish round 2, fresh
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
    work = Path(tempfile.mkdtemp(prefix="egm-dsh-escalate-"))
    proj = work / "jot"
    shutil.copytree(SAMPLE / "before", proj, ignore=shutil.ignore_patterns("__pycache__"))
    (proj / "docs" / "designs").mkdir(parents=True)
    doc_path = proj / "docs" / "designs" / "tags.md"
    doc_path.write_text(DOC)
    (work / "rules.json").write_text(json.dumps(rules))
    (work / "DUMP").write_text("")
    port = free_port()
    server = subprocess.Popen([sys.executable, "-I", str(HERE / "fake_deepseek.py"), str(port), str(work / "rules.json"), str(work)])
    wait_port(port)
    env = dict(os.environ, DEEPSEEK_API_KEY="fake", DEEPSEEK_BASE_URL=f"http://127.0.0.1:{port}", NO_COLOR="1")
    feature_json = proj / ".egm-dsh" / "features" / "tags.json"

    def egm(*args):
        p = subprocess.run([EGM, *args], cwd=proj, env=env, text=True, capture_output=True)
        out = (p.stdout + p.stderr).rstrip()
        print(f"$ egm-dsh {' '.join(args)}\n{out}\n", flush=True)
        return p.returncode, out

    def gate():
        return json.loads(feature_json.read_text())["gate"]

    def bodies():
        return [json.loads(l) for l in (work / "bodies.jsonl").read_text().splitlines()] if (work / "bodies.jsonl").exists() else []

    try:
        egm("init", "--agent", "dsh", "--preset", "economy", "--dsh-command", *DSH)
        egm("adopt", "tags", "--doc", "docs/designs/tags.md")
        r1 = egm("goldfish", "tags", "--no-verify")
        gate_after_review = gate()
        n_before = len(bodies())
        esc = egm("escalate", "tags")
        esc_bodies = bodies()[n_before:]
        gate_after_escalate = gate()
        rec = json.loads((proj / ".egm-dsh" / "decisions" / "tags.json").read_text())
        early_redraft = egm("redraft", "tags")
        early_build = egm("build", "tags")
        listing = egm("decide", "tags")
        other_needs_text = egm("decide", "tags", "3", "B")   # #3: [A] the unconfirmed answer, [B] Other
        answered_refused = egm("decide", "tags", "1", "A")
        d2 = egm("decide", "tags", "2", "A")
        d3 = egm("decide", "tags", "3", "--other", "Tag filtering lives in jot/cli.py; there is no separate index.")
        gate_after_decide = gate()
        doc_before = doc_path.read_text()
        red = egm("redraft", "tags")
        doc_after = doc_path.read_text()
        gate_after_redraft = gate()
        again = egm("redraft", "tags")
        build_before_pass = egm("build", "tags")
        n_before = len(bodies())
        r2 = egm("goldfish", "tags", "--no-verify")
        fresh = [b for b in bodies()[n_before:] if G in json.dumps(b.get("messages", []))]
        gate_after_pass = gate()
        review2 = json.loads((proj / ".egm-dsh" / "reviews" / "tags-2.json").read_text())
        esc_ready = egm("escalate", "tags")
        budget = egm("budget", "tags")
        ledger = [json.loads(l) for l in (proj / ".egm-dsh" / "ledger.jsonl").read_text().splitlines()]
    finally:
        server.terminate()

    items = {i["id"]: i for i in rec["items"]}
    fresh_text = json.dumps(fresh[0]) if fresh else ""
    checks = {
        "round 1 is NOT READY with three questions": r1[0] == 0 and "NOT READY" in r1[1] and "egm-dsh escalate tags" in r1[1],
        "escalate ran one read-only dsh turn": len(esc_bodies) >= 1 and all("file policy: read-only" in json.dumps(b) for b in esc_bodies),
        "#1 answered from the repository with a quote egm-dsh found": items[1]["status"] == "answered" and items[1]["answer"]["file"] == "jot/cli.py",
        "#2 escalated with labelled options, egm's Other last, recommendation kept": items[2]["status"] == "pending"
            and [o["key"] for o in items[2]["options"]] == ["A", "B", "C"] and items[2]["options"][-1]["label"].startswith("Other")
            and items[2]["recommended"] == "A" and Q2 in items[2]["question"],
        "#3's invented quote is not trusted: escalated, with the answer as an option": items[3]["status"] == "pending"
            and "could not confirm" in items[3]["why"] and items[3]["options"][0]["label"].startswith("Accept the elephant's answer"),
        "options are shown as labelled choices": "[A] Never match" in esc[1] and "[B] Migrate" in esc[1] and "(recommended)" in esc[1],
        "escalation does not move the gate": gate_after_escalate == gate_after_review and gate_after_review["phase"] == "designing",
        "redraft refused while decisions are open, naming them": early_redraft[0] != 0 and "#2, #3" in early_redraft[1],
        "build refused before any pass": early_build[0] != 0 and "no goldfish has passed" in early_build[1],
        "decide lists the decisions": "#2 ?" in listing[1],
        "the Other option needs your own text": other_needs_text[0] != 0 and "--other" in other_needs_text[1],
        "a repository-answered question cannot be overridden by decide": answered_refused[0] != 0,
        "decisions recorded": d2[0] == 0 and d3[0] == 0 and "next: egm-dsh redraft tags" in d3[1],
        "deciding does not move the gate either": gate_after_decide == gate_after_review,
        "redraft writes the decisions into the doc": red[0] == 0 and "## Decisions" in doc_after and "## Decisions" not in doc_before
            and "Settled by the repository" in doc_after and "Decided:** Never match" in doc_after
            and "Decided:** Tag filtering lives in jot/cli.py" in doc_after,
        "redraft rewrote the section the questions concern": "read as having no tags" in doc_after and "read as having no tags" not in doc_before,
        "the redraft is a new revision, back in design, with no pass": gate_after_redraft["phase"] == "designing"
            and gate_after_redraft["doc_hash"] != gate_after_review["doc_hash"] and gate_after_redraft["passed_hash"] is None,
        "decisions apply once": again[0] != 0,
        "build still refused after the redraft (gate not weakened)": build_before_pass[0] != 0 and "no goldfish has passed" in build_before_pass[1],
        "a fresh goldfish ran: a new session with one message": len(fresh) == 1 and len(fresh[0]["messages"]) == 1,
        "the fresh goldfish gets nothing from the decisions record, only the doc (which now holds them)": bool(fresh_text)
            and WHY_ONLY_IN_RECORD not in fresh_text and "could not confirm" not in fresh_text and ".egm-dsh" not in fresh_text
            and "docs/designs/tags.md" in fresh_text and review2.get("doc_hash") == f"{gate_after_redraft['doc_hash']:016x}"
            and "## Decisions" in doc_after,
        "the fresh pass on the new revision unlocks the build": r2[0] == 0 and "READY" in r2[1] and gate_after_pass["phase"] == "ready"
            and gate_after_pass["passed_hash"] == gate_after_pass["doc_hash"] == gate_after_redraft["doc_hash"],
        "escalate refused once READY": esc_ready[0] != 0,
        "escalate and redraft are charged as elephant turns": "2/40" in budget[1],
        "ledger records the escalation and the redraft": any(e["kind"] == "escalate" and e["escalated"] == 2 and e["answered_from_repo"] == 1 for e in ledger)
            and any(e["kind"] == "redraft" for e in ledger),
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
