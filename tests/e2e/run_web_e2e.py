"""Browser end-to-end test of egm-team's read-only team view (phase 2).

Everything is real except the model and the OAuth providers: egm-dsh with the real DeepSeek
Harness binary (model faked by fake_deepseek.py) builds a fix in a git checkout; egm-audit
pushes its events to a real Vesdb; egm-team (Topcoat) serves the views from a real Postgres
16; Chromium (Playwright) drives the pages. A small fake GitHub and GitLab answer the OAuth
code flow (GitLab's PKCE is checked).

  A. sign-in: pages need a session; GitHub sign-in works; GitLab sign-in works with PKCE; a
     forged state fails, is refused and is recorded; sign-out ends the session
  B. pages: projects, board, feature (doc, revisions, goldfish reviews, evidence, history), a
     revision, metrics, members, audit trail, each showing what egm-dsh and Vesdb hold
  C. policy: a signed-in outsider sees no projects and gets 403 on every project page, and each
     refusal lands in Vesdb as policy.refused; an unknown project is 404
  D. safety: doc text with a <script> renders as text; a path-traversal revision is 404
  E. rebuild: dropping the views and rebuilding them from Vesdb gives the same pages

usage: python3 run_web_e2e.py EGM_BINARY EGM_AUDIT EGM_TEAM VESDB DSH_COMMAND...
"""
import base64
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
EGM_AUDIT, EGM_TEAM, VESDB = sys.argv[2], sys.argv[3], sys.argv[4]
sys.argv = [sys.argv[0], sys.argv[1]] + sys.argv[5:]
import run_fix_e2e as fx  # noqa: E402

TOKEN = "t" * 40
USERS = {
    "adam": {"github": 1000, "name": "Adam Admin"},
    "alice": {"github": 1001, "name": "Alice Dev"},
    "rex": {"gitlab": 2002, "name": "Rex Reviewer"},
    "otto": {"github": 1003, "name": "Otto Outsider"},
}


class FakeOAuth(BaseHTTPRequestHandler):
    """GitHub's and GitLab's authorize, token and user endpoints, for one chosen user."""
    who = "alice"
    codes = {}   # code -> (user, pkce challenge)
    seen = []

    def log_message(self, *a):
        pass

    def _json(self, obj, status=200):
        b = json.dumps(obj).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_GET(self):
        u = urllib.parse.urlparse(self.path)
        q = dict(urllib.parse.parse_qsl(u.query))
        FakeOAuth.seen.append(u.path)
        if u.path in ("/login/oauth/authorize", "/oauth/authorize"):
            code = f"code-{FakeOAuth.who}-{len(FakeOAuth.codes)}"
            FakeOAuth.codes[code] = (FakeOAuth.who, q.get("code_challenge"))
            self.send_response(302)
            self.send_header("Location", f"{q['redirect_uri']}?{urllib.parse.urlencode({'code': code, 'state': q['state']})}")
            self.end_headers()
        elif u.path in ("/user", "/api/v4/user"):
            tok = self.headers.get("Authorization", "").removeprefix("Bearer ")
            name = tok.removeprefix("tok-")
            if name not in USERS:
                return self._json({"message": "Bad credentials"}, 401)
            info = USERS[name]
            if u.path == "/user":
                self._json({"id": info["github"], "login": name, "name": info["name"]})
            else:
                self._json({"id": info["gitlab"], "username": name, "name": info["name"]})
        else:
            self._json({"error": "not found"}, 404)

    def do_POST(self):
        u = urllib.parse.urlparse(self.path)
        n = int(self.headers.get("Content-Length", "0"))
        form = dict(urllib.parse.parse_qsl(self.rfile.read(n).decode()))
        FakeOAuth.seen.append(u.path)
        entry = FakeOAuth.codes.pop(form.get("code", ""), None)
        if entry is None or form.get("client_secret") != "s3cret":
            return self._json({"error": "bad_verification_code"})
        user, challenge = entry
        if u.path == "/oauth/token":   # GitLab: PKCE S256 must match
            v = form.get("code_verifier", "")
            got = base64.urlsafe_b64encode(hashlib.sha256(v.encode()).digest()).rstrip(b"=").decode()
            if not challenge or got != challenge or form.get("grant_type") != "authorization_code":
                return self._json({"error": "invalid_grant", "error_description": "PKCE verification failed"}, 400)
        self._json({"access_token": f"tok-{user}", "token_type": "bearer"})


def as_postgres(cmd):
    """Run a shell command as the postgres user (root here, sudo on a CI runner)."""
    if os.geteuid() == 0:
        return subprocess.run(["su", "postgres", "-c", cmd], check=True, capture_output=True)
    return subprocess.run(["sudo", "-u", "postgres", "sh", "-c", cmd], check=True, capture_output=True)


