"""End-to-end test of egm-team phase 5: GitHub, GitLab and SOC 2 readiness.

Real: egm-dsh (the fix lane on the real DeepSeek Harness binary, model faked), egm-audit,
egm-team with its worker, Vesdb behind a TLS proxy with a private CA, vesdb-verify, Postgres 16,
Chromium, git. Faked: GitHub's and GitLab's REST APIs (one in-process server records every pull
or merge request and every commit status), and their webhooks (sent by the test, signed).

The same checks run against a GitHub project (`gh`) and a GitLab project (`gl`).

  A. the worker pushes `egm-dsh/<feature>` and opens a pull request (merge request), whose
     description carries the evidence; `egm/evidence-gate` is green on the branch head and
     `egm/merge-approval` waits; every status set is recorded in Vesdb
  B. after an approving review and a merge approval by someone other than the author,
     `egm/merge-approval` turns green on the code host
  C. signed webhooks: a merge is recorded; a revert of the merge commit is recorded; a bad
     signature is 401 and recorded as a refusal; a defect can be reported on a merged change
  D. metrics: review times with intervals; gate outcomes from reverts and defects
  E. export: an approver downloads the feature's bundle and it verifies offline with
     vesdb-verify; one changed byte fails it; a reviewer may not export
  F. evidence pack: every control has its evidence section; a change merged on the code host
     without approval is listed as an exception; the restore drill is recorded; the pack's
     bundle verifies offline
  G. confidentiality: `egm-team redact` erases people's words from Vesdb with a redact-scope
     token, keeps the actions, and every bundle still verifies; no event holds source code
  H. transport: egm-team and egm-audit reach Vesdb over TLS with the pinned CA; a wrong CA is
     refused; `/health/ready` reports Vesdb down when the proxy stops
  I. security self-test: headers on pages and errors, cross-site posts refused, script in a
     comment shown as text, other projects' records refused by id, path tricks refused,
     session cookie flags

usage: python3 run_host_e2e.py EGM_BINARY EGM_AUDIT EGM_TEAM VESDB VESDB_VERIFY DSH_COMMAND...
"""
import hashlib
import hmac
import json
import os
import shutil
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
VERIFY = sys.argv.pop(5)   # run_web_e2e reads the remaining arguments
import run_web_e2e as web  # noqa: E402

fx = web.fx
web.USERS["gina"] = {"gitlab": 2003, "name": "Gina Reviewer"}
SECRET = {"gh": "gh-webhook-secret-0123456789", "gl": "gl-webhook-secret-0123456789"}
REDACT = "r" * 40


