"""End-to-end test: egm driving the real DeepSeek Harness binary.

The model is faked (fake_deepseek.py) so the test needs no API key, but every
other piece is real: dsh's headless profile, its tools, its sandbox, its
session store. The script walks the whole Elephant-Goldfish loop on the jot
sample and checks what dsh actually did.

With `--reviewers builtin` the read-only goldfish roles run on egm-dsh's built-in
/chat/completions client (zoom-out stays in dsh); the default, `dsh`, keeps every
goldfish in a dsh reading room, so both paths stay tested.

usage: python3 run_dsh_e2e.py EGM_BINARY [--preset balanced|economy|quality] [--reviewers dsh|builtin] DSH_COMMAND...
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
EGM, ARGS = sys.argv[1], sys.argv[2:]
PRESET = "balanced"
if ARGS[:1] == ["--preset"]:
    PRESET, ARGS = ARGS[1], ARGS[2:]
REVIEWERS = "dsh"
if ARGS[:1] == ["--reviewers"]:
    REVIEWERS, ARGS = ARGS[1], ARGS[2:]
BUILTIN = REVIEWERS == "builtin"
DSH = ARGS
COMBINED = PRESET == "economy"
VERIFY = PRESET != "quality"   # quality runs without the verifier (Pro goldfish)
NOISY = VERIFY                 # the scripted small model raises two false alarms in round 2

# What each preset must send to the model, per role: (model, effort or None for thinking off, lean tools).
EXPECT = {
    "balanced": {"elephant": ("deepseek-v4-pro", "high", True), "goldfish": ("deepseek-flash", "low", True),
                 "goldfish-critic": ("deepseek-flash", "high", True), "goldfish-verifier": ("deepseek-v4-pro", "low", True),
                 "builder": ("deepseek-v4-pro", "high", False),
                 "reviewer": ("deepseek-flash", "high", True)},
    "economy": {"elephant": ("deepseek-flash", "high", True), "goldfish": ("deepseek-flash", None, True),
                "goldfish-verifier": ("deepseek-flash", "high", True),
                "builder": ("deepseek-flash", "high", False), "reviewer": ("deepseek-flash", "low", True)},
    # goldfish always run in the reading room with read-only (lean) tools, whatever the preset
    "quality": {"elephant": ("deepseek-v4-pro", "high", False), "goldfish": ("deepseek-v4-pro", "high", True),
                "goldfish-critic": ("deepseek-v4-pro", "max", True), "builder": ("deepseek-v4-pro", "high", False),
                "reviewer": ("deepseek-v4-pro", "high", False)},
}[PRESET]
PRICES = {"deepseek-flash": (0.006, 0.30, 1.20), "deepseek-v4-pro": (0.044, 1.32, 3.96)}  # USD/1M, peak

DOC = "docs/designs/jot-tags.md"
PLAN = ("Each note gains a `tags` list, stored in `notes.json` next to `id` and `text`. `jot add` accepts a repeatable "
        "`--tag` option; `jot list --tag NAME` shows only notes carrying that tag. Filtering lives in "
        "`filter_by_tag(notes, tag)` in `jot/cli.py`. Matching is exact and case-sensitive.")
COMPAT = ("\n\nNotes saved before this change have no `tags` key. Readers treat a missing key as an empty list, "
          "so old notes still list and never match a filter. jot never rewrites old notes, so no migration is needed.")


def section(name, body):
    return {"text": f"Here is the section.\n\n## {name}\n\n{body}"}


REAL_QUOTE = "stored in `notes.json` next to `id` and `text`"
OLD_DATA = ["existing", "saved before"]


def gold(round_no):
    """Goldfish replies. Round 1: one real gap, raised twice (question + blocking finding).
    Round 2 (doc fixed): either clean, or, for small-model presets, two false alarms:
    one the doc already answers and one resting on a quote the doc does not contain."""
    read = {"tool": "read", "args": {"file_path": DOC}}
    if round_no == 1:
        ready = False
        questions = [{"question": "What happens to notes saved before tags existed?", "section": "Technical plan", "search_terms": OLD_DATA}]
        findings = [{"severity": "blocking", "issue": "Stored format changes with no rule for notes saved earlier.", "cites": "Technical plan",
                     "quote": REAL_QUOTE, "search_terms": OLD_DATA}]
    elif NOISY:
        ready = False
        questions = [{"question": "Does list --tag match case-insensitively?", "section": "## Technical plan", "search_terms": ["case-sensitive"]}]
        findings = [{"severity": "blocking", "issue": "Tags are lowercased before saving, so spellings collide.", "cites": "Technical plan",
                     "quote": "tags are lowercased before saving", "search_terms": []},
                    {"severity": "minor", "issue": "Consider a hint when a filter matches nothing.", "cites": "Technical plan"}]
    else:
        ready, questions = True, []
        findings = [{"severity": "minor", "issue": "Consider a hint when a filter matches nothing.", "cites": "Technical plan"}]
    answers = {
        "comprehension": {"problem": "Users cannot group notes.", "current_mechanism": "Notes are a JSON list.", "proposed_change": "Add tags and a tag filter.", "unclear": []},
        "critic": {"findings": findings},
        # Upstream zoom-out persona: a Major (non-blocking) finding in round 1, nothing in round 2.
        "zoom_out": {"findings": [{"severity": "Major", "issue": "Showing tags in every list line may be more than the problem needs.", "cites": "Technical plan"}] if round_no == 1 else []},
        "readiness": {"ready": ready, "blocking_questions": questions},
        "invariants": {"criteria": [{"criterion": "tag filter", "predicate": "list --tag work shows only work notes", "check": "tests/test_tags.py"}]},
    }
    if COMBINED:
        out = [{"match": "Complete five separate tasks", **read}, {"match": "Complete five separate tasks", "text": json.dumps(answers)}]
    else:
        out = [
            {"match": "In your own words", **read},
            {"match": "In your own words", "text": json.dumps(answers["comprehension"])},
            {"match": "Act as an expert reviewer", **read},
            {"match": "Act as an expert reviewer", "text": "```json\n" + json.dumps(answers["critic"]) + "\n```"},
            {"match": "Zoom out and judge", **read},
            {"match": "Zoom out and judge", "text": json.dumps(answers["zoom_out"])},
            {"match": "engineer who must implement", **read},
            {"match": "engineer who must implement", "text": json.dumps(answers["readiness"])},
            {"match": "precise predicate", **read},
            {"match": "precise predicate", "text": json.dumps(answers["invariants"])},
        ]
    if VERIFY and round_no == 1:      # both alarms reach the verifier, which confirms them
        verdicts = [{"id": 1, "verdict": "confirmed", "quote": "", "reason": "the doc is silent on old notes"},
                    {"id": 2, "verdict": "confirmed", "quote": "", "reason": "same gap"}]
        out += [{"match": "You are verifying alarms", **read}, {"match": "You are verifying alarms", "text": json.dumps({"items": verdicts})}]
    if VERIFY and round_no == 2:      # the fabricated quote never reaches it; it answers the other from the doc
        verdicts = [{"id": 1, "verdict": "answered", "quote": "Matching is exact and case-sensitive.", "reason": ""}]
        out += [{"match": "You are verifying alarms", **read}, {"match": "You are verifying alarms", "text": json.dumps({"items": verdicts})}]
    return out


rules = [
    {"match": "Idea: Let people tag", "tool": "read", "args": {"file_path": "jot/cli.py"}},
    {"match": "Idea: Let people tag", "tool": "write", "args": {"file_path": "sneaky.txt", "content": "the elephant must not write"}},
    {"match": "Idea: Let people tag", "text": "jot keeps notes as a JSON list in notes.json.\n1. Where are tags stored?\n2. What about notes saved before tags existed?"},
    {"match": "Store tags on each note", "text": "Noted. How will you test the filter?"},
    # the whole doc in one elephant turn (the reply wraps it in a markdown fence, as real models often do)
    {"match": "Draft the whole design doc", "text": "Here is the design doc.\n\n```markdown" + "\n\n## Problem\n\n" + "People keep work and personal notes together and cannot list just one kind. They want to label notes and filter by label." + "\n\n## Goals and non-goals\n\n" + "Goals: tag notes; list notes by one tag.\n\nNon-goals: renaming tags, multi-tag filters." + "\n\n## Technical plan\n\n" + PLAN + "\n\n## Risks and mitigations\n\n" + "**A typo splits a tag.** Mitigation: `jot list` prints each note's tags." + "\n\n## Alternatives considered\n\n" + "**A separate tag index file.** Rejected: two files can disagree.\n\n**Hashtags in note text.** Rejected: ambiguous to parse." + "\n\n## Implementation map\n\n" + "- `jot/cli.py` — tags on notes, `--tag` options, `filter_by_tag`.\n- `tests/test_tags.py` — tests for tagging and filtering." + "\n\n## Acceptance criteria\n\n" + "- `jot list --tag work` prints only notes tagged work. Check: `tests/test_tags.py`" + "\n```"},
    *gold(1),
    {"match": "Notes saved before this change", "text": "Good: that closes the gap. Redraft the plan."},
    {"match": "Draft the `## Technical plan`", **section("Technical plan", PLAN + COMPAT)},
    *gold(2),
    {"match": "Implement the feature", "tool": "read", "args": {"file_path": DOC}},
    {"match": "Implement the feature", "tool": "read", "args": {"file_path": "jot/cli.py"}},
    {"match": "Implement the feature", "tool": "write", "args": {"file_path": "jot/cli.py", "content": (SAMPLE / "after/jot/cli.py").read_text()}},
    {"match": "Implement the feature", "tool": "write", "args": {"file_path": "tests/test_tags.py", "content": (SAMPLE / "after/tests/test_tags.py").read_text()}},
    {"match": "Implement the feature", "tool": "bash", "args": {"command": "python3 -m unittest discover -s tests -t . -q", "description": "Run the unit tests"}},
    {"match": "Implement the feature", "text": "Added tags, `--tag` options, `filter_by_tag`, and tests/test_tags.py. Tests pass."},
    {"match": "Assume the code", "tool": "read", "args": {"file_path": "jot/cli.py"}},
    {"match": "Assume the code", "text": json.dumps({"findings": [{"severity": "minor", "file": "jot/cli.py", "issue": "main() has no comments."}]})},
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
    work = Path(tempfile.mkdtemp(prefix="egm-dsh-e2e-"))
    proj = work / "jot"
    shutil.copytree(SAMPLE / "before", proj)
    (work / "rules.json").write_text(json.dumps(rules))
    port = free_port()
    server = subprocess.Popen([sys.executable, "-I", str(HERE / "fake_deepseek.py"), str(port), str(work / "rules.json"), str(work)])
    wait_port(port)
    env = dict(os.environ, DEEPSEEK_API_KEY="fake", DEEPSEEK_BASE_URL=f"http://127.0.0.1:{port}", NO_COLOR="1")

    def egm(*args, ok=True):
        print(f"\n$ egm {' '.join(args)}", flush=True)
        r = subprocess.run([EGM, *args], cwd=proj, env=env, text=True, capture_output=True)
        print((r.stdout + r.stderr).rstrip())
        if ok and r.returncode != 0:
            raise SystemExit(f"FAILED: egm {' '.join(args)} exited {r.returncode}")
        return r

    try:
        egm("init", "--agent", "dsh", "--preset", PRESET, "--dsh-command", *DSH, "--acceptance", "python3", "-m", "unittest", "discover", "-s", "tests", "-t", ".", "-q")
        egm("set-reviewers", REVIEWERS)
        egm("set-review", "--votes", "1", "--invariants")   # one readiness reader and the invariants stage, as these scripts were written; run_vote_e2e.py covers the vote
        egm("elephant", "jot-tags", "--idea", "Let people tag notes and list only the notes with a given tag.")
        egm("elephant", "jot-tags", "--say", "Store tags on each note in notes.json as a list of strings.")
        egm("draft", "jot-tags")
        r = egm("goldfish", "jot-tags"); assert "NOT READY" in r.stdout + r.stderr
        assert "refused" in egm("build", "jot-tags", ok=False).stderr
        egm("elephant", "jot-tags", "--say", "Notes saved before this change have no tags key; treat it as empty, never rewrite old notes.")
        egm("draft", "jot-tags", "--section", "Technical plan")
        r = egm("goldfish", "jot-tags"); assert "verdict: READY" in r.stdout
        egm("build", "jot-tags")
        egm("review", "jot-tags")
        egm("drift")
        egm("status")
        usage_out = egm("usage").stdout

        feature = json.loads((proj / ".egm-dsh/features/jot-tags.json").read_text())
        log = [json.loads(l) for l in (work / "requests.jsonl").read_text().splitlines()]
        model = [r for r in log if not r["title"]]
        def role_of(r):
            t = r["task"]
            if r.get("verify"): return "goldfish-verifier"
            if "Implement the feature" in t: return "builder"
            if "Assume the code" in t: return "reviewer"
            if "You are a reviewer" in t:
                if "Zoom out and judge" in t and not COMBINED: return "goldfish-critic"   # zoom-out runs on the critic's agent
                return "goldfish-critic" if "Act as an expert reviewer" in t and not COMBINED else "goldfish"
            return "elephant"
        # The logged task is truncated, so classify goldfish stages from the full rule match instead.
        for r in model:
            r["role"] = role_of(r)
        def matches(rows, spec):
            mdl, effort, lean = spec
            return bool(rows) and all(
                r["model"] == mdl and (r["effort"] == effort if effort else r["thinking"] == "disabled")
                and (len(r["tools"]) <= 7) == lean for r in rows)
        role_ok = {}
        for role, spec in EXPECT.items():
            if role == "goldfish-critic":
                continue
            rows = [r for r in model if r["role"] == role]
            if role == "goldfish" and "goldfish-critic" in EXPECT:
                # Stages share one prompt prefix; tell the critic apart by the effort its patch requested.
                crit_spec = EXPECT["goldfish-critic"]
                if crit_spec[1] != spec[1]:
                    role_ok["goldfish-critic"] = matches([r for r in rows if r["effort"] == crit_spec[1]], crit_spec)
                    rows = [r for r in rows if r["effort"] != crit_spec[1]]
            role_ok[role] = matches(rows, spec)
        usage_rows = [json.loads(l) for l in (proj / ".egm-dsh/usage.jsonl").read_text().splitlines()]
        r1 = json.loads((proj / ".egm-dsh/reviews/jot-tags-1.json").read_text())
        r2 = json.loads((proj / ".egm-dsh/reviews/jot-tags-2.json").read_text())
        status = lambda r: sorted((a["status"], a.get("dropped_by") or "") for a in r["alarms"])
        verifier_calls = [r for r in model if r["role"] == "goldfish-verifier"]
        if VERIFY:
            mitigation = {
                "round 1: the real gap is confirmed by the verifier": r1["verdict"] == "not-ready" and status(r1) == [("confirmed", ""), ("confirmed", "")],
                "round 2: fabricated quote dropped by grounding, answered alarm dropped by the verifier":
                    r2["verdict"] == "ready" and status(r2) == [("dropped", "grounding"), ("dropped", "verifier")],
                "verifier ran once per round with alarms (read + answer; built in: the answer alone)": len(verifier_calls) == (2 if BUILTIN else 4),
                "the fabricated-quote alarm never reached the verifier": all("lowercased" not in r["task"] for r in verifier_calls),
            }
        else:
            mitigation = {
                "verifier off: no verifier requests": not verifier_calls,
                "round 1 alarms stand unverified": r1["verdict"] == "not-ready" and all(a["status"] == "open" for a in r1["alarms"]),
            }
        checks = {
            "feature reached hardened": feature["gate"]["phase"] == "hardened",
            "elephant session id came from dsh": str(feature["elephant_session"]).startswith("session-"),
            "elephant could not write (sandbox)": not (proj / "sneaky.txt").exists(),
            "elephant follow-ups resumed the session": any(r["messages"] > 1 and "Store tags" in r["task"] for r in model),
            "design/review turns ran read-only (dsh's read-only mode, or the built-in client: no tools at all)": all(
                r["permission"] == "read-only" or (r.get("format") == "openai" and not r["tools"]) for r in model if "Implement the feature" not in r["task"]),
            # built in: every goldfish request but zoom-out (one dsh session per round, with a read) uses the client
            "goldfish requests used the client asked for": (
                sum(r.get("format") == "openai" for r in model if r["role"].startswith("goldfish")) > 0
                and sum(r.get("format") != "openai" for r in model if r["role"].startswith("goldfish")) <= (0 if COMBINED else 4)
            ) if BUILTIN else not any(r.get("format") == "openai" for r in model),
            "every model request matched a scripted reply": not any(r.get("unmatched") for r in model)
                and (BUILTIN or len(model) == len(rules)),
            "tags code is in place": "filter_by_tag" in (proj / "jot/cli.py").read_text(),
            "no session-title side requests": not any(r["title"] for r in log),
            **{f"{role} runs on the preset's model/effort/tools": ok for role, ok in role_ok.items()},
            "usage log records cache hits": sum(u["cache_hit"] for u in usage_rows) > 0,
            "egm-dsh usage prints a cost": "TOTAL" in usage_out and "$" in usage_out,
            **mitigation,
            "zoom-out stage (upstream Skeptical Architect) reported its Major finding without blocking":
                (r1.get("zoom_out") or {}).get("findings", [{}])[0].get("severity") == "Major"
                and not any(a["source"] == "zoom-out" for a in r1["alarms"]),
            "design doc has the upstream Goals and Risks sections":
                all(h in (proj / DOC).read_text() for h in ["## Goals and non-goals", "## Risks and mitigations"]),
        }
        hit = sum(r["hit"] for r in model); miss = sum(r["miss"] for r in model)
        cost = sum((r["hit"] * PRICES[r["model"]][0] + r["miss"] * PRICES[r["model"]][1]) / 1e6 for r in model)
        by_role = {}
        for r in model:
            b = by_role.setdefault(r["role"], {"requests": 0, "input": 0, "tool_schema_tokens": 0})
            b["requests"] += 1; b["input"] += r["hit"] + r["miss"]; b["tool_schema_tokens"] += r["chars"]["tools"] // 4
        print("SUMMARY " + json.dumps({"preset": PRESET, "requests": len(model), "cache_hit": hit, "cache_miss": miss,
                                       "input_cost_usd_peak": round(cost, 6), "by_role": by_role}))
        print("\n== checks")
        for name, passed in checks.items():
            print(f"{'PASS' if passed else 'FAIL'}  {name}")
        print(f"model requests: {len(model)}  work dir: {work}")
        if not all(checks.values()):
            raise SystemExit(1)
    finally:
        server.terminate()


if __name__ == "__main__":
    main()
