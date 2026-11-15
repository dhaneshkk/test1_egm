"""Head-to-head: egm-dsh's goldfish pipeline vs Google's official EGM skills,
both running on DeepSeek Harness, over the same design docs.

  A  egm-dsh adopt + egm-dsh goldfish        verdict from .egm-dsh/reviews/<slug>-1.json
  B  /egm-review (official, Step: uninterrupted) inside dsh
                                             verdict: not-ready iff consolidated_review_report.md has a 🔴 Blocker

Scores, with "not ready" as the positive class:
  * MCC of each pipeline against the labels (labels.json: slug -> ready | not-ready, or the richer form; see labels.py)
  * agreement MCC between the two pipelines
  * tokens and peak-hour USD per pipeline

Real run (needs DEEPSEEK_API_KEY; each official review is a long multi-subagent turn):
  python3 bench/compare.py --egm-dsh target/release/egm-dsh --upstream PATH/elephant-goldfish-model --dsh dsh

Spend only where it is needed:
  --egm-only         run egm-dsh's pipeline alone (the official one costs about 3x more per doc)
  --docs a,b,c       only these corpus docs (e.g. the ones a refused run left incomplete)

Billed cost: on a real run the script reads DeepSeek's account balance (GET /user/balance)
before and after, and reports the difference as what was billed, next to its own
token-count estimate. The difference covers everything the account was charged meanwhile,
and the balance can lag a few seconds, so the script re-reads it until it settles.

Plumbing self-test, no key (A uses the offline mock agent, B a scripted fake model
that writes reports matching the labels; it proves the harness, not either method):
  python3 bench/compare.py --selftest --egm-dsh target/release/egm-dsh --upstream PATH --dsh dsh
"""
import argparse
import json
import math
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

PRICES = {"deepseek-flash": (0.006, 0.30, 1.20), "deepseek-v4-pro": (0.044, 1.32, 3.96)}  # USD / 1M: hit, miss, output (peak)
N_CAL_FORMAL_BOUND = 29   # below this many labeled docs, treat MCC as noise


def mcc(pred: list[bool], truth: list[bool]) -> tuple[float | None, dict]:
    tp = sum(p and t for p, t in zip(pred, truth))
    tn = sum((not p) and (not t) for p, t in zip(pred, truth))
    fp = sum(p and not t for p, t in zip(pred, truth))
    fn = sum((not p) and t for p, t in zip(pred, truth))
    den = math.sqrt((tp + fp) * (tp + fn) * (tn + fp) * (tn + fn))
    return ((tp * tn - fp * fn) / den if den else None), {"tp": tp, "tn": tn, "fp": fp, "fn": fn}


def cost(model: str, u: dict) -> float:
    hit, miss, out = PRICES.get(model, PRICES["deepseek-v4-pro"])
    return (u["cache_hit"] * hit + u["cache_miss"] * miss + u["output"] * out) / 1e6


def sh(cmd, cwd, env, check=True):
    p = subprocess.run(cmd, cwd=cwd, env=env, text=True, capture_output=True)
    if check and p.returncode != 0:
        raise RuntimeError(f"{' '.join(map(str, cmd))} exited {p.returncode}:\n{p.stdout[-1500:]}\n{p.stderr[-1500:]}")
    return p


import threading
REFUSED = threading.Event()   # the provider refused (no credit, bad key): starting more docs is pointless


def refused(text: str) -> bool:
    t = text.lower()
    return "insufficient balance" in t or "no credit left" in t or "api key was rejected" in t or "authentication fails" in t


def read_balance(env) -> dict[str, float] | None:
    """{currency: total balance} from DeepSeek's GET /user/balance, or None if it cannot be read."""
    import urllib.request
    base = env.get("DEEPSEEK_BASE_URL", "https://api.deepseek.com").rstrip("/")
    req = urllib.request.Request(f"{base}/user/balance", headers={"Authorization": f"Bearer {env.get('DEEPSEEK_API_KEY', '')}"})
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            data = json.loads(r.read())
        return {i["currency"]: float(i["total_balance"]) for i in data.get("balance_infos", [])}
    except Exception:
        return None


