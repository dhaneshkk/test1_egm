"""End-to-end test of the token reductions on the real DeepSeek Harness binary
(model faked by fake_deepseek.py; every model request is logged and counted).

  * `goldfish --fail-fast` on a doc egm-dsh's own checks already reject calls no model
  * on a doc whose readiness stage asks a question, it runs readiness alone
  * on a clean doc it runs every stage and is READY, like the full review
  * the same docs under the full review, for the request and token counts saved
  * `estimate` predicts the sessions exactly and the tokens from recorded usage
  * `set-ceiling` refuses a round before it starts: no request, gate unchanged
  * `loop` is fail-fast by default

Prints a savings table (requests and tokens, full vs fail-fast) at the end.

usage: python3 run_tokens_e2e.py EGM_BINARY DSH_COMMAND...
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

FULL_DOC = """# Design: Tags

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
# No Risks section and a criterion without a check: egm-dsh's own checks reject it.
GAPPY_DOC = FULL_DOC.replace("## Risks and mitigations\n\n**Small.** Tests.\n\n", "").replace(" Check: `tests/test_tags.py`", "")

COMP, CRIT, ZOOM, READY, INV = ("In your own words, state the problem", "Act as an expert reviewer", "Zoom out and judge",
                                "You are the engineer who must implement", "For each item under 'Acceptance criteria'")
ANS = {
    COMP: {"problem": "p", "current_mechanism": "m", "proposed_change": "c", "unclear": []},
    CRIT: {"findings": []},
    ZOOM: {"findings": []},
    INV: {"criteria": [{"criterion": "c", "predicate": "p", "check": "tests/test_tags.py"}]},
}
CLEAN_READY = {"ready": True, "blocking_questions": []}
ASKS = {"ready": False, "blocking_questions": [{"question": "Is tag matching case-sensitive?", "section": "Technical plan", "search_terms": ["case"]}]}


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


