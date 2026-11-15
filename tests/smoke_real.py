"""Real-model smoke run: the jot loop with real DeepSeek models through real
DeepSeek Harness, inside egm-dsh's sandbox. Needs DEEPSEEK_API_KEY.

It does not expect a real model to follow the script exactly. It measures and
fails only on things that must hold whatever the model does:

  fail  egm-dsh crashed, a goldfish review came back incomplete (unparseable
        answers), a builder changed files outside the map without the build
        being rejected, or the run cost more than --max-usd
  record cost per role, requests, cache-hit share, alarms raised / dropped by
        grounding / dropped by the verifier / confirmed, verdicts, phase reached

usage: python3 tests/smoke_real.py EGM_BINARY [--preset economy] [--max-usd 1.0] [--out smoke.json] [--dsh-command dsh]
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("egm")
    ap.add_argument("--preset", default="economy", choices=["economy", "balanced", "quality"])
    ap.add_argument("--max-usd", type=float, default=1.0)
    ap.add_argument("--out", type=Path, default=Path("smoke_real.json"))
    ap.add_argument("--dsh-command", nargs="+", default=None)
    ap.add_argument("--agent", default="dsh", choices=["dsh", "mock"], help="mock: offline self-test of this script")
    a = ap.parse_args()
    if a.agent == "dsh" and not os.environ.get("DEEPSEEK_API_KEY"):
        print("SKIP: DEEPSEEK_API_KEY is not set")
        return
    work = Path(tempfile.mkdtemp(prefix="egm-dsh-smoke-"))
    proj = work / "jot"
    cmd = [a.egm, "demo", "--dir", str(proj), "--agent", a.agent, "--preset", a.preset]
    if a.dsh_command:
        cmd += ["--dsh-command", *a.dsh_command]
    p = subprocess.run(cmd, text=True, capture_output=True, env=dict(os.environ, NO_COLOR="1"), timeout=3600)
    log = p.stdout + p.stderr
    (work / "demo.log").write_text(log)
    print(log[-4000:])

    egm = proj / ".egm-dsh"
    usage = [json.loads(l) for l in (egm / "usage.jsonl").read_text().splitlines()] if (egm / "usage.jsonl").exists() else []
    prices = json.loads((egm / "config.json").read_text()).get("prices", {}) if (egm / "config.json").exists() else {}

    def usd(row):
        pr = prices.get(row.get("model"), {})
        return (row.get("cache_hit", 0) * pr.get("cache_hit", 0) + row.get("cache_miss", 0) * pr.get("cache_miss", 0)
                + row.get("output", 0) * pr.get("output", 0)) / 1e6

    by_role = {}
    for row in usage:
        r = by_role.setdefault(row["role"], {"requests": 0, "cache_hit": 0, "cache_miss": 0, "output": 0, "usd_peak": 0.0})
        for k in ["requests", "cache_hit", "cache_miss", "output"]:
            r[k] += row.get(k, 0)
        r["usd_peak"] += usd(row)
    total_usd = sum(r["usd_peak"] for r in by_role.values())
    inputs = sum(r["cache_hit"] + r["cache_miss"] for r in by_role.values())
    reviews = [json.loads(f.read_text()) for f in sorted((egm / "reviews").glob("*.json"))] if (egm / "reviews").exists() else []
    alarms = [x for r in reviews for x in r.get("alarms", []) if x.get("source") != "egm"]
    feature_file = egm / "features" / "jot-tags.json"
    feature = json.loads(feature_file.read_text()) if feature_file.exists() else {}
    prices_known = all(row.get("model") in prices for row in usage)

    summary = {
        "preset": a.preset,
        "exit": p.returncode,
        "phase_reached": feature.get("gate", {}).get("phase"),
        "verdicts": [r.get("verdict") for r in reviews],
        "usd_peak": round(total_usd, 4),
        "requests": sum(r["requests"] for r in by_role.values()),
        "cache_hit_share": round(sum(r["cache_hit"] for r in by_role.values()) / inputs, 3) if inputs else None,
        "by_role": {k: {**v, "usd_peak": round(v["usd_peak"], 4)} for k, v in by_role.items()},
        "alarms": {
            "raised_by_models": len(alarms),
            "dropped_by_grounding": sum(x.get("dropped_by") == "grounding" for x in alarms),
            "dropped_by_verifier": sum(x.get("dropped_by") == "verifier" for x in alarms),
            "confirmed": sum(x.get("status") == "confirmed" for x in alarms),
        },
        "log": str(work / "demo.log"),
    }
    stray_accepted = "not in the Implementation map" in log and "built:" in log and "not accepted" not in log
    provider_refused = "insufficient balance" in log.lower() or "no credit left" in log or "api key was rejected" in log
    failures = {
        "the provider refused the requests (insufficient balance or a rejected key; top up at https://platform.deepseek.com/top_up)": provider_refused,
        "egm-dsh crashed (panic)": "panicked at" in log,
        "a goldfish review was incomplete (unparseable answers)": "incomplete" in summary["verdicts"],
        "out-of-map change accepted": stray_accepted,
        f"cost over budget (${a.max_usd})": total_usd > a.max_usd,
        "usage was not recorded": not usage and not provider_refused,
        "a model has no price in config": not prices_known,
    }
    summary["failures"] = [k for k, v in failures.items() if v]
    a.out.write_text(json.dumps(summary, indent=1) + "\n")
    print("SUMMARY " + json.dumps(summary))
    if summary["failures"]:
        print("FAIL: " + "; ".join(summary["failures"]))
        sys.exit(1)
    print(f"OK: {a.agent} run reached `{summary['phase_reached']}` for ${summary['usd_peak']} (peak rates); details in {a.out}")
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
