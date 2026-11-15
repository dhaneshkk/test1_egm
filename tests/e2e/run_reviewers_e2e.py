"""End-to-end comparison of the goldfish's two clients, on the real DeepSeek Harness
binary (model faked by fake_deepseek.py, which answers instantly, so the times
measure only what egm-dsh and dsh themselves cost).

The same design doc (corpus `search-complete`, labelled ready) gets the same
scripted answers in three projects, balanced preset (parallel stages, verifier on).
Readiness asks one question; the verifier drops it as answered with a quote.

  * A: `set-reviewers dsh` — every goldfish session is a dsh reading room (the old way)
  * B: the default — reader, critic and verifier on the built-in client, zoom-out in dsh
  * C: `set-reviewers builtin --zoom-out-builtin` — no dsh process at all in the review

Checks: the same verdict and the same alarms everywhere; built-in requests carry one
user message, no tools and no system prompt, with the linked code embedded and a link
outside the project left out; B and C need fewer requests, fewer input tokens and less
time than A; the elephant still runs in dsh. Prints the three measurements.

usage: python3 run_reviewers_e2e.py EGM_BINARY DSH_COMMAND...
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
SECRET = "OUTSIDE-THE-PROJECT-SECRET"
BODY = (CORPUS / "search-complete.md").read_text().replace(
    "## Alternatives considered", "Background: [team notes](../../../../outside-notes.md).\n\n## Alternatives considered")
QUESTION = "Is the match case-insensitive on both the note text and WORD?"
QUOTE = "compared case-insensitively with `str.casefold()`"


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


def review_rules():
    """The same answers for every client. dsh sessions first read the doc (a scripted
    tool call); the built-in client skips that rule, its files are in the prompt."""
    read = {"tool": "read", "args": {"file_path": DOC}}
    answers = {
        "In your own words": {"problem": "Finding a note by topic.", "current_mechanism": "Scrolling `jot list`.",
                              "proposed_change": "A search command.", "unclear": []},
        "Act as an expert reviewer": {"findings": []},
        "Zoom out and judge": {"findings": []},
        "engineer who must implement": {"ready": False, "blocking_questions": [
            {"question": QUESTION, "section": "Technical plan", "quote": "", "search_terms": ["casefold both"]}]},
        "precise predicate": {"criteria": []},
    }
    out = []
    for marker, a in answers.items():
        out += [{"match": marker, **read}, {"match": marker, "text": json.dumps(a)}]
    items = [{"id": i, "verdict": "answered", "quote": QUOTE, "reason": ""} for i in range(1, 4)]
    out += [{"match": "You are verifying alarms", **read}, {"match": "You are verifying alarms", "text": json.dumps({"items": items})}]
    return out


def main():
    work = Path(tempfile.mkdtemp(prefix="egm-reviewers-"))
    (work / "outside-notes.md").write_text(SECRET + "\n")
    rules = []
    for _ in "ABC":
        rules += [{"match": "Idea: Let people search", "text": "Understood. Draft it."},
                  {"match": "Draft the whole design doc", "text": BODY}]
        rules += review_rules()
    (work / "rules.json").write_text(json.dumps(rules))
    (work / "DUMP").write_text("")
    port = free_port()
    server = subprocess.Popen([sys.executable, "-I", str(HERE / "fake_deepseek.py"), str(port), str(work / "rules.json"), str(work)])
    wait_port(port)
    env = dict(os.environ, DEEPSEEK_API_KEY="fake", DEEPSEEK_BASE_URL=f"http://127.0.0.1:{port}", NO_COLOR="1")

    def egm(proj, *args):
        t = time.time()
        p = subprocess.run([EGM, *args], cwd=proj, env=env, text=True, capture_output=True, timeout=900)
        out = (p.stdout + p.stderr).rstrip()
        print(f"$ egm-dsh {' '.join(args)}  ({time.time() - t:.1f}s)\n{out[-2000:]}\n", flush=True)
        return p.returncode, out, time.time() - t

    def requests():
        f = work / "requests.jsonl"
        return [json.loads(l) for l in f.read_text().splitlines()] if f.exists() else []

    def bodies():
        f = work / "bodies.jsonl"
        return [json.loads(l) for l in f.read_text().splitlines()] if f.exists() else []

    m, res, outs = {}, {}, {}
    try:
        for case in "ABC":
            proj = work / "p" / case
            proj.parent.mkdir(exist_ok=True)
            shutil.copytree(SAMPLE / "before", proj, ignore=shutil.ignore_patterns("__pycache__"))
            egm(proj, "init", "--agent", "dsh", "--preset", "balanced", "--dsh-command", *DSH)
            egm(proj, "set-review", "--votes", "1", "--invariants")   # as scripted: one readiness reader; run_vote_e2e.py covers the vote
            if case == "A":
                outs["set-A"] = egm(proj, "set-reviewers", "dsh")
            if case == "C":
                outs["set-C"] = egm(proj, "set-reviewers", "builtin", "--zoom-out-builtin")
            e0 = len(requests())
            outs[f"{case}-elephant"] = egm(proj, "elephant", "search", "--idea", "Let people search notes by a word.")
            egm(proj, "draft", "search")
            r0, b0 = len(requests()), len(bodies())
            outs[case] = egm(proj, "goldfish", "search")
            reqs, bods = requests()[r0:], bodies()[b0:]
            rows = [json.loads(l) for l in (proj / ".egm-dsh" / "usage.jsonl").read_text().splitlines()]
            g = [r for r in rows if r["role"].startswith("goldfish")]
            m[case] = {"seconds": round(outs[case][2], 2), "requests": len(reqs),
                       "dsh_requests": sum(r["format"] == "anthropic" for r in reqs),
                       "builtin_requests": sum(r["format"] == "openai" for r in reqs),
                       "input_tokens": sum(r["cache_hit"] + r["cache_miss"] for r in g),
                       "request_chars": sum(len(json.dumps(b)) for b in bods)}
            res[case] = json.loads(sorted((proj / ".egm-dsh" / "reviews").glob("search-*.json"))[-1].read_text())
            res[case]["_bodies"] = bods
            res[case]["_elephant"] = requests()[e0:r0]
        # Ctrl-C while a built-in request is stalled: egm-dsh stops at once, writes no review
        proj = work / "p" / "C"
        n_reviews = len(list((proj / ".egm-dsh" / "reviews").glob("search-*.json")))
        stall = [{"match": "You are a reviewer", "text": "{}", "delay": 60, "keep": True}]
        (work / "rules.json").write_text(json.dumps(stall))   # read only at server start: restart it
        server.terminate(); server.wait()
        server = subprocess.Popen([sys.executable, "-I", str(HERE / "fake_deepseek.py"), str(port), str(work / "rules.json"), str(work)])
        wait_port(port)
        t = time.time()
        p = subprocess.Popen([EGM, "goldfish", "search"], cwd=proj, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        time.sleep(3)
        p.send_signal(__import__("signal").SIGINT)
        try:
            out = p.communicate(timeout=20)[0]
        except subprocess.TimeoutExpired:
            p.kill(); out = p.communicate()[0] + "\n(killed: did not stop)"
        interrupt = {"exit": p.returncode, "seconds": round(time.time() - t, 1), "out": out[-400:],
                     "reviews_after": len(list((proj / ".egm-dsh" / "reviews").glob("search-*.json"))), "reviews_before": n_reviews}
        print(f"--- Ctrl-C during a stalled built-in request: {interrupt}\n", flush=True)
    finally:
        server.terminate()

    def alarms(case):
        return [(a["source"], a["kind"], a["text"], a["status"]) for a in res[case]["alarms"]]

    built = [b for b in res["B"]["_bodies"] + res["C"]["_bodies"] if b["_path"].rstrip("/").endswith("/chat/completions")]
    a_, b_, c_ = m["A"], m["B"], m["C"]
    checks = {
        "the same verdict everywhere (READY: the verifier's quote checks out)": [res[c]["verdict"] for c in "ABC"] == ["ready"] * 3,
        "the same alarms, the same statuses": alarms("A") == alarms("B") == alarms("C") and len(alarms("A")) == 1,
        "A: every goldfish request goes through dsh": a_["builtin_requests"] == 0 and a_["dsh_requests"] >= 5,
        "B: reader, critic and verifier built in, zoom-out alone in dsh": b_["builtin_requests"] == 5 and b_["dsh_requests"] >= 1
            and all("Zoom out and judge" in json.dumps(x) for x in res["B"]["_bodies"] if not x["_path"].endswith("/chat/completions")),
        "C: no dsh request at all during the review": c_["dsh_requests"] == 0 and c_["builtin_requests"] == 6,
        "built-in requests: one user message, no tools, no system prompt, thinking and effort set": bool(built) and all(
            len(b["messages"]) == 1 and b["messages"][0]["role"] == "user" and "tools" not in b and "system" not in b
            and b["thinking"]["type"] == "enabled" and b.get("reasoning_effort") in ("low", "high", "max") for b in built),
        "built-in requests embed the linked code and leave out the link outside the project": all(
            "def main" in b["messages"][0]["content"] and SECRET not in json.dumps(b) and "outside the project" in b["messages"][0]["content"]
            for b in built),
        "the elephant still runs in dsh": all(r["format"] == "anthropic" for r in res["B"]["_elephant"] + res["C"]["_elephant"]),
        "goldfish output and doctor name the client": "built-in /chat/completions client" in outs["B"][1] and "zoom-out stays in dsh" in outs["B"][1]
            and "the critic (built-in · deepseek-flash/high) and zoom-out (dsh · " in outs["B"][1]
            and "the critic and zoom-out (built-in · " in outs["C"][1]
            and "built-in /chat/completions client" not in outs["A"][1] and "zoom-out too" in outs["set-C"][1],
        "B and C take fewer requests than A": b_["requests"] < a_["requests"] and c_["requests"] < b_["requests"],
        "B and C send fewer input tokens than A": c_["input_tokens"] < b_["input_tokens"] < a_["input_tokens"],
        "B and C are faster than A": c_["seconds"] < b_["seconds"] < a_["seconds"],
        "Ctrl-C during a stalled built-in request: exit 130 within seconds, no review written": interrupt["exit"] == 130
            and interrupt["seconds"] < 10 and interrupt["reviews_after"] == interrupt["reviews_before"],
    }
    print("\nreview of one doc, balanced preset, instant fake model:")
    for c, name in zip("ABC", ["A dsh reading rooms", "B built in, zoom-out in dsh", "C all built in"]):
        print(f"  {name:30} {m[c]['seconds']:6.2f}s  {m[c]['requests']:3} requests ({m[c]['dsh_requests']} via dsh)  "
              f"{m[c]['input_tokens']:7} input tokens  {m[c]['request_chars']:8} request bytes")
    for name, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + name)
    print("SUMMARY " + json.dumps({"passed": sum(map(bool, checks.values())), "failed": sum(not v for v in checks.values()), "measured": m}))
    if not all(checks.values()):
        sys.exit(1)
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
