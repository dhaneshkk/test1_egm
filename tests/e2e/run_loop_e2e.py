"""End-to-end test of `egm-dsh loop`, the Verification checklist and the builder as a
goldfish, on the real DeepSeek Harness binary (model faked by fake_deepseek.py).

  loop      round 1's question is answered from the repository (quote checked), so the
            loop redrafts on its own; round 2's question is a real decision, so it stops
            for the person; after `decide`, `loop` again writes the decision in and a
            fresh goldfish passes. The loop never decides and never builds.
  stall     the same two questions come back after a redraft: the loop stops instead of
            spinning, and the gate stays shut.
  checklist with `set-verification --require mcc,sam`, a doc the models call ready is
            still NOT READY until its `## Verification` section defines both; those
            alarms are egm-dsh's own (a model cannot drop them).
  builder   a builder that had to decide something the doc left open rejects the build:
            the design reopens, the review is the builder's, and `escalate` takes it from
            there; a builder with only non-blocking assumptions builds.

usage: python3 run_loop_e2e.py EGM_BINARY DSH_COMMAND...
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

- Tags filter. Check: manual — the scripted model writes no tests
"""
G = "Complete five separate tasks"
E = "could not proceed. Its open questions"
R = "These questions are now settled"
B = "Implement the feature"
Q_REPO = "Where are notes persisted?"
Q_DECIDE = "Should notes saved before tags existed match a --tag filter?"
Q_OTHER = "What is the maximum number of tags per note?"


def answers(questions=(), ready=None):
    ready = not questions if ready is None else ready
    return json.dumps({"comprehension": {"problem": "p", "current_mechanism": "m", "proposed_change": "c", "unclear": []},
                       "critic": {"findings": []}, "zoom_out": {"findings": []},
                       "readiness": {"ready": ready, "blocking_questions": [{"question": q, "section": "Technical plan"} for q in questions]},
                       "invariants": {"criteria": [{"criterion": "c", "predicate": "p", "check": "tests"}]}})


REPO_ANSWER = {"answer": "In notes.json, as a JSON list.", "file": "jot/cli.py", "quote": 'STORE = Path("notes.json")'}
PLAN_V2 = "## Technical plan\n\nNotes gain `tags`. `jot list --tag T` filters. Notes are stored in notes.json as a JSON list.\n"
PLAN_V3 = PLAN_V2 + "\nNotes saved before tags existed never match a tag filter.\n"


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


