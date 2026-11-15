"""egm-dsh on Harness² (github.com/google-research/harness2, Apache-2.0): tune the
goldfish reviewers' prompt text with Harness²'s unmodified recursion engine.

  DSH_SURFACE / DshSubstrate  Harness²'s c1..c8 component tree, mapped to what a
                              dsh-run goldfish can take from a setup: reviewer
                              guidance (always-on files) and per-stage personas
                              (sub_agents/<stage>/AGENT.md). Nothing else.
  GoldfishTuneAdapter         a Harness² benchmark adapter; one execution is one
                              `egm-dsh goldfish` review of one design doc.
  TuneProposer                Harness²'s improver, over an OpenAI-compatible
                              /chat/completions endpoint (or scripted, offline).
                              Selection uses the doc's LABEL, not a model judge:
                              egm-dsh has ground truth, so Harness²'s main
                              weakness (no verifier) does not apply here.

What tuning cannot reach, by construction: egm-dsh loads a setup through
`src/tuning.rs`, which accepts prompt text only. JSON schemas, grounding, the
verifier, the pass rule, the gate, frozen tests and limits stay in code
(`proofs/EgmTune.lean`).
"""
from __future__ import annotations

import hashlib
import json
import threading
from concurrent.futures import ThreadPoolExecutor
import os
import re
import shutil
import subprocess
import time
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path

from harness2.core import tree
from harness2.core.proposer import Proposer, Reply
from harness2.core.spec import BenchPrompts, BenchSpec, ChannelSpec
from harness2.substrate.base import Install, RunRequest

HERE = Path(__file__).resolve().parent
CRATE = HERE.parent.parent
CORPUS = CRATE / "bench" / "corpus"
STAGES = ("comprehension", "critic", "zoom-out", "readiness")
GUIDANCE = ("systemprompt.md", "guardrails.md", "LongTermMEMORY.md", "tool_descriptions/tool_guidance.md")
STUBS = ("scripts/README.md", "sub_agents/README.md", "skills/README.md")
MAX_FILE_CHARS = 8_000   # same cap as src/tuning.rs

DSH_SURFACE = tree.SurfaceSpec(
    name="dsh",
    always_on=GUIDANCE,
    always_on_max=16_000,
    always_on_cap_reason="egm-dsh pays for every guidance token on every goldfish request",
    skill_dir="skills",
    skill_name_rule="must_match",
    trigger_hint="Skills are not used by egm-dsh goldfish; put guidance in guardrails.md instead.",
    autotrigger_file="guardrails.md",
    trigger_line="- When {desc}, load the `{name}` skill.\n",
    subagent_requires_mode=False,
    plugin_dir=None,
    hooks_file=None,
    stubs=(
        ("sub_agents/README.md", "<!-- c5: one persona per goldfish stage: sub_agents/<stage>/AGENT.md with a "
                                 "`description:` line, stage one of comprehension, critic, zoom-out, readiness. -->\n"),
    ),
)


# Harness² keeps per-executor prompt text in a private registry and has no public way
# to add one, so the `dsh` entry is registered here (the one place this module reaches
# into Harness² internals; an upstream `register_surface()` would replace it).
from harness2.core import prompts as _h2_prompts  # noqa: E402

_DSH_SURFACE_MD = """\
# THE SETUP YOU ARE EDITING

These files shape memoryless reviewers who read one software design document and say whether an
engineer could implement it correctly in one pass. Each review runs five stages: comprehension,
critic, zoom-out, readiness, invariants.

  guardrails.md, systemprompt.md, LongTermMEMORY.md, tool_descriptions/tool_guidance.md
      appended to every stage as "Reviewer guidance": checks every reviewer applies
  sub_agents/<stage>/AGENT.md  (stage: comprehension | critic | zoom-out | readiness)
      replaces that stage's persona; start it with a `description:` line

Nothing else is read. The reviewers' answer format and the rules that turn answers into a
verdict are fixed elsewhere; editing other files is discarded.

# HOW TO MAKE A CHANGE

Edit files under context/harness/. Prefer one short, general check over many specific ones:
guidance is paid for on every review of every future document, and a check that only fits the
document in front of you buys nothing on the next one."""


