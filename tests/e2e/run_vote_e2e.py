"""End-to-end test of readiness by majority vote and the leaner default review, on the
real DeepSeek Harness binary (model faked by fake_deepseek.py; request bodies captured).

Defaults under test: 3 readiness readers vote, the display-only invariants stage is off,
every stage is asked for a brief answer. The same ready doc (corpus `search-complete`)
is reviewed in three projects; the verifier confirms every alarm, so only the vote
can drop one.

  * A: 1 of 3 readiness readers asks a question: outvoted, READY, the question kept on
       record as dropped by `vote`
  * B: 2 of 3 ask: a strict majority, NOT READY, both questions stand
  * C: no reader objects, but the critic raises a Blocker: NOT READY (the vote never
       touches a critic blocker)
  * every project: exactly 3 readiness requests, no invariants request, the brief-answer
    line in every stage prompt, all three readers' answers saved with the review

usage: python3 run_vote_e2e.py EGM_BINARY DSH_COMMAND...
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
DOC = "docs/designs/search.md"
BODY = (CORPUS / "search-complete.md").read_text()
READY = {"ready": True, "blocking_questions": []}
BLOCKER = "Search loads every note into memory."


def ask(i):
    return {"ready": False, "blocking_questions": [{"question": f"QUESTION-{i}: what does search print for a note with no text?",
                                                    "section": "Technical plan", "quote": "", "search_terms": ["empty text"]}]}


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


def rules_for(case):
    readers = {"A": [ask(1), READY, READY], "B": [ask(1), ask(2), READY], "C": [READY, READY, READY]}[case]
    findings = [{"severity": "Blocker", "issue": BLOCKER, "cites": "Risks and mitigations", "quote": "", "search_terms": ["stream"]}] if case == "C" else []
    out = [{"match": "In your own words", "text": json.dumps({"problem": "p", "current_mechanism": "c", "proposed_change": "s", "unclear": []})},
           {"match": "Act as an expert reviewer", "text": json.dumps({"findings": findings})},
           {"match": "Zoom out and judge", "tool": "read", "args": {"file_path": DOC}},   # zoom-out stays in dsh
           {"match": "Zoom out and judge", "text": json.dumps({"findings": []})}]
    out += [{"match": "engineer who must implement", "text": json.dumps(r)} for r in readers]
    out += [{"match": "You are verifying alarms", "text": json.dumps({"items": [{"id": i, "verdict": "confirmed"} for i in range(1, 9)]})}]
    return out


def main():
    work = Path(tempfile.mkdtemp(prefix="egm-vote-"))
    rules = []
    for case in "ABC":
        rules += [{"match": "Idea: Let people search", "text": "Understood. Draft it."},
                  {"match": "Draft the whole design doc", "text": BODY}]
        rules += rules_for(case)
    (work / "rules.json").write_text(json.dumps(rules))
    (work / "DUMP").write_text("")
    port = free_port()
    server = subprocess.Popen([sys.executable, "-I", str(HERE / "fake_deepseek.py"), str(port), str(work / "rules.json"), str(work)])
    wait_port(port)
    env = dict(os.environ, DEEPSEEK_API_KEY="fake", DEEPSEEK_BASE_URL=f"http://127.0.0.1:{port}", NO_COLOR="1")

    def egm(proj, *args):
        p = subprocess.run([EGM, *args], cwd=proj, env=env, text=True, capture_output=True, timeout=900)
        out = (p.stdout + p.stderr).rstrip()
        print(f"$ egm-dsh {' '.join(args)}\n{out[-2500:]}\n", flush=True)
        return p.returncode, out

    def bodies():
        f = work / "bodies.jsonl"
        return [json.loads(l) for l in f.read_text().splitlines()] if f.exists() else []

    res, outs, spans = {}, {}, {}
    try:
        for case in "ABC":
            proj = work / case
            shutil.copytree(SAMPLE / "before", proj, ignore=shutil.ignore_patterns("__pycache__"))
            egm(proj, "init", "--agent", "dsh", "--preset", "balanced", "--dsh-command", *DSH)
            egm(proj, "elephant", "search", "--idea", "Let people search notes by a word.")
            egm(proj, "draft", "search")
            b0 = len(bodies())
            outs[case] = egm(proj, "goldfish", "search")
            spans[case] = bodies()[b0:]
            res[case] = json.loads(sorted((proj / ".egm-dsh" / "reviews").glob("search-*.json"))[-1].read_text())
    finally:
        server.terminate()

    def readiness(case):
        return [(a["status"], a.get("dropped_by"), a.get("vote")) for a in res[case]["alarms"] if a["source"] == "readiness"]

    def n(case, marker):
        return sum(marker in json.dumps(b) for b in spans[case] if "You are verifying alarms" not in json.dumps(b))

    stage_prompts = [b for c in "ABC" for b in spans[c] if "You are a reviewer" in json.dumps(b) and "You are verifying alarms" not in json.dumps(b)]
    checks = {
        "A: 1 of 3 readers objects: outvoted, READY": res["A"]["verdict"] == "ready",
        "A: the outvoted question is kept on record, dropped by vote": len(readiness("A")) == 1 and readiness("A")[0][:2] == ("dropped", "vote")
            and "raised by 1 of 3 readiness readers" in next(a["reason"] for a in res["A"]["alarms"] if a["source"] == "readiness"),
        "B: 2 of 3 object: NOT READY, both questions stand": res["B"]["verdict"] == "not-ready"
            and sorted(st for st, _, _ in readiness("B")) == ["confirmed", "confirmed"] and len({v for _, _, v in readiness("B")}) == 2,
        "C: no reader objects but a critic Blocker still blocks": res["C"]["verdict"] == "not-ready" and readiness("C") == []
            and any(a["text"] == BLOCKER and a["status"] == "confirmed" for a in res["C"]["alarms"]),
        "every project: exactly 3 readiness requests": all(n(c, "engineer who must implement") == 3 for c in "ABC"),
        "every project: no invariants request (the stage is off)": all(n(c, "precise predicate") == 0 for c in "ABC"),
        "every stage prompt asks for a brief answer": bool(stage_prompts) and all("Be brief: at most 5 items" in json.dumps(b) for b in stage_prompts),
        "all three readers' answers are saved with the review": all(len(res[c]["readiness_votes"]) == 3 for c in "ABC"),
        "the review says how many readers there are (comprehension + 3 readiness)": "4 readers" in outs["A"][1],
    }
    for name, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + name)
    print("SUMMARY " + json.dumps({"passed": sum(map(bool, checks.values())), "failed": sum(not v for v in checks.values())}))
    if not all(checks.values()):
        sys.exit(1)
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