def settled_balance(env, before, tries=4, wait=8.0):
    """Read the balance until two reads agree (billing can lag), at most `tries` times."""
    last = read_balance(env)
    for _ in range(tries - 1):
        if last is None:
            return None
        time.sleep(wait)
        now = read_balance(env)
        if now == last and now != before:
            return now
        last = now
    return last


def setup(base: Path, repo: Path, slug: str, doc: Path) -> Path:
    proj = base / slug
    shutil.copytree(repo, proj, ignore=shutil.ignore_patterns("__pycache__"))
    (proj / "docs" / "designs").mkdir(parents=True, exist_ok=True)
    shutil.copy2(doc, proj / "docs" / "designs" / f"{slug}.md")
    return proj


def run_a(proj: Path, slug: str, args, env) -> dict:
    """egm-dsh: adopt the doc, one goldfish review."""
    agent = ["--agent", "mock"] if args.selftest else ["--agent", "dsh", "--dsh-command", *args.dsh]
    sh([args.egm_dsh, "init", *agent, "--preset", args.preset, "--acceptance", "true"], proj, env)
    sh([args.egm_dsh, "adopt", slug, "--doc", f"docs/designs/{slug}.md"], proj, env)
    t0 = time.time()
    out = sh([args.egm_dsh, "goldfish", slug], proj, env, check=False)   # exits non-zero on NOT READY
    if refused(out.stdout + out.stderr):
        REFUSED.set()
    secs = time.time() - t0
    review_path = proj / ".egm-dsh" / "reviews" / f"{slug}-1.json"
    review = json.loads(review_path.read_text()) if review_path.exists() else {"verdict": "incomplete", "alarms": []}
    usage = {"cache_hit": 0, "cache_miss": 0, "output": 0, "requests": 0}
    usd = 0.0
    log = proj / ".egm-dsh" / "usage.jsonl"
    for line in (log.read_text().splitlines() if log.exists() else []):
        row = json.loads(line)
        for k in usage:
            usage[k] += row.get(k, 0)
        usd += cost(row.get("model", ""), row)
    alarms = review.get("alarms", [])
    kept = [a for a in alarms if a.get("status") != "dropped" and a.get("kind") != "note"]
    reasons = [a["text"] for a in kept]
    notes = [a["text"] for a in alarms if a.get("kind") == "note"]   # comprehension notes: shown, never blocking
    # every dropped alarm with who dropped it and why, so the next analysis needs no rerun
    dropped_detail = [{"source": a.get("source"), "kind": a.get("kind"), "text": a.get("text"), "by": a.get("dropped_by"),
                       "reason": a.get("reason"), "caught_by": a.get("caught_by")} for a in alarms if a.get("status") == "dropped"]
    # which stage raised each blocking reason, so false alarms can be traced to a stage
    sources = [{"source": a.get("source"), "kind": a.get("kind"), "section": a.get("section"), "grounding": a.get("grounding")} for a in kept]
    dropped = len(dropped_detail)
    return {"verdict": review["verdict"], "reasons": reasons, "sources": sources, "dropped": dropped,
            "notes": notes, "dropped_detail": dropped_detail,
            # per stage: tokens of each model call (output includes reasoning, when reported)
            "calls": [{"stage": c.get("stage"), "model": c.get("model"), **{k: (c.get("usage") or {}).get(k, 0)
                       for k in ("cache_hit", "cache_miss", "output", "reasoning", "requests")}} for c in review.get("calls", [])],
            "usage": usage, "usd_peak": usd, "seconds": round(secs, 1)}