def _register_dsh_prompts() -> None:
    oc = _h2_prompts._SURFACE_TEXTS["opencode"]
    _h2_prompts._SURFACE_TEXTS.setdefault("dsh", _h2_prompts._SurfaceText(
        surface_md=_DSH_SURFACE_MD,
        inv=_h2_prompts._INV_TEMPLATE.replace("__LOOP_LEVER__", "check in guardrails.md that every reviewer stage applies"),
        dom=_h2_prompts._DOM_TEMPLATE.replace("__TRIGGER_SENTENCE__", "A persona takes effect by its file location; no trigger line is needed."),
        tts_surface=_DSH_SURFACE_MD if oc else _DSH_SURFACE_MD,
    ))


_register_dsh_prompts()


def allowed(rel: str) -> bool:
    """Mirror of `tuning::allowed` in src/tuning.rs."""
    return rel in GUIDANCE or rel in STUBS or any(rel == f"sub_agents/{s}/AGENT.md" for s in STAGES)


def clean_setup(src: Path, dst: Path) -> Path:
    """Copy only the files egm-dsh accepts into `dst`; refuse anything else."""
    shutil.rmtree(dst, ignore_errors=True)
    dst.mkdir(parents=True)
    for p in sorted(src.rglob("*")):
        if p.is_file():
            rel = p.relative_to(src).as_posix()
            if not allowed(rel):
                raise ValueError(f"setup file outside the tunable surface: {rel}")
            (dst / rel).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(p, dst / rel)
    return dst


def fingerprint(d: Path) -> str:
    h = hashlib.sha256()
    for p in sorted(x for x in d.rglob("*") if x.is_file()):
        h.update(p.relative_to(d).as_posix().encode() + b"\0" + p.read_bytes())
    return h.hexdigest()[:16]


class DshSubstrate:
    """Harness²'s Substrate protocol for DeepSeek Harness."""

    name = "dsh"

    def __init__(self, launcher=None, egm_dsh: str | None = None):
        self.launcher = launcher
        self.egm_dsh = egm_dsh

    def surface(self):
        return DSH_SURFACE

    def materialise(self, tree_dir: Path, spec, ctx) -> Install:
        stage = Path(ctx.get("staging", "")) if ctx and ctx.get("staging") else Path(tree_dir).with_suffix(".staged")
        clean_setup(Path(tree_dir), stage)
        return Install(staging_dir=stage, fingerprint=fingerprint(Path(tree_dir)))

    def execute(self, task_ref: str, install: Install, run_ids: list[str], timeout: int) -> list[Path]:
        if self.launcher is None:
            raise RuntimeError("DshSubstrate needs a launcher (the benchmark supplies it)")
        return self.launcher(RunRequest(task_ref, tuple(run_ids), timeout), install)

    def read_trajectory(self, stream_path: Path, *, obs_trunc: int, max_chars: int, args_trunc: int = 400) -> str:
        """dsh `--json` events -> Harness²'s [ACTION]/[OBSERVATION] grammar."""
        out: list[str] = []
        for line in Path(stream_path).read_text(errors="replace").splitlines():
            try:
                e = json.loads(line)
            except ValueError:
                continue
            if e.get("type") == "tool_call":
                out.append(f"[ACTION] {e.get('tool')} {json.dumps(e.get('input', {}))[:args_trunc]}")
            elif e.get("type") == "tool_result":
                out.append("[OBSERVATION]\n" + str(e.get("output", e.get("content", "")))[:obs_trunc])
            elif e.get("type") == "final":
                out.append("[FINAL]\n" + str(e.get("text", ""))[:obs_trunc])
        return "\n".join(out)[:max_chars]

    def preflight(self) -> None:
        if self.egm_dsh and not Path(self.egm_dsh).is_file():
            raise RuntimeError(f"egm-dsh binary not found at {self.egm_dsh}; run: cargo build --release")