class FakeHost(web.FakeOAuth):
    """GitHub's and GitLab's REST APIs for pull/merge requests and commit statuses, plus the
    OAuth endpoints of FakeOAuth (one address serves both, as `api_url` does for GitHub)."""
    requests = {}   # (repo, number) -> {branch, base, title, body}
    statuses = []   # (host, repo, sha, context, state, description)

    def _body(self):
        n = int(self.headers.get("Content-Length", "0"))
        return json.loads(self.rfile.read(n) or b"null") if n else None

    def _api(self, method):
        u = urllib.parse.urlparse(self.path)
        q = dict(urllib.parse.parse_qsl(u.query))
        parts = u.path.split("/")
        if u.path.startswith("/repos/"):
            if self.headers.get("Authorization") != "Bearer gh-token":
                return self._json({"message": "Bad credentials"}, 401)
            repo, rest, host = f"{parts[2]}/{parts[3]}", parts[4:], "github"
        else:
            if self.headers.get("PRIVATE-TOKEN") != "gl-token":
                return self._json({"message": "401 Unauthorized"}, 401)
            repo, rest, host = urllib.parse.unquote(parts[4]), parts[5:], "gitlab"
        body = self._body() if method in ("POST", "PATCH", "PUT") else None
        mine = {k: v for k, v in FakeHost.requests.items() if k[0] == repo}
        if host == "github" and rest == ["pulls"] and method == "GET":
            branch = q.get("head", ":").split(":", 1)[1]
            return self._json([{"number": k[1], "html_url": f"https://github.example/{repo}/pull/{k[1]}"} for k, v in mine.items() if v["branch"] == branch])
        if host == "gitlab" and rest == ["merge_requests"] and method == "GET":
            return self._json([{"iid": k[1], "web_url": f"https://gitlab.example/{repo}/-/merge_requests/{k[1]}"} for k, v in mine.items() if v["branch"] == q.get("source_branch")])
        if rest in (["pulls"], ["merge_requests"]) and method == "POST":
            n = len(mine) + 1
            gh = host == "github"
            FakeHost.requests[(repo, n)] = {"branch": body["head" if gh else "source_branch"], "base": body["base" if gh else "target_branch"],
                                            "title": body["title"], "body": body["body" if gh else "description"]}
            return self._json({"number": n, "html_url": f"https://github.example/{repo}/pull/{n}"} if gh else
                              {"iid": n, "web_url": f"https://gitlab.example/{repo}/-/merge_requests/{n}"}, 201)
        if len(rest) == 2 and rest[0] in ("pulls", "merge_requests") and method in ("PATCH", "PUT"):
            FakeHost.requests[(repo, int(rest[1]))]["body"] = body["body" if host == "github" else "description"]
            return self._json({})
        if len(rest) == 2 and rest[0] == "statuses" and method == "POST":
            FakeHost.statuses.append((host, repo, rest[1], body["context" if host == "github" else "name"], body["state"], body.get("description", "")))
            return self._json({}, 201)
        return self._json({"message": "not found"}, 404)

    def do_GET(self):
        if self.path.startswith(("/repos/", "/api/v4/projects/")):
            return self._api("GET")
        return super().do_GET()

    def do_POST(self):
        if self.path.startswith(("/repos/", "/api/v4/projects/")):
            return self._api("POST")
        return super().do_POST()

    def do_PATCH(self):
        return self._api("PATCH")

    def do_PUT(self):
        return self._api("PUT")


class TLSProxy(BaseHTTPRequestHandler):
    """HTTPS in front of Vesdb, as a load balancer or reverse proxy terminates TLS for it."""
    upstream = ""

    def log_message(self, *a):
        pass

    def _fwd(self):
        n = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(n) if n else None
        hdrs = {k: v for k, v in self.headers.items() if k.lower() not in ("host", "content-length", "connection")}
        req = urllib.request.Request(TLSProxy.upstream + self.path, data=body, method=self.command, headers=hdrs)
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                status, data, rh = r.status, r.read(), r.headers
        except urllib.error.HTTPError as e:
            status, data, rh = e.code, e.read(), e.headers
        self.send_response(status)
        for k, v in rh.items():
            if k.lower() not in ("transfer-encoding", "connection", "content-length"):
                self.send_header(k, v)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(data)

    do_GET = do_POST = do_PUT = _fwd


def certs(d):
    def run(*a):
        subprocess.run(a, cwd=d, check=True, capture_output=True)
    run("openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", "ca.key", "-out", "ca.pem", "-days", "2", "-subj", "/CN=egm test CA",
        "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign,cRLSign")
    run("openssl", "req", "-newkey", "rsa:2048", "-nodes", "-keyout", "srv.key", "-out", "srv.csr", "-subj", "/CN=localhost")
    (d / "ext.cnf").write_text("subjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n")
    run("openssl", "x509", "-req", "-in", "srv.csr", "-CA", "ca.pem", "-CAkey", "ca.key", "-CAcreateserial", "-out", "srv.pem", "-days", "2", "-extfile", "ext.cnf")
    run("openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", "other.key", "-out", "other.pem", "-days", "2", "-subj", "/CN=another CA",
        "-addext", "basicConstraints=critical,CA:TRUE")


def start_tls(port, d):
    srv = ThreadingHTTPServer(("127.0.0.1", port), TLSProxy)
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(d / "srv.pem", d / "srv.key")
    srv.socket = ctx.wrap_socket(srv.socket, server_side=True)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv


