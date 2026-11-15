"""Evaluate egm-dsh's goldfish against the labelled corpus, with numbers you can trust:

  * every doc is reviewed --repeats times, each in a fresh project, so run-to-run
    variance is measured instead of hidden: the flip rate is the share of docs whose
    verdict changed between repeats
  * the score is MCC ("not ready" is the positive class) of each doc's majority verdict,
    with a 95% bootstrap confidence interval (docs resampled with replacement, 2000
    times, fixed seed), plus the MCC of each single repeat
  * by split: `dev` docs were used to tune egm-dsh, `test` docs never were; report test
  * output and reasoning tokens per stage, the estimated cost and, on a real run, what
    DeepSeek billed (account balance before and after)
  * below 29 labelled docs the MCC is flagged as noise

Real run (needs DEEPSEEK_API_KEY). About $0.08 per doc and repeat at peak rates:
  python3 bench/evaluate.py --egm-dsh target/release/egm-dsh --dsh dsh --split test --repeats 3
Spend control: --docs a,b (only these), --max-usd X (start no new review once the
estimate passes X).

Plumbing self-test, no key (egm-dsh's offline mock agent; proves the harness, not the method):
  python3 bench/evaluate.py --selftest --egm-dsh target/release/egm-dsh --dsh dsh
"""
import argparse
import json
import os
import random
import sys
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import compare  # noqa: E402
import labels as labels_file  # noqa: E402

N_CAL_FORMAL_BOUND = compare.N_CAL_FORMAL_BOUND


def majority(verdicts: list[str]) -> str | None:
    """not-ready when at least half the complete repeats say so (a tie errs on caution)."""
    done = [v for v in verdicts if v in ("ready", "not-ready")]
    if not done:
        return None
    return "not-ready" if 2 * sum(v == "not-ready" for v in done) >= len(done) else "ready"


def score(pairs: list[tuple[str, str]]) -> tuple[float | None, dict]:
    """MCC of (verdict, label) pairs, not-ready positive."""
    return compare.mcc([v == "not-ready" for v, _ in pairs], [l == "not-ready" for _, l in pairs])


def bootstrap(pairs: list[tuple[str, str]], n=2000, seed=0) -> tuple[float, float] | None:
    """95% percentile interval of MCC over doc resamples; resamples where MCC is undefined are skipped."""
    if len(pairs) < 2:
        return None
    rng = random.Random(seed)
    vals = []
    for _ in range(n):
        m, _ = score([pairs[rng.randrange(len(pairs))] for _ in pairs])
        if m is not None:
            vals.append(m)
    if len(vals) < n // 2:
        return None
    vals.sort()
    return vals[int(0.025 * len(vals))], vals[min(len(vals) - 1, int(0.975 * len(vals)))]


def summarize(rows: list[dict], labels: dict[str, dict], repeats: int) -> dict:
    by_doc: dict[str, list[str]] = {}
    for r in rows:
        by_doc.setdefault(r["doc"], [None] * repeats)[r["repeat"]] = r["verdict"]
    out = {}
    for split in ("dev", "test", "all"):
        docs = [d for d in by_doc if split == "all" or labels[d]["split"] == split]
        if not docs:
            continue
        pairs = [(majority(by_doc[d]), labels[d]["label"]) for d in docs]
        pairs = [p for p in pairs if p[0]]
        m, conf = score(pairs)
        multi = [d for d in docs if sum(v in ("ready", "not-ready") for v in by_doc[d]) >= 2]
        flipped = [d for d in multi if len({v for v in by_doc[d] if v in ("ready", "not-ready")}) > 1]
        per_repeat = []
        for k in range(repeats):
            pk = [(by_doc[d][k], labels[d]["label"]) for d in docs if by_doc[d][k] in ("ready", "not-ready")]
            per_repeat.append(score(pk)[0] if pk else None)
        ci = bootstrap(pairs)
        out[split] = {"docs": len(docs), "scored": len(pairs), "mcc (majority verdict)": m, "confusion": conf,
                      "mcc 95% CI": list(ci) if ci else None, "mcc per repeat": per_repeat,
                      "flip rate": (len(flipped) / len(multi)) if multi else None, "flipped": flipped,
                      "incomplete runs": sum(v not in ("ready", "not-ready") for d in docs for v in by_doc[d]),
                      "noise (< 29 docs)": len(pairs) < N_CAL_FORMAL_BOUND}
    # output where it goes: per stage, mean per call
    stages: dict[str, dict] = {}
    for r in rows:
        for c in r.get("calls", []):
            s = stages.setdefault(c["stage"], {"calls": 0, "output": 0, "reasoning": 0, "input": 0})
            s["calls"] += 1
            s["output"] += c["output"]
            s["reasoning"] += c["reasoning"]
            s["input"] += c["cache_hit"] + c["cache_miss"]
    out["tokens per stage (mean per call)"] = {k: {"calls": v["calls"], "output": round(v["output"] / v["calls"]),
                                                   "reasoning": round(v["reasoning"] / v["calls"]), "input": round(v["input"] / v["calls"])}
                                               for k, v in sorted(stages.items())}
    out["by doc"] = {d: {"label": labels[d]["label"], "split": labels[d]["split"], "verdicts": v} for d, v in sorted(by_doc.items())}
    return out