def mcc(pred: list[bool], truth: list[bool]) -> tuple[float | None, dict]:
    tp = sum(p and t for p, t in zip(pred, truth))
    tn = sum(not p and not t for p, t in zip(pred, truth))
    fp = sum(p and not t for p, t in zip(pred, truth))
    fn = sum(not p and t for p, t in zip(pred, truth))
    den = ((tp + fp) * (tp + fn) * (tn + fp) * (tn + fn)) ** 0.5
    return ((tp * tn - fp * fn) / den if den else None), {"tp": tp, "tn": tn, "fp": fp, "fn": fn}


@dataclass
class Endpoint:
    base_url: str
    model: str
    api_key_env: str = "DEEPSEEK_API_KEY"


class GoldfishTuneAdapter:
    """Harness² benchmark adapter: each task is a labelled design doc; each
    execution runs `egm-dsh goldfish` on it with the candidate setup."""

    def __init__(self, root: Path, egm_dsh: str, reviewer: Endpoint, tasks: list[str], labels: dict[str, str]):
        self.root = root
        self.egm_dsh = egm_dsh
        self.reviewer = reviewer
        self._tasks = tasks
        self.labels = labels
        self.substrate = DshSubstrate(egm_dsh=egm_dsh)
        ch = ChannelSpec("opencode", f"deepseek/{reviewer.model}")
        self.spec = BenchSpec(
            name="egm-goldfish",
            runs_root=root / "runs",
            install_mode="workspace_config",
            prompts=BenchPrompts(work="review software design documents and decide whether an engineer could "
                                      "implement each one correctly in one pass, without guessing"),
            channels={"proposer": ch, "selector": ch},
            leak_globs=("*.label", "labels.json"),
        )
        self.execution_count = 0
        self.usage: dict[str, int] = {"requests": 0, "input_tokens": 0}
        self.jobs = 4                      # docs reviewed at the same time in `evaluate`
        self._lock = threading.Lock()      # guards `usage` across those reviews
        base = {name: "" for name in DSH_SURFACE.always_on}
        tree.write_tree(DSH_SURFACE, base, self.base_harness(), stubs=False)
        for t in self.all_docs():
            d = self.task_files(t)
            d.mkdir(parents=True, exist_ok=True)
            shutil.copy2(CORPUS / f"{t}.md", d / f"{t}.md")

    def all_docs(self) -> list[str]:
        return sorted(p.stem for p in CORPUS.glob("*.md"))

    # --- Harness² adapter interface -------------------------------------------------
    def tasks(self, split: str = "all") -> list[str]:
        return list(self._tasks)

    def domain_of(self, task_id: str) -> str:
        return "design-review"

    def task_prompt(self, task_id: str) -> str:
        return (f"Review the design document {task_id}.md as a reviewer with no prior knowledge of the project, "
                f"and decide whether an engineer could implement it correctly in one pass.")

    def task_files(self, task_id: str) -> Path:
        return self.root / "sources" / task_id

    def base_harness(self) -> Path:
        return self.root / "base"

    def validate_tree(self, harness: dict) -> None:
        bad = [rel for rel in harness if not allowed(rel)]
        big = [rel for rel, text in harness.items() if len(text) > MAX_FILE_CHARS]
        if bad or big:
            raise ValueError(f"only reviewer guidance and stage personas may be tuned; refused: {bad + big}")

    def execute(self, task_id: str, harness_dir: Path | None, run_id: str, *, timeout: int | None = None) -> Path:
        self.execution_count += 1
        run_dir = self.root / "executions" / run_id
        shutil.rmtree(run_dir, ignore_errors=True)
        run_dir.mkdir(parents=True)
        review = self.review(task_id, harness_dir, run_dir / "work")
        verdict = review.get("verdict", "incomplete")
        open_alarms = [a["text"] for a in review.get("alarms", []) if a.get("status") != "dropped"]
        (run_dir / "answer.txt").write_text(f"VERDICT: {verdict}\n" + "".join(f"- {a}\n" for a in open_alarms))
        lines = [f"[ACTION] egm-dsh goldfish {task_id}"]
        for stage in ("comprehension", "critic", "zoom_out", "readiness", "invariants"):
            lines.append(f"[OBSERVATION] {stage}: {json.dumps(review.get(stage))[:1500]}")
        lines.append(f"[FINAL] verdict {verdict}; open alarms: {len(open_alarms)}")
        (run_dir / "trajectory.md").write_text("\n".join(lines) + "\n")
        return run_dir

    def delivered(self, run_dir: Path) -> bool:
        return (run_dir / "answer.txt").is_file()

    def trajectory(self, run_dir: Path, *, obs_trunc: int) -> str:
        return (run_dir / "trajectory.md").read_text()[: max(obs_trunc, 2000) * 4]

    def deliverable_text(self, run_dir: Path) -> str:
        return (run_dir / "answer.txt").read_text()

    def deliverable_files(self, run_dir: Path) -> dict[str, str]:
        return {"answer.txt": self.deliverable_text(run_dir)}

    # --- the executor ------------------------------------------------------------------
    def review(self, task_id: str, harness_dir: Path | None, work: Path) -> dict:
        """One `egm-dsh goldfish` review of one doc, in a scratch copy of the sample repo."""
        proj = work / "jot"
        shutil.rmtree(work, ignore_errors=True)
        shutil.copytree(CRATE / "sample" / "before", proj, ignore=shutil.ignore_patterns("__pycache__"))
        (proj / "docs" / "designs").mkdir(parents=True)
        shutil.copy2(CORPUS / f"{task_id}.md", proj / "docs" / "designs" / f"{task_id}.md")
        env = dict(os.environ, NO_COLOR="1")
        env.pop("EGM_DSH_GOLDFISH_HARNESS", None)
        if harness_dir is not None:
            env["EGM_DSH_GOLDFISH_HARNESS"] = str(clean_setup(Path(harness_dir), work / "setup"))
        run = lambda *a: subprocess.run([self.egm_dsh, *a], cwd=proj, env=env, text=True, capture_output=True, timeout=900)
        run("init", "--agent", "openai", "--preset", "balanced")
        cfg_path = proj / ".egm-dsh" / "config.json"
        cfg = json.loads(cfg_path.read_text())
        cfg["openai"] = {"base_url": self.reviewer.base_url, "model": self.reviewer.model, "api_key_env": self.reviewer.api_key_env}
        cfg["roles"] = {}   # every goldfish role on the reviewer endpoint's model
        cfg_path.write_text(json.dumps(cfg, indent=1))
        run("adopt", task_id, "--doc", f"docs/designs/{task_id}.md")
        out = run("goldfish", task_id, "--no-verify")
        (work / "goldfish.log").write_text(out.stdout + out.stderr)
        path = proj / ".egm-dsh" / "reviews" / f"{task_id}-1.json"
        review = json.loads(path.read_text()) if path.exists() else {"verdict": "incomplete", "alarms": []}
        with self._lock:
            for c in review.get("calls", []):
                u = c.get("usage", {})
                self.usage["requests"] += u.get("requests", 0)
                self.usage["input_tokens"] += u.get("cache_hit", 0) + u.get("cache_miss", 0)
        return review

    def evaluate(self, harness_dir: Path | None, docs: list[str], tag: str) -> dict:
        """Verdicts on `docs` with this setup, scored against the labels (not-ready = positive)."""
        before = dict(self.usage)
        # each doc is reviewed in its own copy of the project, so they can run side by side
        with ThreadPoolExecutor(max(1, self.jobs)) as pool:
            verdicts = list(pool.map(lambda t: self.review(t, harness_dir, self.root / "eval" / tag / t).get("verdict"), docs))
        rows = [{"doc": t, "label": self.labels[t], "verdict": v} for t, v in zip(docs, verdicts)]
        done = [r for r in rows if r["verdict"] != "incomplete"]
        m, cm = mcc([r["verdict"] == "not-ready" for r in done], [r["label"] == "not-ready" for r in done])
        return {"mcc": m, **cm, "n": len(rows), "incomplete": len(rows) - len(done),
                "requests": self.usage["requests"] - before["requests"],
                "input_tokens": self.usage["input_tokens"] - before["input_tokens"], "rows": rows}