def main():
    work = Path(tempfile.mkdtemp(prefix="egm-dsh-tokens-"))
    rules = [{"match": m, "keep": True, "text": json.dumps(a)} for m, a in ANS.items()]
    # readiness answers in order: the first two calls (the full and fail-fast reviews of case 2) ask; every later call is clean
    rules += [{"match": READY, "text": json.dumps(ASKS)}, {"match": READY, "text": json.dumps(ASKS)}]
    rules += [{"match": READY, "keep": True, "text": json.dumps(CLEAN_READY)}]
    (work / "rules.json").write_text(json.dumps(rules))
    port = free_port()
    server = subprocess.Popen([sys.executable, "-I", str(HERE / "fake_deepseek.py"), str(port), str(work / "rules.json"), str(work)])
    wait_port(port)
    env = dict(os.environ, DEEPSEEK_API_KEY="fake", DEEPSEEK_BASE_URL=f"http://127.0.0.1:{port}", NO_COLOR="1")

    def requests():
        f = work / "requests.jsonl"
        return len(f.read_text().splitlines()) if f.exists() else 0

    projects = {}

    def project(name, doc):
        proj = work / name
        shutil.copytree(SAMPLE / "before", proj, ignore=shutil.ignore_patterns("__pycache__"))
        (proj / "docs" / "designs").mkdir(parents=True)
        (proj / "docs" / "designs" / "tags.md").write_text(doc)
        egm(proj, "init", "--agent", "dsh", "--preset", "economy", "--dsh-command", *DSH)
        egm(proj, "set-review", "--votes", "1", "--invariants")   # as written: one readiness reader, 5 stages; run_vote_e2e.py covers the vote
        cfg = json.loads((proj / ".egm-dsh" / "config.json").read_text())
        cfg["goldfish_mode"] = "parallel"
        (proj / ".egm-dsh" / "config.json").write_text(json.dumps(cfg, indent=1))
        egm(proj, "adopt", "tags", "--doc", "docs/designs/tags.md")
        projects[name] = proj
        return proj

    def egm(proj, *args):
        p = subprocess.run([EGM, *args], cwd=proj, env=env, text=True, capture_output=True, timeout=600)
        out = (p.stdout + p.stderr).rstrip()
        print(f"$ egm-dsh {' '.join(args)}\n{out[-2000:]}\n", flush=True)
        return p.returncode, out

    def measured(proj, *args):
        r0 = requests()
        rc, out = egm(proj, *args)
        return rc, out, requests() - r0

    def last_review(proj):
        f = json.loads((proj / ".egm-dsh" / "features" / "tags.json").read_text())
        return json.loads((proj / ".egm-dsh" / "reviews" / f"tags-{f['goldfish_runs']}.json").read_text())

    def phase(proj):
        return json.loads((proj / ".egm-dsh" / "features" / "tags.json").read_text())["gate"]["phase"]

    def tokens(proj):
        f = proj / ".egm-dsh" / "usage.jsonl"
        return sum(r["cache_hit"] + r["cache_miss"] + r["output"] for r in map(json.loads, f.read_text().splitlines())) if f.exists() else 0

    rows = {}
    try:
        # 2. a complete doc whose readiness stage asks a question (rules: first two readiness answers ask)
        q_full, q_ff = project("asks-full", FULL_DOC), project("asks-ff", FULL_DOC)
        est_before = egm(q_full, "estimate", "tags", "--no-verify")
        c = measured(q_full, "goldfish", "tags", "--no-verify")
        d = measured(q_ff, "goldfish", "tags", "--no-verify", "--fail-fast")
        rows["readiness asks a question"] = (c[2], tokens(q_full), d[2], tokens(q_ff))
        rv_q_full, rv_q_ff = last_review(q_full), last_review(q_ff)

        # 1. a doc egm-dsh's own checks reject
        g_full, g_ff = project("gappy-full", GAPPY_DOC), project("gappy-ff", GAPPY_DOC)
        est_gappy = egm(g_ff, "estimate", "tags", "--fail-fast", "--no-verify")
        a = measured(g_full, "goldfish", "tags", "--no-verify")
        b = measured(g_ff, "goldfish", "tags", "--no-verify", "--fail-fast")
        rows["doc missing a section"] = (a[2], tokens(g_full), b[2], tokens(g_ff))
        rv_gap_full, rv_gap_ff = last_review(g_full), last_review(g_ff)

        # 3. the same docs, now clean: fail-fast runs everything and is READY, like the full review
        est_after = egm(q_full, "estimate", "tags", "--no-verify")
        t0 = tokens(q_full)
        e = measured(q_full, "goldfish", "tags", "--no-verify")
        actual_round = tokens(q_full) - t0
        t1 = tokens(q_ff)
        f_ = measured(q_ff, "goldfish", "tags", "--no-verify", "--fail-fast")
        rows["clean doc (READY)"] = (e[2], actual_round, f_[2], tokens(q_ff) - t1)
        rv_clean_full, rv_clean_ff = last_review(q_full), last_review(q_ff)

        # 4. the ceiling: a round that would cross it is refused before any request
        c_proj = project("ceiling", FULL_DOC)
        egm(c_proj, "goldfish", "tags", "--no-verify")
        used = tokens(c_proj)
        egm(c_proj, "set-ceiling", str(used + 10))
        Path(c_proj / "docs" / "designs" / "tags.md").write_text(FULL_DOC + "\nMore detail.\n")
        ph_before = phase(c_proj)
        refused = measured(c_proj, "goldfish", "tags", "--no-verify")
        ph_after = phase(c_proj)
        egm(c_proj, "set-ceiling", "--none")

        # 5. loop is fail-fast by default: on the gappy doc its review calls no model (escalate then asks the elephant)
        l_proj = project("loop", GAPPY_DOC)
        lp = measured(l_proj, "loop", "tags", "--max-rounds", "1", "--no-verify")
        rv_loop = last_review(l_proj)
    finally:
        server.terminate()

    def est_total(out):
        line = [l for l in out.splitlines() if l.strip().startswith("total")]
        return float(line[-1].split()[-1]) if line else -1

    def est_sessions(out):
        line = [l for l in out.splitlines() if l.strip().startswith("total")]
        return int(line[-1].split()[1]) if line else -1

    pred, err = est_total(est_after[1]), None
    if actual_round > 0 and pred > 0:
        err = abs(pred - actual_round) / actual_round
    checks = {
        "fail-fast on a gappy doc: NOT READY with no model request": b[2] == 0 and rv_gap_ff["verdict"] == "not-ready"
            and rv_gap_ff["stopped_at"] == "precheck" and len(rv_gap_ff["skipped"]) == 5,
        "the full review of the gappy doc is NOT READY too (same verdict, 5 sessions)": rv_gap_full["verdict"] == "not-ready" and a[2] >= 5,
        "the precheck stop names the same egm-dsh gaps as the full review": sorted(x["text"] for x in rv_gap_ff["alarms"])
            == sorted(x["text"] for x in rv_gap_full["alarms"] if x["source"] == "egm"),
        "fail-fast on a readiness question: readiness alone, NOT READY": rv_q_ff["stopped_at"] == "readiness" and rv_q_ff["verdict"] == "not-ready"
            and [c["stage"] for c in rv_q_ff["calls"]] == ["readiness"] and d[2] < c[2],
        "the full review agrees (NOT READY on the same question)": rv_q_full["verdict"] == "not-ready"
            and any("case-sensitive" in x["text"] for x in rv_q_full["alarms"]),
        "clean doc: fail-fast runs every stage and is READY like the full review": rv_clean_ff["verdict"] == rv_clean_full["verdict"] == "ready"
            and rv_clean_ff["skipped"] == [] and rv_clean_ff["stopped_at"] is None and phase(q_ff) == "ready",
        "estimate: precheck gap predicts 0 sessions": est_sessions(est_gappy[1]) == 0 and "calls no model" in est_gappy[1],
        "estimate: 5 sessions per parallel round without the verifier": est_sessions(est_before[1]) == 5 and est_sessions(est_after[1]) == 5,
        "estimate: heuristic before any record, recorded usage after": "heuristic" in est_before[1] and "recorded call(s)" in est_after[1],
        "estimate from recorded usage is within 25% of the next round's actual tokens": err is not None and err <= 0.25,
        "ceiling: the round is refused before any request, the gate unchanged": refused[0] != 0 and refused[2] == 0
            and "refused before starting" in refused[1] and ph_before == ph_after,
        "loop is fail-fast by default (its review called no model; only the escalate turn did)": rv_loop["stopped_at"] == "precheck"
            and rv_loop["calls"] == [] and lp[2] >= 1,
    }
    print("\nsavings (requests to the model, tokens reported):")
    print(f"  {'case':<28} {'full req':>8} {'full tok':>9} {'ff req':>7} {'ff tok':>8}")
    for k, (fr, ft, xr, xt) in rows.items():
        print(f"  {k:<28} {fr:>8} {ft:>9} {xr:>7} {xt:>8}")
    print(f"  estimate for the clean round: {pred:.0f} predicted vs {actual_round} actual tokens"
          + (f" ({err:.0%} off)" if err is not None else ""))
    print()
    for name, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + name)
    print("SUMMARY " + json.dumps({"passed": sum(map(bool, checks.values())), "failed": sum(not v for v in checks.values()),
                                   "savings": rows, "estimate_error": err}))
    if not all(checks.values()):
        sys.exit(1)
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
