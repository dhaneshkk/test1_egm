"""Run Google's official EGM skills (github.com/google/elephant-goldfish-model)
inside DeepSeek Harness, and read their verdict.

Installation: the upstream plugin (agents/, resources/, skills/, LICENSE) is
copied into `<project>/.dsh/egm/`, and a dsh patch overlay adds
`.dsh/egm/skills` to the `skill-filesystem` plugin's `customSkillDirs`.
Upstream skills refer to `agents/<file>.md` and `resources/...` relative to the
plugin root, so the prompt names that root.

egm-review is started non-interactively with its documented inputs
(`TargetFile`, `GoldfishContext`, `Step: uninterrupted`), which selects preset 1,
"Consolidated report with all review rubrics", and writes
`consolidated_review_report.md`. Its verdict: not ready iff that report lists
at least one 🔴 Blocker item.
"""
import json
import os
import re
import shutil
import subprocess
from pathlib import Path

PLUGIN_DIR = ".dsh/egm"
UPSTREAM_PARTS = ["agents", "resources", "skills", "LICENSE"]


def install(project: Path, upstream: Path) -> Path:
    """Copy the upstream plugin into the project. Returns the skills directory."""
    dest = project / PLUGIN_DIR
    shutil.rmtree(dest, ignore_errors=True)
    dest.mkdir(parents=True)
    for part in UPSTREAM_PARTS:
        src = upstream / part
        if src.is_dir():
            shutil.copytree(src, dest / part)
        elif src.is_file():
            shutil.copy2(src, dest / part)
    return dest / "skills"


def patch_yaml(skills_dir: Path, model: str | None = None, effort: str | None = None) -> str:
    """dsh overlay: model, effort, no title requests, and the upstream skills root."""
    s = "# Written by egm-dsh bench for the official EGM pipeline.\n"
    if model:
        s += f"- id: agent-default-model\n  config:\n    provider: deepseek-official\n    model: {json.dumps(model)}\n"
    if effort:
        s += f"- id: llm-deepseek\n  config:\n    reasoningEffort: {json.dumps(effort)}\n"
    s += "- id: session-title-llm\n  disabled: true\n"
    s += f"- id: skill-filesystem\n  config:\n    customSkillDirs:\n      - {json.dumps(str(skills_dir.resolve()))}\n"
    return s


def review_prompt(doc_rel: str) -> str:
    return (
        f"/egm-review\n\n"
        f"TargetFile: {doc_rel}\n"
        f"GoldfishContext: none (proceed with no additional context; reviewers still read files the doc links)\n"
        f"Step: uninterrupted\n\n"
        f"The EGM plugin root is `{PLUGIN_DIR}`: persona files are in `{PLUGIN_DIR}/agents/` and standards in "
        f"`{PLUGIN_DIR}/resources/document_standards/`. This is a non-interactive run: do not ask questions, do not "
        f"edit the design document, and write `consolidated_review_report.md` in the repository root."
    )


def run(project: Path, dsh: list[str], prompt: str, patch_path: Path, env: dict | None = None,
        permission: str = "workspace-write", timeout: int = 3600) -> dict:
    """One headless dsh turn. Returns final text, tool calls, usage and errors."""
    cmd = [*dsh, "--profile", "headless", "--patch", str(patch_path), "--json", "-"]   # --patch before --json
    p = subprocess.run(cmd, cwd=project, input=prompt, text=True, capture_output=True, timeout=timeout,
                       env=dict(env or os.environ, DSH_PERMISSION_MODE=permission))
    out = {"final": "", "tools": [], "usage": {"cache_hit": 0, "cache_miss": 0, "output": 0, "requests": 0},
           "error": None, "exit": p.returncode, "stderr": p.stderr[-2000:]}
    for line in p.stdout.splitlines():
        try:
            e = json.loads(line)
        except json.JSONDecodeError:
            continue
        kind = e.get("type")
        if kind == "tool_call":
            out["tools"].append(e.get("tool"))
        elif kind == "status" and e.get("phase") == "step_end":
            u = e.get("usage") or {}
            out["usage"]["requests"] += 1
            out["usage"]["cache_miss"] += u.get("inputTokens", 0)
            out["usage"]["cache_hit"] += u.get("cacheReadTokens", 0)
            out["usage"]["output"] += u.get("outputTokens", 0)
        elif kind == "status" and e.get("phase") == "turn_end" and (e.get("reason") or {}).get("kind") != "completed":
            out["error"] = ((e.get("reason") or {}).get("error") or {}).get("message", "turn did not complete")
        elif kind == "final":
            out["final"] = e.get("text", "")
        elif kind == "error":
            out["error"] = e.get("message", "error")
    return out


_HEADING = re.compile(r"^\s{0,3}#{1,6}\s")
_ITEM = re.compile(r"^\s*(?:[-*+]|\d+[.)]|\|)\s*")
_NONE = re.compile(r"\b(none|no blockers?|0 blockers?|n/a)\b", re.I)


def blockers(report: str) -> list[str]:
    """🔴 Blocker items in an upstream consolidated report.

    Counts list items and table rows that carry the 🔴 marker or the word
    Blocker, plus items under a heading that names Blockers. A legend line, a
    table header/separator, or an explicit "none" does not count.
    """
    found, under_blocker_heading = [], False
    for raw in report.splitlines():
        line = raw.strip()
        if not line:
            continue
        if _HEADING.match(raw):
            under_blocker_heading = ("🔴" in line or re.search(r"\bblockers?\b", line, re.I)) and not _NONE.search(line)
            continue
        if not _ITEM.match(raw) or set(line) <= set("|-: "):
            continue
        cells = [c.strip() for c in line.strip("|").split("|")] if line.startswith("|") else [line]
        if line.startswith("|") and all(c.lower() in {"priority", "severity", "id", "finding", "action", "item", "stage", "status", "source", ""} for c in cells):
            continue   # table header
        marked = "🔴" in line or re.search(r"\bblocker\b", line, re.I)
        if (marked or under_blocker_heading) and not _NONE.search(line) and "🟡" not in line and "🟢" not in line:
            found.append(line)
    return found


def find_report(project: Path) -> Path | None:
    hits = [p for p in project.rglob("consolidated_review_report.md") if PLUGIN_DIR not in str(p.relative_to(project))]
    return min(hits, key=lambda p: len(p.parts)) if hits else None


def verdict(project: Path) -> tuple[str, list[str]]:
    """('not-ready' | 'ready' | 'incomplete', blocker lines)."""
    path = find_report(project)
    if path is None:
        return "incomplete", []
    found = blockers(path.read_text(errors="replace"))
    return ("not-ready" if found else "ready"), found


if __name__ == "__main__":   # quick self-check of the report parser
    sample = """# Consolidated Review Report

Legend: 🔴 Blocker, 🟡 Major, 🟢 Nit

## 🔴 Blockers

1. 🔴 The plan never says what happens to notes saved before tags existed.

## 🟡 Major

- 🟡 Showing tags on every line may be more than needed.

| Priority | Finding |
|---|---|
| 🔴 Blocker | Old notes have no rule. |
| 🟢 Nit | Wording. |
"""
    clean = "# Report\n\nLegend: 🔴 Blocker, 🟡 Major, 🟢 Nit\n\n## 🔴 Blockers\n\nNone.\n\n## 🟡 Major\n\n- 🟡 Consider a hint.\n"
    assert len(blockers(sample)) == 2, blockers(sample)
    assert blockers(clean) == [], blockers(clean)
    print("report parser ok")