# ------------------------------------------------------------------------------------------
# The improver: Harness²'s Proposer, over /chat/completions or scripted.

EDIT_CONTRACT = """
You can change ONLY these files of the reviewer setup (paths relative to the setup):
  guardrails.md, systemprompt.md, LongTermMEMORY.md, tool_descriptions/tool_guidance.md
      -> "Reviewer guidance" given to every goldfish reviewer stage
  sub_agents/<stage>/AGENT.md, stage in comprehension | critic | zoom-out | readiness
      -> replaces that stage's persona; start it with a `description:` line
Each file at most 8000 characters. The reviewers' answer format, and every rule that turns their
answers into a verdict, are fixed elsewhere and cannot be changed from here.
Reply with only one JSON object: {"files": {"<path>": "<complete new content>"}, "notes": "<why, in one or two sentences>"}
Omitted files keep their current content.
"""


def _read_context(ctx: Path, limit: int = 60_000) -> str:
    parts, total = [], 0
    for p in sorted(ctx.rglob("*")):
        if not p.is_file() or p.suffix not in {".md", ".txt", ".json"}:
            continue
        text = p.read_text(errors="replace")[:6000]
        chunk = f"=== {p.relative_to(ctx).as_posix()} ===\n{text}\n"
        if total + len(chunk) > limit:
            break
        parts.append(chunk)
        total += len(chunk)
    return "".join(parts)


