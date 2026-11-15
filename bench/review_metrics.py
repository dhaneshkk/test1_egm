"""Review metrics for a GitHub repository, defined as in Chen & Stratton (2026), so a
team's numbers can be read against the paper's: record a baseline before adopting
egm-dsh's evidence gate, then compare.

For merged pull requests in a window (bots' reviews and comments are left out):
  time to merge        opened → merged (days)
  waiting time         opened → first review activity by someone other than the author
  active review time   first review activity → merged
  comments per PR      human review comments and non-empty reviews
  blocked share        PRs with at least one "changes requested" review
  newcomer reviews     share of reviews by someone with no earlier merged PR touching
                       the same top-level directory, in the fetched history (an
                       approximation: older history is not seen)
  changes per issue    commits per closed issue, for PRs that close issues
  post-merge defects   a revert of the PR, or a later bug-fix PR touching one of its
                       files, within 14 days of the merge
  gate MCC             egm-dsh's gate verdict (the `<!-- egm-gate: pass|fail -->` marker
                       its evidence bundle leaves in the PR body) against "defect-free
                       after 14 days"; flagged as noise below 29 PRs per class

Medians come with 95% bootstrap intervals (2000 resamples, fixed seed). With
--split DATE, PRs merged before and after DATE are compared (baseline vs after).

  python3 bench/review_metrics.py --repo owner/name --since 2026-06-01 [--split 2026-09-01] [--out m.json]
  python3 bench/review_metrics.py --from-json prs.json      # offline, from a saved fetch
  python3 bench/review_metrics.py --selftest                # checks the arithmetic on built-in fixtures
"""
import argparse
import json
import random
import subprocess
import sys
from datetime import datetime, timedelta
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from compare import mcc  # noqa: E402

N_CAL_FORMAL_BOUND = 29
DEFECT_DAYS = 14

QUERY = """
query($owner: String!, $name: String!, $cursor: String) {
  repository(owner: $owner, name: $name) {
    pullRequests(states: MERGED, first: 50, after: $cursor, orderBy: {field: CREATED_AT, direction: DESC}) {
      pageInfo { hasNextPage endCursor }
      nodes {
        number title body createdAt mergedAt
        author { login __typename }
        commits { totalCount }
        files(first: 100) { nodes { path } }
        labels(first: 20) { nodes { name } }
        closingIssuesReferences(first: 10) { nodes { number } }
        reviews(first: 100) { nodes { author { login __typename } state submittedAt body
          comments(first: 100) { nodes { author { login __typename } createdAt } } } }
      }
    }
  }
}
"""


