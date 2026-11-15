"""Tune egm-dsh's goldfish reviewers with Harness², then decide adoption on held-out docs.

  1. Harness² task-level recursion (Propose-Probe-Compose, parallel, budget k) runs on the
     TRAINING docs. Selection uses each doc's label; the library carries lessons forward.
  2. Candidates = the base setup + every setup Harness² adopted. Pick one by TRAINING MCC
     (ties: fewer guidance characters, i.e. cheaper prompts).
  3. Gate, on HELD-OUT docs never seen in step 1 (mirrors `adopt` in proofs/EgmTune.lean):
        held-out MCC(tuned) > held-out MCC(base)
        goldfish input tokens(tuned) <= 1.25 x tokens(base)   (guidance is paid on every request)
        egm-dsh accepts the setup (prompt text only; src/tuning.rs)
        and a human approves (--approve)
  4. Adoption copies the setup into PROJECT/.egm-dsh/setups/<fingerprint>/ and points
     `goldfish_harness` at it. Undo: delete that config line.

Offline (default): the goldfish model is bench/tune/persona_reviewer.py and the improver is
scripted. That shows the mechanism end to end; it says nothing about gains on a real model.
Real: --reviewer deepseek --improver chat (needs DEEPSEEK_API_KEY).

usage: python3 bench/tune/tune.py --egm-dsh target/release/egm-dsh [--k 1] [--out runs/tune]
          [--reviewer scripted|deepseek] [--improver scripted|chat] [--approve --adopt-into PROJECT]
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from harness2.core.schedule import run_domain  # noqa: E402

from egm_tune import (CORPUS, Endpoint, GoldfishTuneAdapter, TuneProposer, clean_setup,  # noqa: E402
                      fingerprint)

N_CAL_FORMAL_BOUND = 29
COST_SLACK = 1.25


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


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def guidance_chars(d: Path | None) -> int:
    return 0 if d is None else sum(len(p.read_text()) for p in d.rglob("*.md") if "README" not in p.name)


def adopt_ok(base: dict, tuned: dict, accepted_by_egm: bool, approved: bool) -> tuple[bool, list[str]]:
    """The adoption rule. Mirrors `adopt` in proofs/EgmTune.lean (MCC compared x1000 as integers there)."""
    why = []
    if tuned["mcc"] is None or base["mcc"] is None:
        why.append("held-out MCC undefined (a class is missing)")
    elif not tuned["mcc"] > base["mcc"]:
        why.append(f"held-out MCC did not improve ({tuned['mcc']:+.3f} vs base {base['mcc']:+.3f})")
    if tuned["input_tokens"] > COST_SLACK * max(base["input_tokens"], 1):
        why.append(f"costs more: {tuned['input_tokens']} input tokens vs {base['input_tokens']} (limit x{COST_SLACK})")
    if not accepted_by_egm:
        why.append("egm-dsh refused the setup")
    if not approved:
        why.append("not approved (pass --approve after reading the setup)")
    return not why, why


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--egm-dsh", required=True, type=lambda p: str(Path(p).resolve()))
    ap.add_argument("--k", type=int, default=1)
    ap.add_argument("--jobs", type=int, default=4, help="docs scored at the same time")
    ap.add_argument("--out", type=Path, default=Path("runs/tune"))
    ap.add_argument("--reviewer", choices=["scripted", "deepseek"], default="scripted")
    ap.add_argument("--reviewer-model", default="deepseek-flash")
    ap.add_argument("--improver", choices=["scripted", "chat"], default="scripted")
    ap.add_argument("--improver-url", default=os.environ.get("TUNE_IMPROVER_URL", "https://api.deepseek.com"))
    ap.add_argument("--improver-model", default="deepseek-v4-pro")
    ap.add_argument("--approve", action="store_true")
    ap.add_argument("--adopt-into", type=Path, default=None)
    a = ap.parse_args()

    split = json.loads((HERE / "split.json").read_text())
    sys.path.insert(0, str(CORPUS.parent))
    import labels as labels_file
    labels = labels_file.plain(CORPUS / "labels.json")
    root = a.out.resolve()
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True)
    server = None
    if a.reviewer == "scripted":
        port = free_port()
        server = subprocess.Popen([sys.executable, "-I", str(HERE / "persona_reviewer.py"), str(port), str(root / "reviewer.jsonl")])
        wait_port(port)
        reviewer = Endpoint(f"http://127.0.0.1:{port}", a.reviewer_model, api_key_env="")
    else:
        if not os.environ.get("DEEPSEEK_API_KEY"):
            sys.exit("--reviewer deepseek needs DEEPSEEK_API_KEY")
        reviewer = Endpoint("https://api.deepseek.com", a.reviewer_model)
    if a.improver == "chat" and not os.environ.get("DEEPSEEK_API_KEY") and "api.deepseek.com" in a.improver_url:
        sys.exit("--improver chat needs DEEPSEEK_API_KEY (or --improver-url to another OpenAI-compatible endpoint)")

    try:
        adapter = GoldfishTuneAdapter(root, a.egm_dsh, reviewer, split["train"], labels)
        adapter.jobs = a.jobs
        proposer = TuneProposer(adapter.spec.channels["proposer"], mode=a.improver, labels=labels,
                                endpoint=Endpoint(a.improver_url, a.improver_model))
        # 1. Harness², unmodified: task-level recursion + library, parallel mode.
        t0 = time.time()
        lib = root / "library" / f"k{a.k}"
        result = run_domain("design-review", adapter.tasks(), a.k, adapter=adapter, prop=proposer,
                            runs_root=adapter.spec.runs_root, library_root=lib, modes=("agg",))
        h2 = {"done": result.get("done"), "failed": result.get("failed"), "executions": adapter.execution_count,
              "improver_calls": len(proposer._calls), "seconds": round(time.time() - t0, 1)}
        print(f"Harness²: {h2['done']} tasks, {h2['failed']} failed, {h2['executions']} executions, {h2['improver_calls']} improver calls")

        # 2. Candidates and selection by TRAINING MCC.
        adopted = sorted((lib / "agg" / "design-review" / "harnesses").glob("*/harness"))
        cands: dict[str, Path | None] = {"base": None}
        seen = set()
        for d in adopted:
            fp = fingerprint(d)
            if fp not in seen:
                seen.add(fp)
                cands[f"after:{d.parent.name}"] = d
        train = {name: adapter.evaluate(d, split["train"], f"train-{i}") for i, (name, d) in enumerate(cands.items())}
        score = lambda n: (train[n]["mcc"] if train[n]["mcc"] is not None else -2, -guidance_chars(cands[n]))
        best = max(cands, key=score)
        print("training MCC: " + ", ".join(f"{n} {'undefined' if train[n]['mcc'] is None else round(train[n]['mcc'], 3)}" for n in cands)
              + f"  -> {best}")

        # 3. Held-out gate.
        held_base = adapter.evaluate(None, split["held_out"], "held-base")
        held_best = held_base if best == "base" else adapter.evaluate(cands[best], split["held_out"], "held-tuned")
        accepted = True
        if cands[best] is not None:
            check = subprocess.run([a.egm_dsh, "--help"], capture_output=True)   # binary present
            try:
                clean_setup(cands[best], root / "candidate-setup")
            except ValueError:
                accepted = False
            accepted = accepted and check.returncode == 0
        ok, why = adopt_ok(held_base, held_best, accepted, a.approve) if best != "base" else (False, ["no setup beat the base on the training docs"])
        fmt = lambda m: "undefined" if m is None else f"{m:+.3f}"
        print(f"held-out MCC: base {fmt(held_base['mcc'])}  tuned {fmt(held_best['mcc'])}  "
              f"(n={held_base['n']}; input tokens base {held_base['input_tokens']}, tuned {held_best['input_tokens']})")
        if held_base["n"] < N_CAL_FORMAL_BOUND:
            print(f"note: {held_base['n']} held-out docs (< {N_CAL_FORMAL_BOUND}); a pilot needs more labelled docs before trusting this")

        # 4. Adoption.
        adopted_to = None
        if ok and a.adopt_into:
            fp = fingerprint(cands[best])
            dest = a.adopt_into.resolve() / ".egm-dsh" / "setups" / fp
            clean_setup(cands[best], dest)
            cfg = a.adopt_into.resolve() / ".egm-dsh" / "config.json"
            c = json.loads(cfg.read_text())
            c["goldfish_harness"] = f".egm-dsh/setups/{fp}"
            cfg.write_text(json.dumps(c, indent=2) + "\n")
            adopted_to = str(dest)
        if best != "base":
            print("chosen setup:")
            for p in sorted(cands[best].rglob("*.md")):
                if "README" not in p.name and p.read_text().strip():
                    print(f"  --- {p.relative_to(cands[best])}\n" + "".join(f"  {l}\n" for l in p.read_text().splitlines()))
        verdict = "ADOPTED" if adopted_to else ("would adopt (pass --adopt-into PROJECT)" if ok else "not adopted: " + "; ".join(why))
        print(verdict)
        report = {"mode": {"reviewer": a.reviewer, "improver": a.improver, "k": a.k}, "harness2": h2,
                  "candidates": {n: str(d) if d else None for n, d in cands.items()}, "train": train, "chosen": best,
                  "held_out": {"base": held_base, "tuned": held_best}, "adopt": ok, "why_not": why, "adopted_to": adopted_to,
                  "improver_tokens": proposer.spent_tokens}
        (root / "tune_report.json").write_text(json.dumps(report, indent=1, default=str) + "\n")
        print("SUMMARY " + json.dumps({"harness2_ok": h2["failed"] == 0, "chosen": best, "train_mcc": train[best]["mcc"],
                                       "held_base": held_base["mcc"], "held_tuned": held_best["mcc"], "adopt": ok,
                                       "adopted": bool(adopted_to), "executions": h2["executions"]}))
        return 0
    finally:
        if server:
            server.terminate()


if __name__ == "__main__":
    raise SystemExit(main())