@dataclass
class TuneProposer(Proposer):
    """`mode` is "chat" (an OpenAI-compatible endpoint) or "scripted" (offline, deterministic)."""

    mode: str = "scripted"
    endpoint: Endpoint | None = None
    labels: dict = field(default_factory=dict)
    spent_tokens: int = 0

    # --- selection by ground truth: the doc's label decides which candidate is right ---
    def _select(self, ctx: Path, label: str) -> Reply:
        doc = next(iter(sorted((ctx / "source").glob("*.md"))), None)
        want = self.labels.get(doc.stem) if doc else None
        cands = sorted((ctx / "candidates").glob("*.md")) if (ctx / "candidates").is_dir() else []
        verdict = lambda p: (re.search(r"VERDICT: (\S+)", p.read_text()) or [None, ""])[1]
        chosen = next((p.stem for p in cands if verdict(p) == want), cands[0].stem if cands else "A")
        why = f"the candidate's verdict matches the document's label ({want})" if want else "no label; first candidate"
        return Reply(label=label, text=f"<why>{why}</why>\n```choice\n{chosen}\n```", ok=True, attempts=1)

    def _chat(self, system: str, user: str) -> str:
        assert self.endpoint is not None
        key = os.environ.get(self.endpoint.api_key_env, "")
        body = json.dumps({"model": self.endpoint.model, "temperature": 0,
                           "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}]}).encode()
        req = urllib.request.Request(self.endpoint.base_url.rstrip("/") + "/chat/completions", data=body,
                                     headers={"content-type": "application/json", **({"authorization": f"Bearer {key}"} if key else {})})
        with urllib.request.urlopen(req, timeout=600) as r:
            out = json.loads(r.read())
        self.spent_tokens += int((out.get("usage") or {}).get("total_tokens", 0))
        return out["choices"][0]["message"]["content"]

    @staticmethod
    def _apply(harness: Path, files: dict) -> list[str]:
        changed = []
        for rel, text in (files or {}).items():
            rel = tree.normalize_rel(rel)
            if not allowed(rel) or rel in STUBS or len(str(text)) > MAX_FILE_CHARS:
                continue   # silently outside the surface: the edit contract already said so
            (harness / rel).parent.mkdir(parents=True, exist_ok=True)
            (harness / rel).write_text(str(text))
            changed.append(rel)
        return changed

    def _scripted_edit(self, ctx: Path) -> tuple[dict, str]:
        """Deterministic stand-in for a model: add guidance for the defect classes the task's doc,
        or any library harness, points at. Like Harness²'s own demo editor, it encodes the fix."""
        docs = " ".join(p.read_text().lower() for p in (ctx / "source").glob("*.md"))
        current = (ctx / "harness" / "guardrails.md").read_text() if (ctx / "harness" / "guardrails.md").exists() else ""
        lib = " ".join(p.read_text() for p in (ctx / "library").rglob("guardrails.md")) if (ctx / "library").is_dir() else ""
        lines = [l for l in (current + "\n" + lib).splitlines() if l.startswith("- ")]
        if "notes.json" in docs and any(w in docs for w in ["gain", "field", "list"]):
            lines.append("- When a plan changes what is stored, check what happens to data written by earlier versions.")
        if any(w in docs for w in ["delete", "purge", "remove"]):
            lines.append("- When a plan deletes records, check that identifiers are never reused for new records.")
        uniq = list(dict.fromkeys(lines))
        if not uniq:
            return {}, "no change: nothing in this task points at a missing check"
        return {"guardrails.md": "Checks every reviewer applies:\n" + "\n".join(uniq) + "\n"}, "added review checks for the defect classes this task exposes"

    def call(self, mission, context_dir, label, **kwargs) -> Reply:
        ctx = Path(context_dir)
        if label.startswith("select."):
            r = self._select(ctx, label)
        elif label.startswith("experience."):
            if self.mode == "chat":
                text = self._chat("You review how a reviewer setup was improved and record lessons, in the exact format asked.",
                                  mission + "\n\n" + _read_context(ctx))
            else:
                g = ctx / "harness" / "guardrails.md"
                text = ("<edits>\ncomponent: guardrails.md\ncandidates: inv1\nchange: added review checks\n"
                        "observed: verdicts changed on documents with the targeted defect\nverdict: helped\n</edits>\n\n"
                        "<experience>\ncue: When reviewing design documents that change stored data or delete records\n"
                        "lesson: Keep explicit checks for old data and identifier reuse in guardrails.md.\n"
                        "evidence: guardrails.md row.\nconfidence: scripted run\n</experience>"
                        if g.exists() and g.read_text().strip() else "NO_ENTRY")
            r = Reply(label=label, text=text, ok=True, attempts=1)
        else:
            harness = ctx / "harness"
            if self.mode == "chat":
                raw = self._chat("You improve the setup of memoryless design-doc reviewers.\n" + EDIT_CONTRACT,
                                 mission + "\n\n" + _read_context(ctx))
                m = re.search(r"\{.*\}", raw, re.S)
                obj = json.loads(m.group(0)) if m else {}
                files, notes = obj.get("files", {}), obj.get("notes", "")
            else:
                files, notes = self._scripted_edit(ctx)
            changed = self._apply(harness, files)
            r = Reply(label=label, text=f"<notes>{notes} (changed: {', '.join(changed) or 'nothing'})</notes>",
                      ok=True, attempts=1, workspace=harness)
        self._calls.append(r)
        return r
