"""Mechanical check, no API key: does DeepSeek Harness load Google's official
EGM skills, and does `/egm-review` put the skill's instructions in front of the
model together with a subagent tool to spawn the persona reviewers?

Runs the real dsh binary against tests/e2e/fake_deepseek.py, which records every
request body. It checks what dsh SENDS, not what a real model would do with it.

usage: python3 bench/probe_official_in_dsh.py UPSTREAM_REPO DSH_COMMAND...
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
sys.path.insert(0, str(HERE))
import official_dsh  # noqa: E402

UPSTREAM = Path(sys.argv[1]).resolve()
DSH = sys.argv[2:]
CRATE = HERE.parent


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
    work = Path(tempfile.mkdtemp(prefix="egm-official-probe-"))
    proj = work / "jot"
    shutil.copytree(CRATE / "sample" / "before", proj)
    (proj / "docs" / "designs").mkdir(parents=True)
    (proj / "docs" / "designs" / "tags.md").write_text("# Tags\n\n## The Problem\n\nUsers cannot group notes.\n")
    skills = official_dsh.install(proj, UPSTREAM)
    patch = work / "official.patch.yml"
    patch.write_text(official_dsh.patch_yaml(skills, model="deepseek-flash", effort="high"))
    (work / "rules.json").write_text(json.dumps([{"match": "", "text": "probe ok"}] * 4))   # read once at server start
    (work / "DUMP").write_text("")
    port = free_port()
    server = subprocess.Popen([sys.executable, "-I", str(CRATE / "tests" / "e2e" / "fake_deepseek.py"), str(port), str(work / "rules.json"), str(work)])
    wait_port(port)
    env = dict(os.environ, DEEPSEEK_API_KEY="fake", DEEPSEEK_BASE_URL=f"http://127.0.0.1:{port}", NO_COLOR="1")
    # Negative control first: same prompt, no customSkillDirs. The skill body must NOT appear.
    control = work / "control.patch.yml"
    control.write_text("\n".join(l for l in official_dsh.patch_yaml(skills).split("- id: skill-filesystem")[:1]))
    try:
        official_dsh.run(proj, DSH, official_dsh.review_prompt("docs/designs/tags.md"), control, env=env, timeout=300)
        control_bodies = (work / "bodies.jsonl").read_text()
        (work / "bodies.jsonl").unlink()
        out = official_dsh.run(proj, DSH, official_dsh.review_prompt("docs/designs/tags.md"), patch, env=env, timeout=300)
    finally:
        server.terminate()
    bodies = [json.loads(l) for l in (work / "bodies.jsonl").read_text().splitlines()]
    main_reqs = [b for b in bodies if "concise title" not in json.dumps(b.get("system", ""))]
    first = main_reqs[0] if main_reqs else {}
    rendered = json.dumps(first, ensure_ascii=False)
    tools = [t.get("name") for t in first.get("tools", [])]
    skill_body = (UPSTREAM / "skills" / "egm_review" / "SKILL.md").read_text()
    marker = "Consolidate observations into actionable tasks by priority"   # a line from the skill body, not its description
    assert marker in skill_body
    checks = {
        "dsh ran the turn": out["exit"] == 0 and out["error"] is None,
        "the request reached the model": bool(main_reqs),
        "egm-review's instructions were injected (body text, not just the description)": marker in rendered,
        "negative control: without customSkillDirs the instructions are absent": marker not in control_bodies,
        "a subagent tool was offered for the persona reviewers": any(t and "subagent" in t for t in tools),
        "the persona files are installed where the skill looks": (proj / official_dsh.PLUGIN_DIR / "agents" / "zoom_out.md").is_file(),
        "no title side requests": len(main_reqs) == len(bodies),
        "the requested model was used": first.get("model") == "deepseek-flash",
    }
    for name, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + name)
    print("tools offered:", ", ".join(t for t in tools if t))
    print("SUMMARY " + json.dumps({"passed": sum(checks.values()), "failed": sum(not v for v in checks.values())}))
    if not all(checks.values()):
        print("stderr tail:", out["stderr"][-800:])
        sys.exit(1)
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