def fmt(m):
    return "undefined" if m is None else f"{m:+.3f}"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--egm-dsh", required=True, type=lambda p: str(Path(p).resolve()))
    ap.add_argument("--dsh", nargs="+", required=True, help="command that launches dsh (put this flag last)")
    ap.add_argument("--corpus", type=lambda p: Path(p).resolve(), default=HERE / "corpus")
    ap.add_argument("--repo", type=lambda p: Path(p).resolve(), default=HERE.parent / "sample" / "before")
    ap.add_argument("--preset", default="balanced", choices=["balanced", "economy", "quality"])
    ap.add_argument("--split", default="all", choices=["dev", "test", "all"])
    ap.add_argument("--repeats", type=int, default=3)
    ap.add_argument("--docs", type=lambda s: [d.strip() for d in s.split(",") if d.strip()], default=None)
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--max-usd", type=float, default=None, help="start no new review once the estimated spend passes this")
    ap.add_argument("--balance-wait", type=float, default=8.0)
    ap.add_argument("--out", type=Path, default=Path("evaluate_results.json"))
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args()

    labels = labels_file.load(args.corpus / "labels.json")
    docs = {p.stem: p for p in sorted(args.corpus.glob("*.md")) if p.stem in labels}
    if args.docs:
        unknown = [d for d in args.docs if d not in docs]
        if unknown:
            sys.exit(f"unknown doc(s): {', '.join(unknown)}; the corpus has: {', '.join(docs)}")
        docs = {d: docs[d] for d in args.docs}
    docs = {d: p for d, p in docs.items() if args.split == "all" or labels[d]["split"] == args.split}
    env = dict(os.environ, NO_COLOR="1")
    real = not args.selftest
    if real and not env.get("DEEPSEEK_API_KEY"):
        sys.exit("set DEEPSEEK_API_KEY (or use --selftest)")
    est = len(docs) * args.repeats * 0.08
    print(f"{len(docs)} docs ({args.split}) x {args.repeats} repeats = {len(docs) * args.repeats} reviews"
          + (f", about ${est:.2f} at peak rates" if real else " (self-test, mock agent)"), flush=True)
    before = compare.read_balance(env) if real else None

    work = Path(tempfile.mkdtemp(prefix="egm-evaluate-"))
    spent = [0.0]
    lock = threading.Lock()
    a_args = argparse.Namespace(egm_dsh=args.egm_dsh, dsh=args.dsh, preset=args.preset, selftest=args.selftest)

    def one(job):
        rep, slug = job
        with lock:
            if (args.max_usd is not None and spent[0] >= args.max_usd) or compare.REFUSED.is_set():
                return {"doc": slug, "repeat": rep, "verdict": "not run"}
        proj = compare.setup(work / f"r{rep}", args.repo, slug, docs[slug])
        r = compare.run_a(proj, slug, a_args, env)
        with lock:
            spent[0] += r["usd_peak"]
        print(f"· {slug:24} repeat {rep + 1}: {r['verdict']:10} (label {labels[slug]['label']})  ${r['usd_peak']:.4f}  {r['seconds']}s", flush=True)
        return {"doc": slug, "repeat": rep, **r}

    jobs = [(rep, slug) for rep in range(args.repeats) for slug in docs]   # repeat-major: a stopped run still covers every doc once
    (work / "r0").mkdir(parents=True, exist_ok=True)
    for rep in range(args.repeats):
        (work / f"r{rep}").mkdir(parents=True, exist_ok=True)
    with ThreadPoolExecutor(max_workers=args.jobs) as ex:
        rows = list(ex.map(one, jobs))

    summary = summarize(rows, labels, args.repeats)
    summary["estimated USD (peak)"] = round(spent[0], 4)
    if real:
        after = compare.settled_balance(env, before, wait=args.balance_wait)
        if before and after:
            billed = sum(before.values()) - sum(after.values())
            summary["billed (balance before - after)"] = round(billed, 4)
    args.out.write_text(json.dumps({"summary": summary, "rows": rows, "labels": labels}, indent=1))

    for split in ("dev", "test", "all"):
        s = summary.get(split)
        if not s:
            continue
        ci = s["mcc 95% CI"]
        print(f"\n{split}: {s['scored']}/{s['docs']} docs scored  MCC {fmt(s['mcc (majority verdict)'])}"
              f"  95% CI {('[' + fmt(ci[0]) + ', ' + fmt(ci[1]) + ']') if ci else 'n/a'}  {s['confusion']}")
        print(f"  per repeat: {', '.join(fmt(m) for m in s['mcc per repeat'])}   flip rate: "
              f"{'n/a' if s['flip rate'] is None else f'{s['flip rate']:.0%}'} {s['flipped'] or ''}"
              f"{'   incomplete runs: ' + str(s['incomplete runs']) if s['incomplete runs'] else ''}")
        if s["noise (< 29 docs)"]:
            print(f"  note: fewer than {N_CAL_FORMAL_BOUND} docs scored; treat this MCC as noise")
    print("\ntokens per stage (mean per call):")
    for k, v in summary["tokens per stage (mean per call)"].items():
        print(f"  {k:14} {v['calls']:4} calls  output {v['output']:6}  of which reasoning {v['reasoning']:6}  input {v['input']:7}")
    print(f"\nestimated ${summary['estimated USD (peak)']:.2f} (peak)" + (f"; billed ${summary['billed (balance before - after)']:.2f}" if "billed (balance before - after)" in summary else ""))
    print("details: " + str(args.out.resolve()))
    print("SUMMARY " + json.dumps({k: summary[k] for k in ("dev", "test", "all") if k in summary} | {"estimated USD (peak)": summary["estimated USD (peak)"]}, default=str)[:4000])


if __name__ == "__main__":
    main()