def wait_http(url, timeout=30):
    end = time.time() + timeout
    while time.time() < end:
        try:
            urllib.request.urlopen(url, timeout=2)
            return True
        except urllib.error.HTTPError:
            return True
        except Exception:
            time.sleep(0.2)
    return False


def vesdb_events(port, subject):
    req = urllib.request.Request(f"http://127.0.0.1:{port}/api/v2/data/read-events", method="POST",
                                 data=json.dumps({"subject": subject, "options": {"recursive": True}}).encode(),
                                 headers={"Authorization": f"Bearer {TOKEN}", "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=10) as r:
        return [json.loads(l)["data"] for l in r.read().decode().splitlines() if json.loads(l)["type"] == "event"]


def main():
    from playwright.sync_api import sync_playwright

    work = Path(tempfile.mkdtemp(prefix="egm-web-"))
    os.chmod(work, 0o755)   # Postgres runs as its own user and must reach its data folder
    procs = []
    a = None
    out = {}
    checks = {}
    try:
        # --- egm-dsh builds a fix; its events go to Vesdb
        a = fx.Project("web", [{"match": fx.GOLD, "keep": True, "text": json.dumps(fx.CLEAN_ALL)}] + fx.builder(fx.FIXED)
                       + [{"match": fx.DIFF, "text": fx.MINOR}], with_test=True)
        ev = work / "events.jsonl"
        out["fix"] = a.egm("--events", str(ev), "fix", "gap-ids", fx.PROBLEM, "--files", "jot/cli.py", "--check", fx.CHECK)
        out["gate"] = a.egm("--events", str(ev), "gate", "gap-ids", "--no-mutation")
        feat = a.feature("gap-ids")
        co = a.proj
        reviews = sorted((co / ".egm-dsh" / "reviews").glob("gap-ids-*.json"))

        key = work / "log.key"
        subprocess.run([VESDB, "keygen", "--name", "egm-web-test.local/log", "--out", str(key)], check=True, capture_output=True)
        vport = fx.free_port()
        procs.append(subprocess.Popen([VESDB, "run", "--data-directory-temporary", "--api-token", TOKEN, "--signing-key-file", str(key),
                                       "--listen", f"127.0.0.1:{vport}"], stdout=open(work / "vesdb.log", "w"), stderr=subprocess.STDOUT))
        assert wait_http(f"http://127.0.0.1:{vport}/health/ready"), (work / "vesdb.log").read_text()
        env = dict(os.environ, VESDB_URL=f"http://127.0.0.1:{vport}", VESDB_TOKEN=TOKEN)
        out["push"] = subprocess.run([EGM_AUDIT, "push", str(ev), "--project", "jot", "--job", "build-1"], env=env, capture_output=True, text=True)

        # --- Postgres 16
        pg = work / "pg"
        pg.mkdir()
        if os.geteuid() == 0:
            shutil.chown(pg, "postgres")
        else:
            subprocess.run(["sudo", "chown", "postgres", str(pg)], check=True)
        pgbin = Path(os.environ.get("PG_BIN", "/usr/lib/postgresql/16/bin"))
        pgport = fx.free_port()
        as_postgres(f"{pgbin}/initdb -A trust -U postgres -D {pg}/data")
        as_postgres(f"{pgbin}/pg_ctl -D {pg}/data -l {pg}/log -w -o \"-k {pg} -p {pgport} -c listen_addresses=''\" start")

        # --- fake GitHub and GitLab
        oport = fx.free_port()
        osrv = ThreadingHTTPServer(("127.0.0.1", oport), FakeOAuth)
        threading.Thread(target=osrv.serve_forever, daemon=True).start()
        fake = f"http://127.0.0.1:{oport}"

        # --- egm-team
        wport = fx.free_port()
        base = f"http://localhost:{wport}"
        cfg = work / "egm-team.toml"
        cfg.write_text(f'''listen = "127.0.0.1:{wport}"
public_url = "{base}"
database_url = "host={pg} port={pgport} user=postgres dbname=postgres"
vesdb_url = "http://127.0.0.1:{vport}"
admins = ["github:1000"]
refresh_ms = 300

[[project]]
name = "jot"
checkout = "{co}"
host = "github"

[github]
client_id = "gh-client"
base_url = "{fake}"
api_url = "{fake}"

[gitlab]
client_id = "gl-client"
base_url = "{fake}"
''')
        tenv = dict(os.environ, EGM_VESDB_TOKEN=TOKEN, EGM_GITHUB_CLIENT_SECRET="s3cret", EGM_GITLAB_CLIENT_SECRET="s3cret")
        team = lambda *args: subprocess.run([EGM_TEAM, "--config", str(cfg), *args], env=tenv, capture_output=True, text=True, timeout=120)
        out["grant-alice"] = team("grant", "--project", "jot", "--user", "github:1001", "--roles", "developer", "--by", "github:1000")
        out["grant-rex"] = team("grant", "--project", "jot", "--user", "gitlab:2002", "--roles", "reviewer", "--by", "github:1000")
        out["grant-bad"] = team("grant", "--project", "jot", "--user", "github:9", "--roles", "worker")
        procs.append(subprocess.Popen([EGM_TEAM, "--config", str(cfg), "serve"], env=tenv, stdout=open(work / "team.log", "w"), stderr=subprocess.STDOUT))
        assert wait_http(f"{base}/health"), (work / "team.log").read_text()
        time.sleep(1.0)

        with sync_playwright() as pw:
            browser = pw.chromium.launch()

            def session(user):
                FakeOAuth.who = user
                ctx = browser.new_context()
                page = ctx.new_page()
                return ctx, page

            def sign_in(page, provider):
                page.goto(f"{base}/sign-in")
                page.click(f"#signin-{provider}")
                try:
                    page.wait_for_url(f"{base}/", timeout=15000)
                except Exception:
                    print(f"--- sign-in with {provider} stopped at {page.url}: {page.content()[:1500]}")
                    raise
                return page

            # A. anonymous
            ctx, page = session("alice")
            page.goto(f"{base}/p/jot")
            checks["A: a project page without a session goes to sign-in"] = page.url == f"{base}/sign-in"
            sign_in(page, "github")
            checks["A: GitHub sign-in lands on the projects page as the right user"] = page.inner_text("#viewer") == "alice" and "jot" in page.inner_text("#projects")
            cookies = ctx.cookies()
            sess = [c for c in cookies if c["name"].startswith("__Host-")]
            checks["A: the session cookie is __Host-, HttpOnly and SameSite=Lax"] = bool(sess) and sess[0]["httpOnly"] and sess[0]["sameSite"] == "Lax"

            # B. pages
            page.click("text=jot")
            phase = feat["gate"]["phase"]
            col = page.inner_text(f'section[data-column="{phase}"]')
            checks["B: the board shows the feature in its phase column"] = "gap-ids" in col
            page.click("text=gap-ids")
            doc_text = (co / feat["doc"]).read_text() if feat.get("doc") and (co / feat["doc"]).exists() else None
            pdoc = page.inner_text("#doc")
            checks["B: the feature page shows the design doc from the checkout"] = doc_text is not None and doc_text.strip()[:60] in pdoc
            checks["B: the phase badge matches egm-dsh's gate"] = page.get_attribute(".phase", "data-phase") == phase
            n_reviews = page.locator("#reviews tr").count() - 1 if page.locator("#reviews").count() else 0
            checks["B: every goldfish review is listed"] = n_reviews == len(reviews) and len(reviews) >= 1
            checks["B: the evidence gate result is shown"] = "Gate passed" in page.inner_text("#evidence")
            hist = page.inner_text("#history")
            checks["B: the history lists the feature's audit events"] = all(k in hist for k in ("feature.created", "goldfish.verdict", "gate.verdict"))
            revs = page.locator("#revisions a")
            checks["B: the doc's git revisions are listed"] = revs.count() >= 1
            if revs.count():
                revs.first.click()
                checks["B: a revision page shows the doc at that commit"] = page.locator("#doc-at").count() == 1 and len(page.inner_text("#doc-at")) > 0
            page.goto(f"{base}/p/jot/metrics")
            mt = page.inner_text("#verdicts")
            checks["B: metrics count the gate verdicts and say why MCC is not shown yet"] = "Evidence gate" in mt and "labels" in page.inner_text("#mcc")
            page.goto(f"{base}/p/jot/members")
            mem = page.inner_text("#members")
            checks["B: members lists the granted roles"] = "developer" in mem and "reviewer" in mem
            page.goto(f"{base}/p/jot/audit")
            au = page.inner_text("#audit")
            checks["B: the audit trail shows feature and member events"] = "member.role-changed" in au and "build.completed" in au

            # D. safety
            evil = "<script>window.__pwned = 1</script> EVIL-MARKER"
            if doc_text is not None:
                (co / feat["doc"]).write_text(doc_text + "\n" + evil + "\n")
                page.goto(f"{base}/p/jot/f/gap-ids")
                checks["D: doc text with a <script> renders as text, not code"] = "EVIL-MARKER" in page.inner_text("#doc") and page.evaluate("window.__pwned") is None
                (co / feat["doc"]).write_text(doc_text)
            r = page.goto(f"{base}/p/jot/f/gap-ids/rev/..%2F..%2Fetc")
            checks["D: a path-traversal revision is 404"] = r.status == 404
            r = page.goto(f"{base}/p/nope")
            checks["C: an unknown project is 404, with a page that says so"] = r.status == 404 and page.locator("#not-found").count() == 1

            # A. sign-out
            page.goto(f"{base}/")
            page.click("text=Sign out")
            page.wait_for_url(f"{base}/sign-in")
            page.goto(f"{base}/p/jot")
            checks["A: after sign-out, pages need a session again"] = page.url == f"{base}/sign-in"
            ctx.close()

            # A. GitLab with PKCE
            ctx, page = session("rex")
            sign_in(page, "gitlab")
            checks["A: GitLab sign-in (PKCE checked by the provider) works"] = page.inner_text("#viewer") == "rex" and "/oauth/token" in FakeOAuth.seen
            r = page.goto(f"{base}/p/jot/f/gap-ids")
            checks["A: the reviewer can read the feature"] = r.status == 200
            ctx.close()

            # C. outsider
            ctx, page = session("otto")
            sign_in(page, "github")
            checks["C: an outsider sees no projects"] = page.locator("#no-projects").count() == 1
            statuses = {p: page.goto(f"{base}{p}").status for p in ["/p/jot", "/p/jot/f/gap-ids", "/p/jot/metrics", "/p/jot/members", "/p/jot/audit"]}
            checks["C: every project page is 403 for an outsider"] = set(statuses.values()) == {403}
            checks["C: the 403 page explains it and keeps the header"] = page.locator("#forbidden").count() == 1 and page.inner_text("#viewer") == "otto"
            ctx.close()

            # A. forged state
            ctx, page = session("alice")
            page.goto(f"{base}/sign-in")
            r = page.goto(f"{base}/auth/github/callback?code=x&state={'f' * 64}")
            checks["A: a callback with no matching sign-in cookie is refused"] = r.status == 400
            ctx.close()

            time.sleep(0.5)
            refused = [e for e in vesdb_events(vport, "/egm/jot") if e["type"] == "io.egm.policy.refused"]
            system = vesdb_events(vport, "/egm/_system")
            checks["C: each refusal is recorded in Vesdb as policy.refused, with the user"] = len(refused) >= 5 and all(e["data"]["actor"] == "github:1003" for e in refused)
            checks["A: sign-ins and the failed one are recorded in Vesdb"] = sum(e["type"] == "io.egm.auth.signed-in" for e in system) >= 3 \
                and any(e["type"] == "io.egm.auth.sign-in-failed" for e in system)

            # E. rebuild
            out["rebuild"] = team("rebuild")
            ctx, page = session("adam")
            sign_in(page, "github")
            page.set_viewport_size({"width": 390, "height": 844})
            page.goto(f"{base}/p/jot/f/gap-ids")
            checks["phone: the feature page fits a 390-pixel screen without sideways scrolling"] = page.evaluate("document.documentElement.scrollWidth <= window.innerWidth + 1")
            page.set_viewport_size({"width": 1280, "height": 800})
            page.goto(f"{base}/p/jot")
            checks["E: after a rebuild from Vesdb the admin sees the same board"] = "gap-ids" in page.inner_text(f'section[data-column="{phase}"]')
            page.goto(f"{base}/p/jot/members")
            checks["E: roles survive the rebuild (they live in Vesdb)"] = "developer" in page.inner_text("#members")
            ctx.close()
            browser.close()

        checks["setup: grant refuses the worker role for a person"] = out["grant-bad"].returncode != 0
        checks["setup: rebuild reports the events it replayed"] = out["rebuild"].returncode == 0 and "rebuilt the views from" in out["rebuild"].stdout
    finally:
        if a is not None:
            a.close()
        for p in procs:
            p.terminate()
            try:
                p.wait(timeout=10)
            except Exception:
                p.kill()
        try:
            as_postgres(f"/usr/lib/postgresql/16/bin/pg_ctl -D {work}/pg/data -m fast stop")
        except Exception:
            pass

    for name, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + name)
    print("SUMMARY " + json.dumps({"passed": sum(map(bool, checks.values())), "failed": sum(not v for v in checks.values())}))
    if not all(checks.values()) or len(checks) < 25:
        for k, v in out.items():
            print(f"--- {k}: {v}"[:1500])
        print("--- team.log:", (work / "team.log").read_text()[-3000:] if (work / "team.log").exists() else "")
        sys.exit(1)
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
