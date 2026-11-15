"""End-to-end test of the head-to-head's spending controls (bench/compare.py), in its
self-test mode: egm-dsh on the mock agent, the official skill on real dsh with a
scripted fake model, and a fake DeepSeek account the fake model debits $0.01 per request.

  * --docs runs only the named docs; an unknown name is refused before anything runs
  * --egm-only runs no official review at all (no model request, nothing billed)
  * the billed cost is read from the account balance before and after, next to the estimate

usage: python3 run_compare_flags_e2e.py EGM_BINARY UPSTREAM_CLONE DSH_COMMAND...
"""
import json
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
COMPARE = HERE.parent.parent / "bench" / "compare.py"
EGM, UPSTREAM, DSH = str(Path(sys.argv[1]).resolve()), sys.argv[2], sys.argv[3:]


def run(*extra):
    out = Path(tempfile.mkdtemp(prefix="egm-cmpflags-")) / "r.json"
    p = subprocess.run([sys.executable, str(COMPARE), "--selftest", "--egm-dsh", EGM, "--upstream", UPSTREAM,
                        "--balance-wait", "1", "--jobs", "2", "--out", str(out), *extra, "--dsh", *DSH],
                       text=True, capture_output=True, timeout=1200)
    print(f"$ compare.py {' '.join(extra)}\n{(p.stdout + p.stderr)[-1500:]}\n", flush=True)
    return p, (json.loads(out.read_text()) if out.exists() else None)


def main():
    two = "delete-stable-ids,search-complete"
    p_full, full = run("--docs", two)
    p_egm, egm = run("--egm-only", "--docs", two)
    p_bad, _ = run("--docs", "no-such-doc")
    s_full, s_egm = full["summary"], egm["summary"]
    checks = {
        "--docs: only the named docs ran": sorted(r["doc"] for r in full["rows"]) == sorted(two.split(","))
            and sorted(r["doc"] for r in egm["rows"]) == sorted(two.split(",")),
        "--docs: an unknown doc is refused before anything runs, naming the corpus": p_bad.returncode != 0
            and "unknown doc(s): no-such-doc" in p_bad.stderr and "search-complete" in p_bad.stderr,
        "both pipelines: official verdicts and agreement are reported": s_full["pipelines"] == ["egm_dsh", "official"]
            and all("official" in r for r in full["rows"]) and "agreement" in s_full,
        "--egm-only: no official review, no agreement, egm-dsh scored": s_egm["pipelines"] == ["egm_dsh"]
            and all("official" not in r for r in egm["rows"]) and "agreement" not in s_egm and "official cost" not in s_egm
            and "egm_dsh vs labels" in s_egm,
        "billed: the balance difference is reported next to the estimate": s_full["billed (balance before - after)"]["USD"] > 0
            and "billed (balance before - after)" in p_full.stdout and "x the estimate" in p_full.stdout,
        "billed: the official run's requests are what was charged ($0.01 each)": abs(
            s_full["billed (balance before - after)"]["USD"] - 0.01 * s_full["official cost"]["requests"]) < 1e-6,
        "--egm-only: nothing billed (the mock egm-dsh side makes no request)": s_egm["billed (balance before - after)"]["USD"] == 0,
        "per-reason sources are recorded for egm-dsh": all(len(r["egm_dsh"]["sources"]) == len(r["egm_dsh"]["reasons"]) for r in full["rows"]),
    }
    for name, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + name)
    print("SUMMARY " + json.dumps({"passed": sum(map(bool, checks.values())), "failed": sum(not v for v in checks.values())}))
    if not all(checks.values()):
        sys.exit(1)


if __name__ == "__main__":
    main()