def run_b(proj: Path, slug: str, args, env) -> dict:
    """Official EGM: install the plugin into the project and run /egm-review headless."""
    skills = official_dsh.install(proj, args.upstream)
    patch = proj.parent / f"{slug}.official.patch.yml"
    patch.write_text(official_dsh.patch_yaml(skills, model=args.official_model, effort=args.official_effort))
    t0 = time.time()
    out = official_dsh.run(proj, args.dsh, official_dsh.review_prompt(f"docs/designs/{slug}.md"), patch, env=env,
                           timeout=args.timeout)
    if out["error"] and refused(str(out["error"])):
        REFUSED.set()
    verdict, found = official_dsh.verdict(proj)
    if out["error"]:
        verdict = "incomplete"
    doc_after = (proj / "docs" / "designs" / f"{slug}.md").read_text()
    return {"verdict": verdict, "reasons": found, "usage": out["usage"], "usd_peak": cost(args.official_model, out["usage"]),
            "seconds": round(time.time() - t0, 1), "tool_calls": len(out["tools"]), "subagents": out["tools"].count("subagent"),
            "error": out["error"], "edited_doc": doc_after != (args.corpus / f"{slug}.md").read_text()}


def selftest_rules(docs: dict[str, Path], labels: dict) -> list:
    """Scripted official-pipeline replies: write a report that agrees with the label."""
    rules = []
    for slug in docs:
        key = f"TargetFile: docs/designs/{slug}.md"
        if labels.get(slug) == "not-ready":
            body = f"# Consolidated Review Report: {slug}\n\nLegend: 🔴 Blocker, 🟡 Major, 🟢 Nit\n\n## 🔴 Blockers\n\n1. 🔴 Scripted blocker for {slug}.\n"
        else:
            body = f"# Consolidated Review Report: {slug}\n\nLegend: 🔴 Blocker, 🟡 Major, 🟢 Nit\n\n## 🔴 Blockers\n\nNone.\n\n## 🟢 Nits\n\n- 🟢 Wording.\n"
        rules += [{"match": key, "tool": "write", "args": {"file_path": "consolidated_review_report.md", "content": body}},
                  {"match": key, "text": "Wrote consolidated_review_report.md"}]
    return rules


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
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--egm-dsh", required=True, type=lambda p: str(Path(p).resolve()))
    ap.add_argument("--upstream", required=True, type=lambda p: Path(p).resolve(), help="clone of google/elephant-goldfish-model")
    ap.add_argument("--dsh", nargs="+", required=True, help="command that launches dsh (put this flag last)")
    ap.add_argument("--corpus", type=lambda p: Path(p).resolve(), default=HERE / "corpus")
    ap.add_argument("--labels", type=Path, default=None, help="default: <corpus>/labels.json")
    ap.add_argument("--repo", type=lambda p: Path(p).resolve(), default=HERE.parent / "sample" / "before")
    ap.add_argument("--preset", default="balanced", choices=["balanced", "economy", "quality"])
    ap.add_argument("--official-model", default="deepseek-v4-pro")
    ap.add_argument("--official-effort", default="high")
    ap.add_argument("--timeout", type=int, default=3600, help="seconds for one official review")
    ap.add_argument("--jobs", type=int, default=4, help="docs reviewed at the same time (each doc runs both pipelines side by side)")
    ap.add_argument("--max-minutes", type=float, default=None,
                    help="stop starting new docs after this long; unfinished docs are reported as not run")
    ap.add_argument("--out", type=Path, default=Path("compare_results.json"))
    ap.add_argument("--selftest", action="store_true")
    ap.add_argument("--egm-only", action="store_true", help="run only egm-dsh's pipeline (no official skill)")
    ap.add_argument("--docs", type=lambda s: [d.strip() for d in s.split(",") if d.strip()], default=None,
                    help="comma-separated corpus doc names to run (default: all)")
    ap.add_argument("--balance-wait", type=float, default=8.0, help="seconds between balance reads after the run")
    args = ap.parse_args()

    labels_path = args.labels or args.corpus / "labels.json"
    import labels as labels_file
    labels = labels_file.plain(labels_path)
    docs = {p.stem: p for p in sorted(args.corpus.glob("*.md"))}
    if args.docs:
        unknown = [d for d in args.docs if d not in docs]
        if unknown:
            sys.exit(f"unknown doc(s): {', '.join(unknown)}; the corpus has: {', '.join(docs)}")
        docs = {d: docs[d] for d in args.docs}
    work = Path(tempfile.mkdtemp(prefix="egm-compare-"))
    env = dict(os.environ, NO_COLOR="1")
    server = None
    if args.selftest:
        (work / "rules.json").write_text(json.dumps(selftest_rules(docs, labels)))
        port = free_port()
        server = subprocess.Popen([sys.executable, "-I", str(HERE.parent / "tests" / "e2e" / "fake_deepseek.py"), str(port), str(work / "rules.json"), str(work)])
        wait_port(port)
        env.update(DEEPSEEK_API_KEY="fake", DEEPSEEK_BASE_URL=f"http://127.0.0.1:{port}")
    elif not os.environ.get("DEEPSEEK_API_KEY"):
        sys.exit("set DEEPSEEK_API_KEY for a real run, or pass --selftest")

    if args.selftest:   # a fake account the fake model debits per request, to test the billed-cost report
        (work / "balance.json").write_text(json.dumps({"is_available": True, "debit_per_request": 0.01,
                                                       "balance_infos": [{"currency": "USD", "total_balance": "5.00"}]}))
    balance_before = read_balance(env)
    rows, not_run = [], []
    deadline = time.time() + args.max_minutes * 60 if args.max_minutes else None
    from concurrent.futures import ThreadPoolExecutor

    def one(slug: str, doc: Path):
        """Both pipelines on one doc, side by side (separate copies of the repo)."""
        if (deadline and time.time() > deadline) or REFUSED.is_set():
            not_run.append(slug)
            return None
        # an official review may not outlive the deadline by more than a minute
        b_args = argparse.Namespace(**{**vars(args), "timeout": int(min(args.timeout, max(60, deadline - time.time()))) if deadline else args.timeout})
        if args.egm_only:
            a = run_a(setup(work / "A", args.repo, slug, doc), slug, args, env)
            print(f"· {slug:<24} egm-dsh {a['verdict']:<11} label {labels.get(slug, '-')}", flush=True)
            return {"doc": slug, "label": labels.get(slug), "egm_dsh": a}
        with ThreadPoolExecutor(2) as pair:
            fa = pair.submit(run_a, setup(work / "A", args.repo, slug, doc), slug, args, env)
            fb = pair.submit(run_b, setup(work / "B", args.repo, slug, doc), slug, b_args, env)
            a, b = fa.result(), fb.result()
        print(f"· {slug:<24} egm-dsh {a['verdict']:<11} official {b['verdict']:<11} label {labels.get(slug, '-')}", flush=True)
        return {"doc": slug, "label": labels.get(slug), "egm_dsh": a, "official": b}

    try:
        with ThreadPoolExecutor(max(1, args.jobs)) as pool:
            rows = [r for r in pool.map(lambda kv: one(*kv), docs.items()) if r]
        balance_after = settled_balance(env, balance_before, wait=args.balance_wait) if balance_before is not None else None
    finally:
        if server:
            server.terminate()

    pipelines = ["egm_dsh"] if args.egm_only else ["egm_dsh", "official"]
    complete = [r for r in rows if all(r[p]["verdict"] != "incomplete" for p in pipelines)]
    pos = lambda v: v == "not-ready"
    summary = {"mode": "selftest (scripted model; plumbing only)" if args.selftest else "real",
               "pipelines": pipelines, "docs": len(rows), "complete": len(complete),
               "incomplete": [r["doc"] for r in rows if r not in complete],
               "not_run (time limit)": sorted(not_run) if not REFUSED.is_set() else [],
               "not_run (provider refused)": sorted(not_run) if REFUSED.is_set() else []}
    agree = None
    if not args.egm_only:
        a_pred = [pos(r["egm_dsh"]["verdict"]) for r in complete]
        b_pred = [pos(r["official"]["verdict"]) for r in complete]
        agree, agree_cm = mcc(a_pred, b_pred)
        summary["agreement"] = {"mcc": agree, "confusion (egm-dsh vs official)": agree_cm,
                                "same verdict": sum(x == y for x, y in zip(a_pred, b_pred))}
    labeled = [r for r in complete if r["label"] in ("ready", "not-ready")]
    if labeled:
        truth = [pos(r["label"]) for r in labeled]
        for name in pipelines:
            m, cm = mcc([pos(r[name]["verdict"]) for r in labeled], truth)
            summary[f"{name} vs labels"] = {"mcc": m, **cm, "n": len(labeled)}
    estimate = 0.0
    for name in pipelines:
        u = {k: sum(r[name]["usage"][k] for r in rows) for k in ["cache_hit", "cache_miss", "output", "requests"]}
        summary[f"{name} cost"] = {**u, "usd_peak": round(sum(r[name]["usd_peak"] for r in rows), 4),
                                   "seconds": round(sum(r[name]["seconds"] for r in rows), 1)}
        estimate += summary[f"{name} cost"]["usd_peak"]
    # What the account was actually charged: the balance before minus after.
    if balance_before is not None and balance_after is not None:
        billed = {c: round(balance_before[c] - balance_after.get(c, balance_before[c]), 4) for c in balance_before}
        summary["billed (balance before - after)"] = billed
        if "USD" in billed and estimate > 0:
            summary["billed / estimate (USD)"] = round(billed["USD"] / estimate, 2)
    else:
        summary["billed (balance before - after)"] = None
    if not args.egm_only and any(r["official"].get("edited_doc") for r in rows):
        summary["warning"] = "the official pipeline edited at least one design doc (its remediation step); verdicts are from the report"
    args.out.write_text(json.dumps({"summary": summary, "rows": rows}, indent=1, ensure_ascii=False) + "\n")

    if REFUSED.is_set():
        print("\nSTOPPED: the provider refused the requests (insufficient balance or a rejected key); "
              "the remaining docs were not started. Top up at https://platform.deepseek.com/top_up and rerun.")
    fmt = lambda m: "undefined (one class only)" if m is None else f"{m:+.3f}"
    what = "egm-dsh verdicts" if args.egm_only else "both verdicts"
    print(f"\n{summary['mode']}: {len(complete)}/{len(rows)} docs with {what}")
    if not args.egm_only:
        print(f"agreement MCC (egm-dsh vs official): {fmt(agree)}   same verdict on {summary['agreement']['same verdict']}/{len(complete)}")
    for name in pipelines:
        if f"{name} vs labels" in summary:
            s = summary[f"{name} vs labels"]
            print(f"{name:<9} vs labels: MCC {fmt(s['mcc'])}  (tp {s['tp']} fp {s['fp']} fn {s['fn']} tn {s['tn']})")
    for name in pipelines:
        c = summary[f"{name} cost"]
        print(f"{name:<9} tokens: hit {c['cache_hit']:>9} miss {c['cache_miss']:>9} out {c['output']:>8}  ${c['usd_peak']:.4f} estimated (peak)  {c['seconds']}s")
    billed = summary["billed (balance before - after)"]
    if billed is None:
        print("billed: could not read the DeepSeek balance before and after; only the estimate above is available")
    else:
        ratio = summary.get("billed / estimate (USD)")
        print("billed (balance before - after): " + ", ".join(f"{v:.4f} {c}" for c, v in billed.items())
              + (f"   = {ratio}x the estimate" if ratio is not None else ""))
    if len(labeled) < N_CAL_FORMAL_BOUND:
        print(f"note: {len(labeled)} labeled docs (< {N_CAL_FORMAL_BOUND}); treat every MCC above as noise until the corpus grows")
    print(f"details: {args.out}")
    print("SUMMARY " + json.dumps(summary, ensure_ascii=False))


if __name__ == "__main__":
    main()
