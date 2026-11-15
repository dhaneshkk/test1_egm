"""End-to-end test of PlantUML sequence diagrams in design docs, on the real
DeepSeek Harness binary (model faked by fake_deepseek.py; request bodies captured).
PlantUML's syntax check is built in (plantuml-core, no Java). With EGM_PLANTUML (an
external command, e.g. "java -jar /path/plantuml.jar") it also tests that path.

  * drafting asks for sequence diagrams (policy `prefer`, the default); `off` does not
  * a drafted diagram is saved intact, fences and all, and does not count as words
  * a malformed diagram (an `alt` never closed) is egm-dsh's own question: fail-fast
    stops with no model call, and the full review cannot drop it
  * with PlantUML: a diagram egm-dsh's structure check accepts but PlantUML rejects
    is a question too; without it, the structure check alone runs
  * `require`: a Technical plan with no diagram is a question
  * `doctor` reports the PlantUML check

usage: python3 run_diagram_e2e.py EGM_BINARY DSH_COMMAND...
"""
import json
import os
import shlex
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
PLANTUML = shlex.split(os.environ.get("EGM_PLANTUML", ""))

START = "You are the elephant in an Elephant-Goldfish design session"
DRAFT = "Draft the whole design doc"
GOLD = "Complete five separate tasks"
DIAGRAM = """```plantuml
@startuml
actor User
participant "jot/cli.py" as CLI
database "notes.json" as Store
User -> CLI : jot add --tag work "text"
CLI -> Store : load()
alt tag is valid
  CLI -> Store : save(note with tags)
  CLI --> User : added #N
else tag has a comma
  CLI --> User : error: one tag per --tag
end
@enduml
```"""
PLAN = "Notes gain a `tags` list; notes saved before have none.\n\n" + DIAGRAM + "\n\nTags are stored lowercased."
SECTIONS = {
    "Problem": "People cannot list only work notes.",
    "Goals and non-goals": "Goals: tag notes, filter by tag. Non-goals: renaming tags.",
    "Technical plan": PLAN,
    "Alternatives considered": "**Folders.** Rejected: one note, many tags.",
    "Risks and mitigations": "**Old notes.** Read as untagged.",
    "Implementation map": "- `jot/cli.py` — tags",
    "Acceptance criteria": "- `list --tag work` shows only work notes. Check: `tests/test_tags.py::TagTest::test_filter`",
}
DRAFT_REPLY = "\n\n".join(f"## {k}\n\n{v}" for k, v in SECTIONS.items())
CLEAN = {"comprehension": {"problem": "p", "current_mechanism": "m", "proposed_change": "c", "unclear": []},
         "critic": {"findings": []}, "zoom_out": {"findings": []}, "readiness": {"ready": True, "blocking_questions": []},
         "invariants": {"criteria": [{"criterion": "c", "predicate": "p", "check": "tests/test_tags.py"}]}}


def doc_with_plan(plan):
    return "# Design: Tags\n\n" + "\n\n".join(f"## {k}\n\n{plan if k == 'Technical plan' else v}" for k, v in SECTIONS.items()) + "\n"