def hook(base, host, project, event, payload, secret, delivery):
    body = json.dumps(payload).encode()
    if host == "github":
        h = {"X-GitHub-Event": event, "X-GitHub-Delivery": delivery,
             "X-Hub-Signature-256": "sha256=" + hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()}
    else:
        h = {"X-Gitlab-Event": event, "X-Gitlab-Event-UUID": delivery, "X-Gitlab-Token": secret}
    req = urllib.request.Request(f"{base}/hooks/{host}/{project}", data=body, method="POST", headers=dict(h, **{"Content-Type": "application/json"}))
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            return r.status
    except urllib.error.HTTPError as e:
        return e.code


def wait(cond, timeout=40):
    end = time.time() + timeout
    while time.time() < end:
        if cond():
            return True
        time.sleep(0.5)
    return False


def main():
    from playwright.sync_api import sync_playwright

    work = Path(tempfile.mkdtemp(prefix="egm-host-"))
    os.chmod(work, 0o755)
    procs, projs = {}, []
    out, checks = {}, {}
    try:
        # --- two checkouts with the fix lane built and gated by egm-dsh (events to a file)
        evs = {}
        for name in ("gh", "gl"):
            a = fx.Project(f"host{name}", [{"match": fx.GOLD, "keep": True, "text": json.dumps(fx.CLEAN_ALL)}] + fx.builder(fx.FIXED)
                           + [{"match": fx.DIFF, "text": fx.MINOR}], with_test=True)
            projs.append(a)
            evs[name] = work / f"{name}.jsonl"
            out[f"fix-{name}"] = a.egm("--events", str(evs[name]), "fix", "gap-ids", fx.PROBLEM, "--files", "jot/cli.py", "--check", fx.CHECK)
            out[f"gate-{name}"] = a.egm("--events", str(evs[name]), "gate", "gap-ids", "--no-mutation")
            subprocess.run(["git", "init", "-q", "--bare", str(work / f"remote-{name}.git")], check=True)
        co = {"gh": projs[0].proj, "gl": projs[1].proj}

        # --- Vesdb behind TLS
        certs(work)
        key = work / "log.key"
        subprocess.run([web.VESDB, "keygen", "--name", "egm-host-test.local/log", "--out", str(key)], check=True, capture_output=True)
        vport, tport = fx.free_port(), fx.free_port()
        procs["vesdb"] = subprocess.Popen([web.VESDB, "run", "--data-directory-temporary", "--api-token", web.TOKEN, "--token", f"{REDACT}=redact",
                                           "--signing-key-file", str(key), "--listen", f"127.0.0.1:{vport}"],
                                          stdout=open(work / "vesdb.log", "w"), stderr=subprocess.STDOUT)
        assert web.wait_http(f"http://127.0.0.1:{vport}/health/ready"), (work / "vesdb.log").read_text()
        TLSProxy.upstream = f"http://127.0.0.1:{vport}"
        tls = start_tls(tport, work)
        vurl = f"https://localhost:{tport}"
        aenv = dict(os.environ, VESDB_URL=vurl, VESDB_TOKEN=web.TOKEN, VESDB_CA=str(work / "ca.pem"))
        for name in ("gh", "gl"):
            out[f"push-{name}"] = subprocess.run([web.EGM_AUDIT, "push", str(evs[name]), "--project", name, "--job", f"build-{name}", "--actor", "github:1001"],
                                                 env=aenv, capture_output=True, text=True)
        bad = subprocess.run([web.EGM_AUDIT, "replay-vesdb", "--project", "gh"], env=dict(aenv, VESDB_CA=str(work / "other.pem")), capture_output=True, text=True, timeout=120)
        checks["H: egm-audit writes to Vesdb over TLS with the pinned CA"] = all(out[f"push-{n}"].returncode == 0 for n in ("gh", "gl"))
        checks["H: with another CA, egm-audit refuses Vesdb's certificate"] = bad.returncode != 0

        # --- Postgres, fake hosts, egm-team
        pg = work / "pg"
        pg.mkdir()
        if os.geteuid() == 0:
            shutil.chown(pg, "postgres")
        else:
            subprocess.run(["sudo", "chown", "postgres", str(pg)], check=True)
        pgbin = Path(os.environ.get("PG_BIN", "/usr/lib/postgresql/16/bin"))
        pgport = fx.free_port()
        web.as_postgres(f"{pgbin}/initdb -A trust -U postgres -D {pg}/data")
        web.as_postgres(f"{pgbin}/pg_ctl -D {pg}/data -l {pg}/log -w -o \"-k {pg} -p {pgport} -c listen_addresses=''\" start")
        oport = fx.free_port()
        osrv = ThreadingHTTPServer(("127.0.0.1", oport), FakeHost)
        threading.Thread(target=osrv.serve_forever, daemon=True).start()
        fake = f"http://127.0.0.1:{oport}"
        wport = fx.free_port()
        base = f"http://localhost:{wport}"

        def config(ca):
            return f'''listen = "127.0.0.1:{wport}"
public_url = "{base}"
database_url = "host={pg} port={pgport} user=postgres dbname=postgres"
vesdb_url = "{vurl}"
vesdb_ca = "{ca}"
admins = ["github:1000"]
refresh_ms = 300

[[project]]
name = "gh"
checkout = "{co['gh']}"
host = "github"
repo = "acme/jot"
push_url = "{work / 'remote-gh.git'}"
retention_days = 30

[[project]]
name = "gl"
checkout = "{co['gl']}"
host = "gitlab"
repo = "acme/jotlab"
push_url = "{work / 'remote-gl.git'}"

[github]
client_id = "gh-client"
base_url = "{fake}"
api_url = "{fake}"

[gitlab]
client_id = "gl-client"
base_url = "{fake}"

[worker]
egm_dsh = "{EGM_BIN}"
'''
        cfg = work / "egm-team.toml"
        cfg.write_text(config(work / "ca.pem"))
        badcfg = work / "bad.toml"
        badcfg.write_text(config(work / "other.pem"))
        tenv = dict(os.environ, EGM_VESDB_TOKEN=web.TOKEN, EGM_GITHUB_CLIENT_SECRET="s3cret", EGM_GITLAB_CLIENT_SECRET="s3cret",
                    EGM_HOST_TOKEN_GH="gh-token", EGM_HOST_TOKEN_GL="gl-token", EGM_WEBHOOK_SECRET_GH=SECRET["gh"], EGM_WEBHOOK_SECRET_GL=SECRET["gl"])
        team = lambda *args, c=cfg, env=tenv: subprocess.run([web.EGM_TEAM, "--config", str(c), *args], env=env, capture_output=True, text=True, timeout=180)
        grants = [("gh", "github:1001", "developer"), ("gh", "gitlab:2002", "reviewer"), ("gh", "github:1004", "approver"),
                  ("gl", "github:1001", "developer"), ("gl", "gitlab:2003", "reviewer"), ("gl", "github:1004", "approver")]
        for p, u, r in grants:
            team("grant", "--project", p, "--user", u, "--roles", r, "--by", "github:1000")
        out["bad-ca"] = team("rebuild", c=badcfg)
        checks["H: egm-team with another CA cannot read Vesdb"] = out["bad-ca"].returncode != 0
        procs["team"] = subprocess.Popen([web.EGM_TEAM, "--config", str(cfg), "serve"], env=tenv, stdout=open(work / "team.log", "w"), stderr=subprocess.STDOUT)
        assert web.wait_http(f"{base}/health"), (work / "team.log").read_text()
        vev = lambda subject: web.vesdb_events(vport, subject)

        # A. pull/merge requests and statuses
        heads = {}
        for name, host, repo in (("gh", "github", "acme/jot"), ("gl", "gitlab", "acme/jotlab")):
            ok = wait(lambda: any(s[1] == repo and s[3] == "egm/merge-approval" for s in FakeHost.statuses), 60)
            local = subprocess.run(["git", "-C", str(co[name]), "rev-parse", "refs/heads/egm-dsh/gap-ids"], capture_output=True, text=True).stdout.strip()
            remote = subprocess.run(["git", "-C", str(work / f"remote-{name}.git"), "rev-parse", "refs/heads/egm-dsh/gap-ids"], capture_output=True, text=True).stdout.strip()
            heads[name] = local
            req = [v for k, v in FakeHost.requests.items() if k[0] == repo]
            st = {s[3]: s[4] for s in FakeHost.statuses if s[1] == repo and s[2] == local}
            checks[f"A ({host}): the worker pushed egm-dsh/gap-ids to the remote"] = ok and local != "" and local == remote
            checks[f"A ({host}): one request opened for the branch, carrying the evidence"] = len(req) == 1 and req[0]["branch"] == "egm-dsh/gap-ids" \
                and "Evidence gate: passed" in req[0]["body"] and "vesdb-verify bundle" in req[0]["body"]
            checks[f"A ({host}): evidence-gate is green and merge-approval waits on the head"] = st.get("egm/evidence-gate") == "success" and st.get("egm/merge-approval") == "pending"
            recorded = [e for e in vev(f"/egm/{name}/gap-ids") if e["type"] in ("io.egm.host.status-set", "io.egm.host.pr-opened")]
            checks[f"A ({host}): every status set and the request are recorded in Vesdb"] = sum(e["type"] == "io.egm.host.pr-opened" for e in recorded) == 1 \
                and sum(e["type"] == "io.egm.host.status-set" for e in recorded) == sum(1 for s in FakeHost.statuses if s[1] == repo)
        out["statuses"] = FakeHost.statuses[:]

        with sync_playwright() as pw:
            browser = pw.chromium.launch()

            def session(user):
                web.FakeOAuth.who = user
                ctx = browser.new_context()
                return ctx, ctx.new_page()

            def sign_in(page, provider):
                page.goto(f"{base}/sign-in")
                page.click(f"#signin-{provider}")
                page.wait_for_url(f"{base}/", timeout=15000)

            def post(page, path, form, extra=None):
                return page.request.post(f"{base}{path}", form=form, max_redirects=0, headers=extra or {}).status

            people = {}
            for who, prov in (("alice", "github"), ("rex", "gitlab"), ("gina", "gitlab"), ("ada", "github"), ("otto", "github"), ("adam", "github")):
                people[who] = session(who)
                sign_in(people[who][1], prov)

            # C (gl first): merged on GitLab before anyone approved: an exception in the pack
            st = hook(base, "gitlab", "gl", "Merge Request Hook", {"object_attributes": {"action": "merge", "iid": 1, "source_branch": "egm-dsh/gap-ids",
                      "target_branch": "main", "merge_commit_sha": "e" * 40}, "user": {"id": 2003, "username": "gina"}}, SECRET["gl"], "gl-1")
            checks["C (gitlab): a signed merge request hook records the merge"] = st == 200 and wait(
                lambda: any(e["type"] == "io.egm.merge.completed" for e in vev("/egm/gl/gap-ids")), 10)

            # B. approval on gh turns merge-approval green
            rex = people["rex"][1]
            out["review"] = post(rex, "/p/gh/f/gap-ids/review", {"verdict": "approve", "note": "Checked the fix and its test."})
            out["approve-rex"] = post(rex, "/p/gh/f/gap-ids/approve", {})
            if out["approve-rex"] == 403:   # high risk: the approver approves
                out["approve-ada"] = post(people["ada"][1], "/p/gh/f/gap-ids/approve", {})
            green = wait(lambda: any(s[1] == "acme/jot" and s[2] == heads["gh"] and s[3] == "egm/merge-approval" and s[4] == "success" for s in FakeHost.statuses), 30)
            checks["B (github): after an approval by someone else, merge-approval turns green on the code host"] = out["review"] == 303 and green
            out["alice-approve"] = post(people["alice"][1], "/p/gh/f/gap-ids/approve", {})
            checks["B: the author's own approval is still refused"] = out["alice-approve"] == 403
            gina = people["gina"][1]
            post(gina, "/p/gl/f/gap-ids/review", {"verdict": "approve", "note": "Fine."})
            if post(gina, "/p/gl/f/gap-ids/approve", {}) == 403:
                post(people["ada"][1], "/p/gl/f/gap-ids/approve", {})
            checks["B (gitlab): merge-approval turns green on GitLab too"] = wait(
                lambda: any(s[1] == "acme/jotlab" and s[3] == "egm/merge-approval" and s[4] == "success" for s in FakeHost.statuses), 30)

            # C. webhooks on gh: merge, revert, bad signature
            merge = {"action": "closed", "pull_request": {"merged": True, "number": 1, "merge_commit_sha": "a" * 40, "head": {"ref": "egm-dsh/gap-ids"},
                     "base": {"ref": "main"}, "merged_by": {"id": 1004, "login": "ada"}}}
            st_bad = hook(base, "github", "gh", "pull_request", merge, "not-the-secret-at-all-000", "gh-0")
            st_ok = hook(base, "github", "gh", "pull_request", merge, SECRET["gh"], "gh-1")
            st_again = hook(base, "github", "gh", "pull_request", merge, SECRET["gh"], "gh-1")
            time.sleep(1)
            ghe = vev("/egm/gh/gap-ids")
            checks["C (github): a signed pull_request hook records the merge, once for a redelivery"] = st_ok == 200 and st_again == 200 \
                and sum(e["type"] == "io.egm.merge.completed" for e in ghe) == 1
            checks["C: a bad signature is 401 and recorded as a refusal"] = st_bad == 401 and any(
                e["type"] == "io.egm.policy.refused" and e["data"]["actor"] == "github-webhook" for e in vev("/egm/gh"))
            revert = {"ref": "refs/heads/main", "sender": {"id": 1004}, "commits": [{"id": "b" * 40, "message": f"Revert \"egm-dsh: gap-ids\"\n\nThis reverts commit {'a' * 40}."}]}
            hook(base, "github", "gh", "push", revert, SECRET["gh"], "gh-2")
            checks["C (github): a push that reverts the merge commit is recorded"] = wait(lambda: any(e["type"] == "io.egm.change.reverted" for e in vev("/egm/gh/gap-ids")), 10)
            page = people["alice"][1]
            page.goto(f"{base}/p/gl/f/gap-ids")
            page.fill("#defect-form textarea", "Ids still collide after a restart.")
            page.click("#defect-form button")
            page.wait_for_load_state()
            checks["C (gitlab): a defect reported on a merged change is shown and recorded"] = "Ids still collide" in page.inner_text("#defects") \
                and any(e["type"] == "io.egm.defect.reported" for e in vev("/egm/gl/gap-ids"))
            page.goto(f"{base}/p/gh/f/gap-ids")
            checks["C: the feature page shows the request, the statuses, the merge and the revert"] = page.locator("#host-request").count() == 1 \
                and page.get_attribute('#host-statuses tr[data-context="egm/merge-approval"] td[data-state]', "data-state") == "success" \
                and page.locator("#merged").count() == 1 and page.locator("#reverted").count() == 1

            # D. metrics
            page.goto(f"{base}/p/gh/metrics")
            rt = page.inner_text("#review-times")
            checks["D: review times from the passing gate to review, approval and merge"] = rt.count("n = 1") == 3 or ("no data" not in rt)
            cells = page.locator("#gate-outcomes td").all_inner_texts()
            checks["D: the reverted change counts as a defect of a passed gate"] = cells[:3] == ["Gate passed", "0", "1"] and page.locator("#gate-warning").count() == 1

            # E. export
            ada = people["ada"][1]
            resp = ada.request.get(f"{base}/p/gh/f/gap-ids/export")
            bundle = work / "bundle.json"
            bundle.write_bytes(resp.body())
            v_ok = subprocess.run([VERIFY, "bundle", str(bundle), "--log-key", f"{key}.pub"], capture_output=True, text=True, timeout=60)
            text = bundle.read_text()
            i = text.find('"data"')
            tampered = work / "tampered.json"
            tampered.write_text(text[:i + 20] + ("X" if text[i + 20] != "X" else "Y") + text[i + 21:])
            v_bad = subprocess.run([VERIFY, "bundle", str(tampered), "--log-key", f"{key}.pub"], capture_output=True, text=True, timeout=60)
            out["verify"] = (v_ok.returncode, v_ok.stdout[-600:], v_ok.stderr[-600:])
            checks["E: the approver's export is a bundle that verifies offline with vesdb-verify"] = resp.status == 200 and "attachment" in resp.headers.get("content-disposition", "") \
                and v_ok.returncode == 0
            checks["E: one changed character in the bundle fails verification"] = v_bad.returncode != 0
            checks["E: a reviewer may not export (403), and each export is recorded"] = rex.request.get(f"{base}/p/gh/f/gap-ids/export").status == 403 \
                and any(e["type"] == "io.egm.export.created" for e in vev("/egm/gh/gap-ids"))

            # I. security self-test
            r = page.request.get(f"{base}/p/gh")
            r404 = page.request.get(f"{base}/p/gh/f/no-such-feature")
            need = {"content-security-policy", "x-frame-options", "x-content-type-options", "referrer-policy"}
            checks["I: security headers on pages and on error pages"] = need <= set(r.headers) and need <= set(r404.headers) \
                and "script-src 'self'" in r.headers["content-security-policy"] and r.headers["x-frame-options"] == "DENY"
            cross = post(page, "/p/gh/f/gap-ids/comment", {"target": "doc", "text": "cross-site"}, {"Origin": "https://evil.example", "Sec-Fetch-Site": "cross-site"})
            checks["I: a cross-site form post is refused"] = cross == 403
            dialogs = []
            page.on("dialog", lambda d: (dialogs.append(d.message), d.dismiss()))
            post(page, "/p/gh/f/gap-ids/comment", {"target": "doc", "text": "<script>alert(1)</script><img src=x onerror=alert(2)>"})
            page.goto(f"{base}/p/gh/f/gap-ids")
            page.wait_for_timeout(500)
            checks["I: script in a comment is shown as text and never runs"] = dialogs == [] and "<script>alert(1)</script>" in page.inner_text("#comment-list") \
                and page.locator("#comment-list img").count() == 0
            gina_gh = (gina.request.get(f"{base}/p/gh/f/gap-ids").status, gina.request.get(f"{base}/p/gh/f/gap-ids/export").status,
                       gina.request.get(f"{base}/p/gh/jobs/1").status)
            checks["I: a member of one project is refused another project's records by id"] = gina_gh == (403, 403, 403)
            tricks = [page.request.get(f"{base}{u}").status for u in ("/p/gh/f/..%2F..%2Fetc%2Fpasswd", "/p/gh/f/gap-ids/rev/..%2F..%2Fetc", "/p/gh/f/gap-ids/rev/zzzz",
                                                                      "/p/..%2Fgl/f/gap-ids", "/p/gh/jobs/1%20OR%201=1")]
            checks["I: path and id tricks are refused"] = all(s in (400, 403, 404) for s in tricks)
            out["tricks"] = tricks
            cookies = [c for c in people["alice"][0].cookies() if c["name"].startswith("__Host-")]
            checks["I: the session cookie is HttpOnly, SameSite=Lax and host-only"] = len(cookies) >= 1 and all(c["httpOnly"] and c["sameSite"] == "Lax" and c["path"] == "/" for c in cookies)

            # G. confidentiality: comment words, then redact
            post(page, "/p/gh/f/gap-ids/comment", {"target": "doc", "text": "Private remark about the rollout."})
            before = team("redact", "--project", "gh", "--before", "2100-01-01")
            applied = team("redact", "--project", "gh", "--before", "2100-01-01", "--apply", "--by", "github:1000", env=dict(tenv, EGM_VESDB_REDACT_TOKEN=REDACT))
            out["redact"] = (before.stdout, applied.stdout, applied.stderr[-800:])
            time.sleep(1)
            page.goto(f"{base}/p/gh/f/gap-ids")
            cl = page.inner_text("#comment-list")
            checks["G: redact lists, then erases people's words from Vesdb with a redact-scope token"] = before.returncode == 0 and "nothing erased" in before.stdout \
                and applied.returncode == 0 and "erased" in applied.stdout
            checks["G: after redaction the words are gone and the actions stay"] = "Private remark" not in cl and "(removed under the retention policy)" in cl \
                and "alice" in cl and any(e["type"] == "io.egm.comment.added" for e in vev("/egm/gh/gap-ids"))
            raw = json.dumps(vev("/egm/gh"))
            checks["G: Vesdb no longer returns the words"] = "Private remark" not in raw and "Checked the fix" not in raw
            resp2 = ada.request.get(f"{base}/p/gh/f/gap-ids/export")
            bundle2 = work / "bundle2.json"
            bundle2.write_bytes(resp2.body())
            v2 = subprocess.run([VERIFY, "bundle", str(bundle2), "--log-key", f"{key}.pub"], capture_output=True, text=True, timeout=60)
            checks["G: a bundle exported after redaction still verifies"] = v2.returncode == 0
            every = json.dumps(vev("/egm"))
            checks["G: no event holds source code"] = "def add" not in every and "import json" not in every

            # F. evidence pack (after a restore drill)
            out["rebuild"] = team("rebuild")
            pack_dir = work / "pack"
            out["evidence"] = team("evidence", "--from", "2000-01-01", "--to", "2100-01-01", "--out", str(pack_dir))
            pack = json.loads((pack_dir / "evidence.json").read_text()) if (pack_dir / "evidence.json").exists() else {"controls": [], "evidence": {}}
            sections = pack["evidence"]
            empty = [c["id"] for c in pack["controls"] if not sections.get(c["evidence"])]
            ids = {c["id"] for c in pack["controls"]}
            checks["F: the pack covers Security, Availability and Confidentiality, each control with its evidence"] = out["evidence"].returncode == 0 and not empty \
                and {"CC6.1", "CC6.3", "CC8.1", "CC4.1", "A1.1", "A1.3", "C1.1", "C1.2"} <= ids
            sod = sections.get("separation_of_duties", {})
            exc = [e["project"] for e in sod.get("exceptions", [])]
            checks["F: the change merged on GitLab without approval is the one exception"] = sod.get("merged_changes") == 2 and exc == ["gl"]
            checks["F: the restore drill and the redaction are in the pack"] = len(sections.get("availability", {}).get("restore_drills", [])) >= 1 \
                and len(sections.get("confidentiality", {}).get("redactions", [])) == 1
            checks["F: refusals and the webhook failure are counted for review"] = sections.get("monitoring", {}).get("refusals", 0) >= 3 \
                and sections.get("monitoring", {}).get("webhook_signature_failures") == 1
            vp = subprocess.run([VERIFY, "bundle", str(pack_dir / "bundle.json"), "--log-key", f"{key}.pub"], capture_output=True, text=True, timeout=60)
            checks["F: the pack's bundle of change-management events verifies offline"] = vp.returncode == 0
            checks["F: the pack's Markdown names every control"] = all(f"| {c} " in (pack_dir / "evidence.md").read_text() for c in ids)

            # H. readiness with Vesdb down
            ready = page.request.get(f"{base}/health/ready")
            tls.shutdown()
            tls.server_close()
            down = page.request.get(f"{base}/health/ready", timeout=20000)
            checks["H: /health/ready is 200 with Vesdb reachable and 503 when its proxy stops"] = ready.status == 200 and down.status == 503 and "vesdb" in down.text()
            for c, _ in people.values():
                c.close()
            browser.close()
    finally:
        for p in procs.values():
            if p.poll() is None:
                p.terminate()
                try:
                    p.wait(timeout=10)
                except Exception:
                    p.kill()
        for a in projs:
            a.close()
        try:
            web.as_postgres(f"/usr/lib/postgresql/16/bin/pg_ctl -D {work}/pg/data -m fast stop")
        except Exception:
            pass

    for name, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + name)
    print("SUMMARY " + json.dumps({"passed": sum(map(bool, checks.values())), "failed": sum(not v for v in checks.values())}))
    if not all(checks.values()) or len(checks) < 35:
        for k, v in out.items():
            print(f"--- {k}: {v}"[:1500])
        print("--- team.log:", (work / "team.log").read_text()[-4000:] if (work / "team.log").exists() else "")
        sys.exit(1)
    shutil.rmtree(work, ignore_errors=True)


EGM_BIN = fx.EGM

if __name__ == "__main__":
    main()
