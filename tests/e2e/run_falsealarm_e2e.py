"""End-to-end test of the false-alarm rules, on the real DeepSeek Harness binary
(model faked by fake_deepseek.py, full request bodies captured).

The same delete design doc (corpus `delete-stable-ids`, labelled ready) is reviewed
in three projects, balanced preset (parallel stages, with the verifier). In each,
the comprehension reader lists three "unclear" points and the readiness reader asks
whether `ID` is parsed as an integer (the question that blocked it on a real run).

  * A: the verifier answers `caught-by-check` naming criterion 1, which has a check:
       READY; the three comprehension points are notes; the tester's prompt then
       carries the question and its criterion
  * B: the verifier names criterion 9, which does not exist: the question stays,
       NOT READY
  * C: the critic also raises a blocking finding and the verifier answers
       `caught-by-check` for both: the question is dropped, the blocker is not,
       NOT READY
  * every project: notes never reach the verifier; the verifier's prompt lists the
       numbered criteria with their checks

usage: python3 run_falsealarm_e2e.py EGM_BINARY DSH_COMMAND...
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
CORPUS = HERE.parent.parent / "bench" / "corpus"
EGM, DSH = str(Path(sys.argv[1]).resolve()), sys.argv[2:]
DOC = "docs/designs/delete.md"
BODY = (CORPUS / "delete-stable-ids.md").read_text()
QUESTION = "Is the delete ID argument parsed as an integer, or compared as the raw string?"
NOTES = ["What test framework is used?", "What is jot and how is it installed?", "The note format is only in the linked cli.py."]
BLOCKER = "Deleting by id still lets `add` reuse a live id."


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


def review_rules(case):
    read = {"tool": "read", "args": {"file_path": DOC}}
    findings = [{"severity": "Blocker", "issue": BLOCKER, "cites": "Technical plan", "quote": "", "search_terms": ["reuse a live id"]}] if case == "C" else []
    answers = {
        "In your own words": {"problem": "Mistyped notes stay forever.", "current_mechanism": "Notes are a JSON list.",
                              "proposed_change": "A delete command.", "unclear": [{"point": n, "section": "Technical plan"} for n in NOTES]},
        "Act as an expert reviewer": {"findings": findings},
        "Zoom out and judge": {"findings": []},
        "engineer who must implement": {"ready": False, "blocking_questions": [
            {"question": QUESTION, "section": "Technical plan", "quote": "removes the note whose `id` equals ID", "search_terms": ["int(", "type=int"]}]},
        "precise predicate": {"criteria": [{"criterion": "delete 2", "predicate": "note 2 gone", "check": "tests/test_delete.py"}]},
    }
    out = []
    for marker, a in answers.items():
        out += [{"match": marker, **read}, {"match": marker, "text": json.dumps(a)}]
    # the verifier is given the question (and, in C, the blocker); its ids follow collect_alarms' order
    n = 9 if case == "B" else 1
    items = [{"id": i, "verdict": "caught-by-check", "criterion": n, "reason": "comparing the text \"2\" with the integer id fails it"}
             for i in range(1, 6)]
    out += [{"match": "You are verifying alarms", **read}, {"match": "You are verifying alarms", "text": json.dumps({"items": items})}]
    return out


def main():
    work = Path(tempfile.mkdtemp(prefix="egm-falsealarm-"))
    rules = []
    for case in "ABC":
        rules += [{"match": "Idea: Let people delete", "text": "Understood. Draft it."},
                  {"match": "Draft the whole design doc", "text": BODY}]
        rules += review_rules(case)
    rules += [{"match": "Write the acceptance tests", "text": "Wrote no tests (scripted)."}]
    (work / "rules.json").write_text(json.dumps(rules))
    (work / "DUMP").write_text("")   # the fake model saves every request body
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

    res, outs = {}, {}
    try:
        for case in "ABC":
            proj = work / case
            shutil.copytree(SAMPLE / "before", proj, ignore=shutil.ignore_patterns("__pycache__"))
            egm(proj, "init", "--agent", "dsh", "--preset", "balanced", "--dsh-command", *DSH)
            egm(proj, "set-review", "--votes", "1", "--invariants")   # as scripted: one readiness reader; run_vote_e2e.py covers the vote
            egm(proj, "elephant", "delete", "--idea", "Let people delete a mistyped note by id.")
            egm(proj, "draft", "delete")
            outs[case] = egm(proj, "goldfish", "delete")
            reviews = sorted((proj / ".egm-dsh" / "reviews").glob("delete-*.json"))
            res[case] = json.loads(reviews[-1].read_text()) if reviews else {"verdict": "missing", "alarms": []}
        n0 = len(bodies())
        outs["A-tests"] = egm(work / "A", "tests", "delete")
        tester = [b for b in bodies()[n0:] if "Write the acceptance tests" in json.dumps(b)]
    finally:
        server.terminate()

    allb = json.dumps(bodies())
    verifier = [b for b in bodies() if "You are verifying alarms" in json.dumps(b)]

    def st(case, pred):
        return [(a["status"], a.get("dropped_by")) for a in res[case]["alarms"] if pred(a)]

    is_q = lambda a: a["text"] == QUESTION
    is_note = lambda a: a["kind"] == "note"
    checks = {
        "A: READY once the verifier names a criterion that has a check": res["A"]["verdict"] == "ready",
        "A: the question is dropped as caught by criterion 1, with the check in the reason": st("A", is_q) == [("dropped", "verifier")]
            and [a.get("caught_by") for a in res["A"]["alarms"] if is_q(a)] == [1]
            and "tests/test_delete.py" in next(a["reason"] for a in res["A"]["alarms"] if is_q(a)),
        "A: the three comprehension points are notes, shown and not blocking": [a["text"] for a in res["A"]["alarms"] if is_note(a)] == NOTES
            and all(a["status"] == "open" for a in res["A"]["alarms"] if is_note(a))
            and "3 comprehension note(s)" in outs["A"][1],
        "B: a criterion that does not exist catches nothing: NOT READY, the question kept": res["B"]["verdict"] == "not-ready"
            and st("B", is_q) == [("confirmed", None)],
        "C: the question is dropped but the blocking finding is not: NOT READY": res["C"]["verdict"] == "not-ready"
            and st("C", is_q) == [("dropped", "verifier")]
            and st("C", lambda a: a["text"] == BLOCKER) == [("confirmed", None)],
        "notes never reach the verifier": bool(verifier) and not any(n in json.dumps(v) for v in verifier for n in NOTES[1:]),
        "the verifier's prompt lists the numbered criteria with their checks": bool(verifier)
            and "1. `jot delete 2` removes note 2 and leaves the others. Check: `tests/test_delete.py` (check: tests/test_delete.py)" in json.dumps(verifier[0]).replace("\\\\", "\\").replace('\\"', '"'),
        "the tester is told to cover the question with criterion 1": bool(tester) and "left these questions to the tests" in json.dumps(tester[0])
            and "criterion 1: " + QUESTION[:40] in json.dumps(tester[0]),
        "test-writing questions are left out of the readiness prompt": "which test files exist yet" in allb,
    }
    for name, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + name)
    print("SUMMARY " + json.dumps({"passed": sum(map(bool, checks.values())), "failed": sum(not v for v in checks.values())}))
    if not all(checks.values()):
        sys.exit(1)
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