UNCLOSED = DIAGRAM.replace("end\n@enduml", "@enduml")            # the alt is never closed
PUML_ONLY = DIAGRAM.replace("database \"notes.json\" as Store", "database \"notes.json\" as Store\nthis is not plantuml at all")


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
    work = Path(tempfile.mkdtemp(prefix="egm-dsh-diagram-"))
    rules = [{"match": START, "keep": True, "text": "Who uses tags?"},
             {"match": DRAFT, "keep": True, "text": DRAFT_REPLY},
             {"match": GOLD, "keep": True, "text": json.dumps(CLEAN)}]
    (work / "rules.json").write_text(json.dumps(rules))
    (work / "DUMP").write_text("")
    port = free_port()
    server = subprocess.Popen([sys.executable, "-I", str(HERE / "fake_deepseek.py"), str(port), str(work / "rules.json"), str(work)])
    wait_port(port)
    env = dict(os.environ, DEEPSEEK_API_KEY="fake", DEEPSEEK_BASE_URL=f"http://127.0.0.1:{port}", NO_COLOR="1")

    def requests():
        f = work / "requests.jsonl"
        return len(f.read_text().splitlines()) if f.exists() else 0

    def project(name):
        proj = work / name
        shutil.copytree(SAMPLE / "before", proj, ignore=shutil.ignore_patterns("__pycache__"))
        egm(proj, "init", "--agent", "dsh", "--preset", "economy", "--dsh-command", *DSH)
        return proj

    def egm(proj, *args):
        r0 = requests()
        p = subprocess.run([EGM, *args], cwd=proj, env=env, text=True, capture_output=True, timeout=600)
        out = (p.stdout + p.stderr).rstrip()
        print(f"$ egm-dsh {' '.join(args)}\n{out[-2500:]}\n", flush=True)
        return p.returncode, out, requests() - r0

    def bodies_with(marker):
        f = work / "bodies.jsonl"
        return [l for l in f.read_text().splitlines() if marker in l] if f.exists() else []

    def review(proj, slug):
        f = json.loads((proj / ".egm-dsh" / "features" / f"{slug}.json").read_text())
        return json.loads((proj / ".egm-dsh" / "reviews" / f"{slug}-{f['goldfish_runs']}.json").read_text())

    def adopt(proj, slug, text):
        (proj / "docs" / "designs" / f"{slug}.md").write_text(text)
        egm(proj, "adopt", slug, "--doc", f"docs/designs/{slug}.md")

    try:
        # 1. drafting with diagrams on (the default), and a word limit
        a = project("draft")
        egm(a, "set-draft-words", "60")
        egm(a, "elephant", "tags", "--idea", "Let notes carry tags.")
        n_before = len(bodies_with(DRAFT))
        draft_a = egm(a, "draft", "tags")
        draft_bodies = bodies_with(DRAFT)[n_before:]
        doc_a = (a / "docs" / "designs" / "tags.md").read_text()
        gold_a = egm(a, "goldfish", "tags", "--no-verify", "--fail-fast")

        # 1b. `off`: no diagram guidance in the draft prompt
        b = project("off")
        egm(b, "set-diagrams", "off")
        egm(b, "elephant", "tags", "--idea", "Let notes carry tags.")
        n_before = len(bodies_with(DRAFT))
        egm(b, "draft", "tags")
        off_bodies = bodies_with(DRAFT)[n_before:]

        # 2. a malformed diagram: fail-fast stops with no request; the full review keeps the question
        c = project("malformed")
        adopt(c, "tags", doc_with_plan(PLAN.replace(DIAGRAM, UNCLOSED)))
        ff_c = egm(c, "goldfish", "tags", "--no-verify", "--fail-fast")
        rv_ff_c = review(c, "tags")
        full_c = egm(c, "goldfish", "tags")
        rv_full_c = review(c, "tags")

        # 3. structure-valid, PlantUML-invalid: the built-in checker (no Java) catches it by default
        d = project("plantuml")
        adopt(d, "tags", doc_with_plan(PLAN.replace(DIAGRAM, PUML_ONLY)))
        builtin = egm(d, "goldfish", "tags", "--no-verify", "--fail-fast")
        rv_builtin = review(d, "tags")
        doctor_builtin = egm(d, "doctor")
        # ... and with the check turned off, the structure check alone lets it through
        egm(d, "set-diagrams", "--no-plantuml-check")
        Path(d / "docs" / "designs" / "tags.md").write_text(doc_with_plan(PLAN.replace(DIAGRAM, PUML_ONLY)) + "\n\n")
        no_puml = egm(d, "goldfish", "tags", "--no-verify", "--fail-fast")
        rv_no_puml = review(d, "tags")
        egm(d, "set-diagrams", "--plantuml-check")
        lint = subprocess.run([EGM, "diagrams", str(d / "docs" / "designs" / "tags.md"), str(a / "docs" / "designs" / "tags.md")], text=True, capture_output=True, env=env)
        with_puml = rv_with_puml = doctor_d = None
        if PLANTUML:
            egm(d, "set-diagrams", "--plantuml-cmd", *PLANTUML)
            Path(d / "docs" / "designs" / "tags.md").write_text(doc_with_plan(PLAN.replace(DIAGRAM, PUML_ONLY)) + "\n")
            with_puml = egm(d, "goldfish", "tags", "--no-verify", "--fail-fast")
            rv_with_puml = review(d, "tags")
            doctor_d = egm(d, "doctor")

        # 3b. a PlantUML that cannot run (as on a JRE missing its AWT/fonts): a warning, never a pass or a block
        f = project("broken-tool")
        fake = work / "broken_plantuml.sh"
        fake.write_text("#!/bin/sh\necho 'Exception in thread \"main\" java.lang.UnsatisfiedLinkError: libawt_xawt.so' >&2\nexit 1\n")
        fake.chmod(0o755)
        egm(f, "set-diagrams", "--plantuml-cmd", str(fake))
        adopt(f, "tags", doc_with_plan(PLAN))
        broken = egm(f, "goldfish", "tags", "--no-verify", "--fail-fast")
        rv_broken = review(f, "tags")
        doctor_f = egm(f, "doctor")

        # 4. require: prose only is a question
        e = project("require")
        egm(e, "set-diagrams", "require")
        adopt(e, "tags", doc_with_plan("Notes gain a `tags` list; `add` saves them and `list --tag` filters on them."))
        req_e = egm(e, "goldfish", "tags", "--no-verify", "--fail-fast")
        rv_req_e = review(e, "tags")
        doctor_e = egm(e, "doctor")
    finally:
        server.terminate()

    egm_q = lambda rv: [x["text"] for x in rv["alarms"] if x["source"] == "egm"]
    checks = {
        "draft prompt asks for PlantUML sequence diagrams (default prefer)": bool(draft_bodies) and all("PlantUML sequence diagram" in x for x in draft_bodies),
        "the drafted diagram is saved intact, fences and all": DIAGRAM in doc_a,
        "diagram lines do not count as words; the plan is reported with its diagram": "Technical plan 14 + 1 diagram" in draft_a[1]
            and "over the 60-word limit" not in draft_a[1],
        "the drafted doc passes egm-dsh's checks and the goldfish": "verdict: READY" in gold_a[1],
        "`off`: no diagram guidance": bool(off_bodies) and all("PlantUML sequence diagram" not in x for x in off_bodies),
        "unclosed alt: fail-fast NOT READY with no model request": ff_c[2] == 0 and rv_ff_c["stopped_at"] == "precheck"
            and any("Diagram 1" in t and "`alt` opened" in t and "never closed" in t for t in egm_q(rv_ff_c)),
        "unclosed alt: the full review keeps the question (no model can drop it)": rv_full_c["verdict"] == "not-ready"
            and any("`alt` opened" in t for t in egm_q(rv_full_c)) and all(not (a["source"] == "egm" and a["status"] == "dropped") for a in rv_full_c["alarms"]),
        "built-in PlantUML (no Java): its syntax error is a question, no model called": builtin[2] == 0
            and any("PlantUML:" in t for t in egm_q(rv_builtin)),
        "doctor: the built-in check passes a good diagram and rejects a broken one": "built-in plantuml-core" in doctor_builtin[1] and "does not work" not in doctor_builtin[1],
        "PlantUML check off: the structure check alone passes the PlantUML-only error": rv_no_puml["verdict"] == "ready",
        "`egm-dsh diagrams` lints files: exit 2, the broken one named, the good one passed": lint.returncode == 2
            and "✖" in lint.stdout and "PlantUML:" in lint.stdout and "✔" in lint.stdout,
        "broken PlantUML: a warning naming the failure, the doc not blocked, no false pass claimed": "PlantUML could not check the diagrams" in broken[1]
            and "UnsatisfiedLinkError" in broken[1] and not any("PlantUML" in t for t in egm_q(rv_broken)),
        "broken PlantUML: doctor says it does not work": "does not work" in doctor_f[1] and "UnsatisfiedLinkError" in doctor_f[1],
        "require: a prose-only plan is a question, no model called": req_e[2] == 0 and any("no valid PlantUML sequence diagram" in t for t in egm_q(rv_req_e)),
        "doctor reports the diagram policy": "diagrams: require" in doctor_e[1],
    }
    if PLANTUML:
        checks["with PlantUML: its syntax error is a question, no model called"] = with_puml[2] == 0 and any("PlantUML:" in t for t in egm_q(rv_with_puml))
        checks["doctor: the PlantUML check works"] = "PlantUML syntax check:" in doctor_d[1]
    for name, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + name)
    print("SUMMARY " + json.dumps({"passed": sum(map(bool, checks.values())), "failed": sum(not v for v in checks.values()), "plantuml": bool(PLANTUML)}))
    if not all(checks.values()):
        sys.exit(1)
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