def ts(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00")) if s else None


def days(a, b):
    return (b - a).total_seconds() / 86400


def is_bot(actor):
    if not actor:
        return True
    return actor.get("__typename") == "Bot" or actor.get("login", "").endswith("[bot]")


def fetch(repo, since, limit):
    owner, name = repo.split("/", 1)
    cursor, out = None, []
    since_dt = ts(since + "T00:00:00Z") if since else None
    while True:
        args = ["gh", "api", "graphql", "-f", f"query={QUERY}", "-F", f"owner={owner}", "-F", f"name={name}"]
        if cursor:
            args += ["-F", f"cursor={cursor}"]
        p = subprocess.run(args, capture_output=True, text=True)
        if p.returncode != 0:
            sys.exit(f"gh api failed: {p.stderr.strip()[:400]}")
        data = json.loads(p.stdout)["data"]["repository"]["pullRequests"]
        for n in data["nodes"]:
            if since_dt and ts(n["createdAt"]) < since_dt:
                return out
            out.append(n)
            if limit and len(out) >= limit:
                return out
        if not data["pageInfo"]["hasNextPage"]:
            return out
        cursor = data["pageInfo"]["endCursor"]


def gate_verdict(body):
    b = body or ""
    if "<!-- egm-gate: pass -->" in b:
        return "pass"
    if "<!-- egm-gate: fail -->" in b:
        return "fail"
    return None


def per_pr(prs):
    """One row per merged PR with every metric the paper uses."""
    prs = sorted(prs, key=lambda n: ts(n["mergedAt"]))
    rows = []
    seen_dirs: dict[str, set] = {}   # login -> top-level dirs of their earlier merged PRs
    for i, n in enumerate(prs):
        author = (n.get("author") or {}).get("login")
        opened, merged = ts(n["createdAt"]), ts(n["mergedAt"])
        files = [f["path"] for f in (n.get("files") or {}).get("nodes", [])]
        dirs = {f.split("/", 1)[0] if "/" in f else "." for f in files}
        activity, comments, blocked, reviewers = [], 0, False, []
        for r in (n.get("reviews") or {}).get("nodes", []):
            who = r.get("author")
            if is_bot(who) or who.get("login") == author:
                continue
            if r.get("submittedAt"):
                activity.append(ts(r["submittedAt"]))
            if (r.get("body") or "").strip():
                comments += 1
            if r.get("state") == "CHANGES_REQUESTED":
                blocked = True
            reviewers.append(who["login"])
            for c in (r.get("comments") or {}).get("nodes", []):
                if not is_bot(c.get("author")) and c["author"].get("login") != author:
                    comments += 1
                    activity.append(ts(c["createdAt"]))
        first = min(activity) if activity else None
        newcomers = [rv for rv in set(reviewers) if not (seen_dirs.get(rv, set()) & dirs)]
        # defects: a revert of this PR, or a later bug-fix PR touching its files, within 14 days
        defect = False
        for later in prs[i + 1:]:
            lm = ts(later["mergedAt"])
            if lm > merged + timedelta(days=DEFECT_DAYS):
                break
            title = (later.get("title") or "").lower()
            labels = {l["name"].lower() for l in (later.get("labels") or {}).get("nodes", [])}
            lfiles = {f["path"] for f in (later.get("files") or {}).get("nodes", [])}
            revert = title.startswith("revert") and f"#{n['number']}" in (later.get("title", "") + (later.get("body") or ""))
            bugfix = (labels & {"bug", "regression", "hotfix"} or title.startswith(("fix", "bugfix", "hotfix"))) and lfiles & set(files)
            if revert or bugfix:
                defect = True
                break
        rows.append({
            "number": n["number"], "merged": n["mergedAt"],
            "time_to_merge": days(opened, merged),
            "waiting": days(opened, first) if first else None,
            "active_review": days(first, merged) if first else None,
            "comments": comments, "blocked": blocked,
            "reviews": len(set(reviewers)), "newcomer_reviews": len(newcomers),
            "commits": (n.get("commits") or {}).get("totalCount", 0),
            "issues": [x["number"] for x in (n.get("closingIssuesReferences") or {}).get("nodes", [])],
            "defect": defect, "gate": gate_verdict(n.get("body")),
        })
        if author:
            seen_dirs.setdefault(author, set()).update(dirs)
    return rows


def median(xs):
    xs = sorted(xs)
    if not xs:
        return None
    m = len(xs) // 2
    return xs[m] if len(xs) % 2 else (xs[m - 1] + xs[m]) / 2


def boot(xs, stat=median, n=2000, seed=0):
    xs = [x for x in xs if x is not None]
    if len(xs) < 2:
        return None
    rng = random.Random(seed)
    vals = sorted(stat([xs[rng.randrange(len(xs))] for _ in xs]) for _ in range(n))
    return [vals[int(0.025 * n)], vals[int(0.975 * n) - 1]]


def summarize(rows):
    if not rows:
        return {"prs": 0}
    t = lambda k: [r[k] for r in rows if r[k] is not None]
    issues = {}
    for r in rows:
        for i in r["issues"]:
            issues.setdefault(i, 0)
            issues[i] += r["commits"]
    gated = [r for r in rows if r["gate"]]
    m, conf = (mcc([r["gate"] == "pass" for r in gated], [not r["defect"] for r in gated]) if gated else (None, {}))
    classes = {"pass": sum(r["gate"] == "pass" for r in gated), "fail": sum(r["gate"] == "fail" for r in gated)}
    out = {"prs": len(rows)}
    for k in ("time_to_merge", "waiting", "active_review", "comments"):
        xs = t(k)
        out[k] = {"median": median(xs), "ci95": boot(xs), "n": len(xs)}
    rate = lambda xs: sum(xs) / len(xs) if xs else None
    out["blocked share"] = {"value": rate([r["blocked"] for r in rows]), "ci95": boot([float(r["blocked"]) for r in rows], stat=lambda v: sum(v) / len(v))}
    rv = sum(r["reviews"] for r in rows)
    out["newcomer review share"] = {"value": (sum(r["newcomer_reviews"] for r in rows) / rv) if rv else None, "reviews": rv}
    out["commits per closed issue"] = {"value": (sum(issues.values()) / len(issues)) if issues else None, "issues": len(issues)}
    out["post-merge defect rate"] = {"value": rate([r["defect"] for r in rows]), "window_days": DEFECT_DAYS}
    out["gate MCC (pass vs defect-free)"] = {"mcc": m, "confusion": conf, "prs with a gate verdict": len(gated), "per class": classes,
                                            "noise": min(classes.values() or [0]) < N_CAL_FORMAL_BOUND}
    return out


def fmt(v, unit=""):
    return "n/a" if v is None else (f"{v:.2f}{unit}" if isinstance(v, float) else f"{v}{unit}")


def report(name, s):
    print(f"\n{name}: {s['prs']} merged PRs")
    if not s["prs"]:
        return
    for k, label in (("time_to_merge", "time to merge (days)"), ("waiting", "waiting for review (days)"),
                     ("active_review", "active review (days)"), ("comments", "comments per PR")):
        v = s[k]
        ci = v["ci95"]
        print(f"  {label:28} median {fmt(v['median'])}  95% CI {('[' + fmt(ci[0]) + ', ' + fmt(ci[1]) + ']') if ci else 'n/a'}  (n={v['n']})")
    print(f"  {'blocked share':28} {fmt(s['blocked share']['value'])}")
    print(f"  {'newcomer review share':28} {fmt(s['newcomer review share']['value'])}  ({s['newcomer review share']['reviews']} reviews)")
    print(f"  {'commits per closed issue':28} {fmt(s['commits per closed issue']['value'])}  ({s['commits per closed issue']['issues']} issues)")
    print(f"  {'post-merge defect rate':28} {fmt(s['post-merge defect rate']['value'])}  (within {DEFECT_DAYS} days)")
    g = s["gate MCC (pass vs defect-free)"]
    print(f"  {'gate MCC':28} {fmt(g['mcc'])}  ({g['prs with a gate verdict']} PRs with an egm-gate marker; per class {g['per class']})"
          + ("  noise: fewer than 29 per class" if g["noise"] else ""))


def selftest():
    """Fixtures with known answers; exits 1 on any mismatch."""
    def pr(n, created, merged, author="dev", reviews=(), files=("src/a.py",), title="feature", body="", commits=1, issues=(), labels=()):
        return {"number": n, "title": title, "body": body, "createdAt": created, "mergedAt": merged, "author": {"login": author, "__typename": "User"},
                "commits": {"totalCount": commits}, "files": {"nodes": [{"path": f} for f in files]}, "labels": {"nodes": [{"name": l} for l in labels]},
                "closingIssuesReferences": {"nodes": [{"number": i} for i in issues]}, "reviews": {"nodes": list(reviews)}}

    def rev(who, at, state="APPROVED", body="", comments=(), bot=False):
        return {"author": {"login": who, "__typename": "Bot" if bot else "User"}, "state": state, "submittedAt": at, "body": body,
                "comments": {"nodes": [{"author": {"login": who, "__typename": "User"}, "createdAt": c} for c in comments]}}

    prs = [
        pr(1, "2026-01-01T00:00:00Z", "2026-01-05T00:00:00Z", reviews=[rev("rhea", "2026-01-03T00:00:00Z", "CHANGES_REQUESTED", "no", ["2026-01-02T00:00:00Z"]),
                                                                       rev("ci-bot", "2026-01-01T01:00:00Z", body="lgtm", bot=True)],
           body="<!-- egm-gate: pass -->", commits=3, issues=[10]),
        pr(2, "2026-01-02T00:00:00Z", "2026-01-03T00:00:00Z", author="rhea", reviews=[rev("kit", "2026-01-02T12:00:00Z")], files=["docs/x.md"],
           body="<!-- egm-gate: fail -->", commits=2, issues=[10]),
        pr(3, "2026-01-06T00:00:00Z", "2026-01-07T00:00:00Z", author="kit", title="Revert #1 (broke search)", files=["src/a.py"]),
    ]
    rows = {r["number"]: r for r in per_pr(prs)}
    s = summarize(list(rows.values()))
    checks = {
        "time to merge in days": rows[1]["time_to_merge"] == 4 and rows[2]["time_to_merge"] == 1,
        "waiting = opened to first human activity (a review comment counts; the bot does not)": rows[1]["waiting"] == 1,
        "active review = first activity to merge": rows[1]["active_review"] == 3,
        "comments: human review bodies and review comments, not the bot": rows[1]["comments"] == 2,
        "blocked: a changes-requested review": rows[1]["blocked"] and not rows[2]["blocked"],
        "a revert within 14 days marks a defect": rows[1]["defect"] and not rows[2]["defect"],
        "newcomer: rhea had no earlier merged PR in src/": rows[1]["newcomer_reviews"] == 1,
        "commits per closed issue: (3 + 2) / 1": s["commits per closed issue"]["value"] == 5,
        "gate MCC counts only PRs with a marker": s["gate MCC (pass vs defect-free)"]["prs with a gate verdict"] == 2,
        "no reviews means no waiting time": rows[3]["waiting"] is None,
    }
    for k, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + k)
    print("SUMMARY " + json.dumps({"passed": sum(checks.values()), "failed": sum(not v for v in checks.values())}))
    if not all(checks.values()):
        sys.exit(1)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--repo")
    ap.add_argument("--since", help="YYYY-MM-DD: PRs created on or after")
    ap.add_argument("--split", help="YYYY-MM-DD: compare PRs merged before and after (baseline vs after)")
    ap.add_argument("--limit", type=int, default=0, help="at most this many PRs (0 = all in the window)")
    ap.add_argument("--from-json", type=Path)
    ap.add_argument("--save-json", type=Path, help="save the raw fetch for offline reruns")
    ap.add_argument("--out", type=Path, default=Path("review_metrics.json"))
    ap.add_argument("--selftest", action="store_true")
    a = ap.parse_args()
    if a.selftest:
        return selftest()
    if a.from_json:
        prs = json.loads(a.from_json.read_text())
    elif a.repo:
        prs = fetch(a.repo, a.since, a.limit)
        if a.save_json:
            a.save_json.write_text(json.dumps(prs))
    else:
        sys.exit("give --repo owner/name, --from-json FILE or --selftest")
    rows = per_pr(prs)
    result = {"repo": a.repo, "since": a.since, "rows": rows}
    if a.split:
        cut = ts(a.split + "T00:00:00Z")
        before = [r for r in rows if ts(r["merged"]) < cut]
        after = [r for r in rows if ts(r["merged"]) >= cut]
        result["before"], result["after"] = summarize(before), summarize(after)
        report(f"before {a.split} (baseline)", result["before"])
        report(f"from {a.split}", result["after"])
    else:
        result["all"] = summarize(rows)
        report(a.repo or str(a.from_json), result["all"])
    a.out.write_text(json.dumps(result, indent=1, default=str))
    print(f"\ndetails: {a.out.resolve()}")
    print("SUMMARY " + json.dumps({k: result[k] for k in ("before", "after", "all") if k in result}, default=str)[:3000])


if __name__ == "__main__":
    main()