class Project:
    def __init__(self, name, rules, preset="economy", git=False):
        self.work = Path(tempfile.mkdtemp(prefix=f"egm-dsh-loop-{name}-"))
        self.proj = self.work / "jot"
        shutil.copytree(SAMPLE / "before", self.proj, ignore=shutil.ignore_patterns("__pycache__"))
        (self.proj / "docs" / "designs").mkdir(parents=True)
        (self.proj / "docs" / "designs" / "tags.md").write_text(DOC)
        if git:   # so a rejected build is reverted from git
            for c in (["init", "-q", "-b", "main"], ["add", "-A"], ["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "start"]):
                subprocess.run(["git", *c], cwd=self.proj, check=True)
        (self.work / "rules.json").write_text(json.dumps(rules))
        port = free_port()
        self.server = subprocess.Popen([sys.executable, "-I", str(HERE / "fake_deepseek.py"), str(port), str(self.work / "rules.json"), str(self.work)])
        wait_port(port)
        self.env = dict(os.environ, DEEPSEEK_API_KEY="fake", DEEPSEEK_BASE_URL=f"http://127.0.0.1:{port}", NO_COLOR="1")
        self.egm("init", "--agent", "dsh", "--preset", preset, "--dsh-command", *DSH, "--acceptance", "true")
        self.egm("set-review", "--votes", "1", "--invariants")   # as scripted: one readiness reader; run_vote_e2e.py covers the vote
        self.egm("adopt", "tags", "--doc", "docs/designs/tags.md")

    def egm(self, *args):
        p = subprocess.run([EGM, *args], cwd=self.proj, env=self.env, text=True, capture_output=True)
        out = (p.stdout + p.stderr).rstrip()
        print(f"$ egm-dsh {' '.join(args)}\n{out}\n", flush=True)
        return p.returncode, out

    def gate(self):
        return json.loads((self.proj / ".egm-dsh/features/tags.json").read_text())["gate"]

    def feature(self):
        return json.loads((self.proj / ".egm-dsh/features/tags.json").read_text())

    def review(self, n):
        return json.loads((self.proj / f".egm-dsh/reviews/tags-{n}.json").read_text())

    def doc(self):
        return (self.proj / "docs/designs/tags.md").read_text()

    def unused_rules(self):
        log = [json.loads(l) for l in (self.work / "requests.jsonl").read_text().splitlines()]
        return len([r for r in log if not r["title"] and r["reply"] == "text" and False])

    def close(self):
        self.server.terminate()


def main():
    checks = {}

    # --- loop: repository answer → auto redraft; decision → stop; decide → resume → READY
    lp = Project("loop", [
        {"match": G, "text": answers([Q_REPO])},
        {"match": E, "text": json.dumps({"items": [{"alarm": 1, **REPO_ANSWER}]})},
        {"match": R, "text": PLAN_V2},
        {"match": G, "text": answers([Q_DECIDE])},
        {"match": E, "text": json.dumps({"items": [{"alarm": 1, "question": Q_DECIDE, "why": "a product call",
                                                    "options": [{"label": "Never match", "consequence": "no migration"},
                                                                {"label": "Migrate old notes", "consequence": "rewrite"}]}]})},
        {"match": R, "text": PLAN_V3},
        {"match": G, "text": answers()},
    ])
    try:
        first = lp.egm("loop", "tags", "--no-verify")
        doc_after_first = lp.doc()
        gate_after_first = lp.gate()
        decided = lp.egm("decide", "tags", "1", "A")
        second = lp.egm("loop", "tags", "--no-verify")
        gate_final, doc_final = lp.gate(), lp.doc()
        built = lp.feature().get("built_doc_hash")
        decisions = json.loads((lp.proj / ".egm-dsh/decisions/tags.json").read_text())
    finally:
        lp.close()
    checks.update({
        "loop: a repository answer (quote checked) lets the loop redraft by itself": "redrafting without you" in first[1]
            and "Settled by the repository" in doc_after_first and "notes.json as a JSON list" in doc_after_first,
        "loop: a real decision stops the loop for the person": first[0] == 0 and "decision(s) need you" in first[1]
            and gate_after_first["phase"] == "designing",
        "loop: after `decide`, the loop writes the decision in and a fresh goldfish passes": decided[0] == 0 and second[0] == 0
            and "your decisions are in; redrafting" in second[1] and "READY" in second[1]
            and gate_final["phase"] == "ready" and gate_final["passed_hash"] == gate_final["doc_hash"]
            and "Decided:** Never match" in doc_final,
        "loop: the decision was the person's, not the loop's": decisions["items"][0]["choice"]["key"] == "A",
        "loop: never builds": built is None and "built" != gate_final["phase"],
    })

    # --- stall: the same questions come back after a redraft
    st = Project("stall", [
        {"match": G, "text": answers([Q_REPO, Q_OTHER])},
        {"match": E, "text": json.dumps({"items": [{"alarm": 1, **REPO_ANSWER}, {"alarm": 2, **REPO_ANSWER}]})},
        {"match": R, "text": PLAN_V2},
        {"match": G, "text": answers([Q_REPO, Q_OTHER])},
        {"match": E, "text": json.dumps({"items": [{"alarm": 1, **REPO_ANSWER}, {"alarm": 2, **REPO_ANSWER}]})},
    ])
    try:
        stalled = st.egm("loop", "tags", "--no-verify", "--stall", "1")
        st_gate = st.gate()
        st_runs = st.feature()["goldfish_runs"]
    finally:
        st.close()
    checks.update({
        "stall: the loop stops when the findings stop falling": stalled[0] == 0 and "stopped falling" in stalled[1] and "2 → 2" in stalled[1],
        "stall: it stopped after two goldfish rounds, gate shut": st_runs == 2 and st_gate["phase"] == "designing",
    })

    # --- checklist: models say ready; egm-dsh says the Verification section is missing items
    ck = Project("checklist", [{"match": G, "text": answers()}, {"match": G, "text": answers()}, {"match": G, "text": answers()}])
    try:
        ck.egm("set-verification", "--require", "mcc,sam")
        missing = ck.egm("goldfish", "tags", "--no-verify")
        rv1 = ck.review(1)
        (ck.proj / "docs/designs/tags.md").write_text(DOC + "\n## Verification\n\nAcceptance: MCC ≥ 0.6 on 40 labelled docs.\n")
        half = ck.egm("goldfish", "tags", "--no-verify")
        (ck.proj / "docs/designs/tags.md").write_text(DOC + "\n## Verification\n\nAcceptance: MCC ≥ 0.6 on 40 labelled docs.\n"
                                                      "Training: SAM with ρ = 0.05 over AdamW.\n")
        full = ck.egm("goldfish", "tags", "--no-verify")
        ck_gate = ck.gate()
    finally:
        ck.close()
    egm_alarms = [a for a in rv1["alarms"] if a["source"] == "egm" and a["section"] == "Verification"]
    checks.update({
        "checklist: the models' READY is overruled while items are missing": "NOT READY" in missing[1] and len(egm_alarms) == 2
            and any("MCC" in a["text"] for a in egm_alarms) and any("SAM" in a["text"] for a in egm_alarms),
        "checklist: one missing item still blocks": "NOT READY" in half[1] and "SAM" in half[1] and "MCC acceptance" not in half[1],
        "checklist: a complete section passes": "READY —" in full[1] and ck_gate["phase"] == "ready",
    })

    # --- builder: a blocking assumption reopens the design; escalate takes it
    def builder_turn(cli, assumptions):
        return [{"match": B, "tool": "read", "args": {"file_path": "jot/cli.py"}},
                {"match": B, "tool": "write", "args": {"file_path": "jot/cli.py", "content": cli}},
                {"match": B, "text": "Added tags.\n\n" + json.dumps({"assumptions": assumptions})}]
    original_cli = (SAMPLE / "before" / "jot" / "cli.py").read_text()
    bl = Project("builder", [
        {"match": G, "text": answers()},
        *builder_turn(original_cli + "\n# tags, lowercased\n", [
            {"decision": "tags are lowercased before saving", "section": "Technical plan", "quote": "", "blocking": True},
            {"decision": "helper named filter_by_tag", "section": "Technical plan", "quote": "", "blocking": False}]),
        {"match": E, "text": json.dumps({"items": [{"alarm": 1, "question": "Are tags case-sensitive?", "why": "product call",
                                                    "options": [{"label": "Keep case", "consequence": "Work and work differ"}]}]})},
        {"match": G, "text": answers()},
        *builder_turn(original_cli + "\n# tags, case kept\n", [
            {"decision": "helper named filter_by_tag", "section": "Technical plan", "quote": "", "blocking": False}]),
    ], git=True)
    try:
        bl.egm("goldfish", "tags", "--no-verify")
        reopened = bl.egm("build", "tags")
        bl_gate = bl.gate()
        runs = bl.feature()["goldfish_runs"]
        br = bl.review(runs)
        cli_after_reject = (bl.proj / "jot/cli.py").read_text()
        esc = bl.egm("escalate", "tags")
        # the person rewrites the doc themselves (e.g. after deciding); a fresh goldfish passes; the builder now has no blocking decision
        (bl.proj / "docs/designs/tags.md").write_text(DOC.replace("filters.", "filters. Tags keep their case."))
        bl.egm("goldfish", "tags", "--no-verify")
        ok_build = bl.egm("build", "tags")
        final_phase = bl.gate()["phase"]
        log = (bl.proj / ".egm-dsh/assumptions/tags.jsonl").read_text().splitlines()
    finally:
        bl.close()
    checks.update({
        "builder: a blocking assumption rejects the build and reopens the design": reopened[0] != 0
            and "decision(s) the design doc leaves open" in reopened[1] and bl_gate["phase"] == "designing" and bl_gate["passed_hash"] is None,
        "builder: its review is the builder's, not-ready, with the decision as a blocking alarm": br["verdict"] == "not-ready"
            and br["alarms"][0]["source"] == "builder" and "lowercased" in br["alarms"][0]["text"] and len(br["assumptions"]) == 2,
        "builder: the rejected change was reverted from git": cli_after_reject == original_cli and "reverted the rejected build" in reopened[1],
        "builder: escalate turns it into a decision for the person": esc[0] == 0 and "The builder had to decide" in esc[1] and "[A] Keep case" in esc[1],
        "builder: only non-blocking assumptions: the build is accepted": ok_build[0] == 0 and final_phase == "built"
            and "assumed builder decided: helper named filter_by_tag" in ok_build[1],
        "builder: every reported decision is kept": len(log) == 3,
    })

    print()
    for name, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + name)
    print("SUMMARY " + json.dumps({"passed": sum(map(bool, checks.values())), "failed": sum(not v for v in checks.values())}))
    if not all(checks.values()):
        sys.exit(1)


if __name__ == "__main__":
    main()
