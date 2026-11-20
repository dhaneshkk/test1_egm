"""Browser end-to-end test of egm-team phase 4: running egm-dsh from the web.

Everything is real except the model: egm-team with its worker, the real egm-dsh binary on its
offline mock agent (the scripted jot-tags story, whose goldfish is a rule-based reader of the
doc), Vesdb, Postgres 16, Chromium. A small wrapper around egm-dsh records each job's
environment and makes `gate` jobs take three seconds, so the test can watch a log stream and
a busy project.

  A. a full feature, idea to evidence, in the browser: start it, answer the elephant in the
     chat, draft, goldfish (not ready), answer, redraft two sections, goldfish (ready), build,
     review, gate. egm-dsh still decides: a build before the goldfish passes fails in its job
  B. the job page streams its log while the job runs, and the log endpoint pages by `after`
  C. one job at a time per project, oldest first, while another project is not held up
  D. a project at its monthly token ceiling has its next job refused, and says why
  E. policy: a reviewer cannot run jobs or start features, an outsider cannot see jobs; bad
     input is 400; each refusal is in Vesdb
  F. cost: tokens per feature on the page equal the usage events in Vesdb
  G. a job's environment holds none of egm-team's secrets
  H. replayed state equals live state: egm-dsh's own status, egm-audit's replay of Vesdb and
     the app agree; after `rebuild` the pages are the same
  I. durability: the server is killed while a job runs; after a restart that job is finished
     as abandoned and the job queued behind it runs

usage: python3 run_jobs_e2e.py EGM_BINARY EGM_AUDIT EGM_TEAM VESDB DSH_COMMAND...
(DSH_COMMAND is accepted for symmetry with run_web_e2e.py and not used: the agent is the mock.)
"""
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from http.server import ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import run_web_e2e as web  # noqa: E402  (fake OAuth, Postgres and Vesdb helpers; parses the same arguments)

fx = web.fx
EGM = fx.EGM
SAMPLE = HERE.parent.parent / "sample" / "before"
ACCEPT = ["python3", "-m", "unittest", "discover", "-s", "tests", "-t", ".", "-q"]


def checkout(path):
    shutil.copytree(SAMPLE, path)
    (path / "README.md").write_text("# jot\n\nA tiny notebook.\n")
    (path / ".gitignore").write_text("notes.json\n__pycache__/\n*.pyc\n")
    for c in (["git", "init", "-q"], ["git", "config", "user.email", "egm@example.test"], ["git", "config", "user.name", "egm"],
              ["git", "add", "-A"], ["git", "commit", "-qm", "init"]):
        subprocess.run(c, cwd=path, check=True, capture_output=True)
    subprocess.run([EGM, "init", "--agent", "mock", "--acceptance", *ACCEPT], cwd=path, check=True, capture_output=True)


def main():
    from playwright.sync_api import sync_playwright

    work = Path(tempfile.mkdtemp(prefix="egm-jobs-"))
    os.chmod(work, 0o755)
    procs = {}
    out, checks = {}, {}
    try:
        co, tight, other = work / "jot", work / "tight", work / "other"
        for c in (co, tight, other):
            checkout(c)
        envdir = work / "envs"
        envdir.mkdir()
        wrapper = work / "egm-dsh-wrapper"
        wrapper.write_text(f"""#!/bin/sh
env > "{envdir}/$$.txt"
if [ "$3" = gate ]; then
  printf '{{"type":"line","speaker":"egm","text":"warming up"}}\\n' >> "$2"
  sleep 3
fi
exec "{EGM}" "$@"
""")
        wrapper.chmod(0o755)

        key = work / "log.key"
        subprocess.run([web.VESDB, "keygen", "--name", "egm-jobs-test.local/log", "--out", str(key)], check=True, capture_output=True)
        vport = fx.free_port()
        procs["vesdb"] = subprocess.Popen([web.VESDB, "run", "--data-directory-temporary", "--api-token", web.TOKEN, "--signing-key-file", str(key),
                                           "--listen", f"127.0.0.1:{vport}"], stdout=open(work / "vesdb.log", "w"), stderr=subprocess.STDOUT)
        assert web.wait_http(f"http://127.0.0.1:{vport}/health/ready"), (work / "vesdb.log").read_text()

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
        osrv = ThreadingHTTPServer(("127.0.0.1", oport), web.FakeOAuth)
        threading.Thread(target=osrv.serve_forever, daemon=True).start()
        fake = f"http://127.0.0.1:{oport}"

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

[[project]]
name = "tight"
checkout = "{tight}"
monthly_tokens = 1

[[project]]
name = "other"
checkout = "{other}"

[github]
client_id = "gh-client"
base_url = "{fake}"
api_url = "{fake}"

[gitlab]
client_id = "gl-client"
base_url = "{fake}"

[worker]
egm_dsh = "{wrapper}"
timeout_s = 120
max_parallel = 2
''')
        tenv = dict(os.environ, EGM_VESDB_TOKEN=web.TOKEN, EGM_GITHUB_CLIENT_SECRET="s3cret", EGM_GITLAB_CLIENT_SECRET="s3cret",
                    VESDB_TOKEN="should-not-leak")
        team = lambda *args: subprocess.run([web.EGM_TEAM, "--config", str(cfg), *args], env=tenv, capture_output=True, text=True, timeout=120)
        for proj in ("jot", "tight", "other"):
            team("grant", "--project", proj, "--user", "github:1001", "--roles", "developer", "--by", "github:1000")
        team("grant", "--project", "jot", "--user", "gitlab:2002", "--roles", "reviewer", "--by", "github:1000")

        def serve():
            procs["team"] = subprocess.Popen([web.EGM_TEAM, "--config", str(cfg), "serve"], env=tenv,
                                             stdout=open(work / "team.log", "a"), stderr=subprocess.STDOUT)
            assert web.wait_http(f"{base}/health"), (work / "team.log").read_text()
            time.sleep(1.0)

        serve()
        vev = lambda subject="/egm/jot": web.vesdb_events(vport, subject)

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

            def finished(page, timeout=90000):
                page.wait_for_selector('#job-state[data-state="done"]', timeout=timeout)
                return page.inner_text("#job-state")

            def run(page, slug, command, text=None, sections=()):
                page.goto(f"{base}/p/jot/f/{slug}")
                if command == "say":
                    page.fill("#say-form textarea", text)
                    page.click("#say-form button")
                elif command == "draft":
                    for s in sections:
                        page.check(f'#draft-form label:has-text("{s}") input')
                    page.click("#draft-form button")
                else:
                    page.click(f'#run-buttons button[data-command="{command}"]')
                page.wait_for_url(f"{base}/p/jot/jobs/*")
                return finished(page)

            def post(page, path, form):
                return page.request.post(f"{base}{path}", form=form, max_redirects=0).status

            # A. a full feature in the browser
            ctx, page = session("alice")
            sign_in(page, "github")
            page.goto(f"{base}/p/jot")
            checks["A: a developer sees the New feature form"] = page.locator("#new-feature").count() == 1
            page.click("#new-feature summary")
            page.fill('#new-feature input[name="slug"]', "jot-tags")
            page.fill('#new-feature textarea[name="idea"]', "Let people tag notes and list only the notes with a given tag.")
            page.click("#new-feature button")
            page.wait_for_url(f"{base}/p/jot/jobs/*")
            out["start"] = finished(page)
            checks["A: starting a feature queues the elephant, which asks questions"] = out["start"] == "done" and "Who needs this" in page.inner_text("#log")
            page.goto(f"{base}/p/jot/f/jot-tags")
            chat = page.inner_text("#chat-log")
            checks["A: the chat shows the idea and the elephant's questions"] = "tag notes" in chat and "Who needs this" in chat
            checks["A: the feature's author is the developer who started it"] = "alice" in page.inner_text("#author")
            out["say1"] = run(page, "jot-tags", "say", "Mostly me: I keep work and personal notes in one jot and want only the work ones.")
            out["say2"] = run(page, "jot-tags", "say", "Store tags on each note in notes.json as a list of strings; a note can have several.")
            page.goto(f"{base}/p/jot/f/jot-tags")
            checks["A: the chat keeps the whole conversation in order"] = page.locator("#chat-log .said").count() == 3 \
                and page.inner_text("#chat-log").index("Mostly me") < page.inner_text("#chat-log").index("Store tags")
            out["draft"] = run(page, "jot-tags", "draft")
            out["gold1"] = run(page, "jot-tags", "goldfish")
            page.goto(f"{base}/p/jot/f/jot-tags")
            checks["A: the first goldfish finds the gap: not ready, the feature stays designing"] = "not-ready" in page.inner_text("#reviews") \
                and page.get_attribute("span.phase[data-phase]", "data-phase") == "designing"
            out["build-early"] = run(page, "jot-tags", "build")
            checks["A: a build before the goldfish passes fails in its job, refused by egm-dsh"] = out["build-early"].startswith("failed") \
                and "refused" in page.inner_text("#log")
            out["say3"] = run(page, "jot-tags", "say", "Notes saved before this change have no tags field. Treat a missing field as an empty list and never rewrite old notes, so existing files need no migration.")
            out["redraft"] = run(page, "jot-tags", "draft", sections=("Technical plan", "Acceptance criteria"))
            checks["A: a redraft of two ticked sections redrafts only those"] = "2 sections" in page.inner_text("#log") and "Technical plan" in page.inner_text("#job-args")
            out["gold2"] = run(page, "jot-tags", "goldfish")
            page.goto(f"{base}/p/jot/f/jot-tags")
            checks["A: the second goldfish passes the revised doc: ready"] = page.get_attribute("span.phase[data-phase]", "data-phase") == "ready"
            out["build"] = run(page, "jot-tags", "build")
            out["review"] = run(page, "jot-tags", "review")
            page.goto(f"{base}/p/jot/f/jot-tags")
            checks["A: build and review run: the feature is hardened"] = out["build"] == "done" and out["review"] == "done" \
                and page.get_attribute("span.phase[data-phase]", "data-phase") == "hardened"

            # B. streaming: the gate job is slowed by three seconds after its first line
            page.goto(f"{base}/p/jot/f/jot-tags")
            page.click('#run-buttons button[data-command="gate"]')
            page.wait_for_url(f"{base}/p/jot/jobs/*")
            gate_url = page.url
            try:
                page.wait_for_selector('#log .line:has-text("warming up")', timeout=15000)
                streamed = page.get_attribute("#job-state", "data-state") != "done"
            except Exception:
                streamed = False
            checks["B: the job page shows lines while the job still runs"] = streamed
            out["gate"] = finished(page)
            page.goto(f"{base}/p/jot/f/jot-tags")
            ev_text = page.inner_text("#evidence")
            checks["A: the gate runs and its evidence is on the feature page"] = out["gate"] in ("done", "failed (exit 1)") \
                and ("Gate passed" in ev_text or "Gate failed" in ev_text) and "no drift" in ev_text
            gate_id = gate_url.rsplit("/", 1)[1]
            full = page.request.get(f"{base}/p/jot/jobs/{gate_id}/log").json()
            some = page.request.get(f"{base}/p/jot/jobs/{gate_id}/log?after=2").json()
            seqs = [l["seq"] for l in full["lines"]]
            checks["B: the log endpoint returns every line in order, and only newer ones with after"] = seqs == sorted(seqs) and len(seqs) > 3 \
                and [l["seq"] for l in some["lines"]] == [s for s in seqs if s > 2] and full["state"] == "done"

            # C. one job at a time per project; another project is not held up
            ids = []
            for _ in range(3):
                page.goto(f"{base}/p/jot/f/jot-tags")
                page.click('#run-buttons button[data-command="gate"]')
                page.wait_for_url(f"{base}/p/jot/jobs/*")
                ids.append(int(page.url.rsplit("/", 1)[1]))
            positions = page.inner_text("#job-position") if page.locator("#job-position").count() else ""
            page.goto(f"{base}/p/other")
            page.click("#new-feature summary")
            page.fill('#new-feature input[name="slug"]', "jot-tags")
            page.fill('#new-feature textarea[name="idea"]', "Let people tag notes and list only the notes with a given tag.")
            page.click("#new-feature button")
            page.wait_for_url(f"{base}/p/other/jobs/*")
            other_id = int(page.url.rsplit("/", 1)[1])
            page.goto(f"{base}/p/jot/jobs/{ids[-1]}")
            finished(page)
            jot = vev()
            starts = {e["data"]["data"]["job"]: int(e["id"]) for e in jot if e["type"] == "io.egm.job.started"}
            ends = {e["data"]["data"]["job"]: int(e["id"]) for e in jot if e["type"] == "io.egm.job.finished"}
            jobs_all = sorted(starts)
            out["overlap"] = [(j, starts[j], ends.get(j)) for j in jobs_all]
            no_overlap = all(ends.get(a, 10**12) < starts[b] for a, b in zip(jobs_all, jobs_all[1:]))
            checks["C: jot's jobs never overlap: each starts after the one before it finished"] = len(jobs_all) >= 12 and no_overlap
            checks["C: queued jobs run oldest first"] = [starts[i] for i in ids] == sorted(starts[i] for i in ids)
            checks["C: a waiting job says how many run before it"] = "run first" in positions
            oth = vev("/egm/other")
            other_start = next((int(e["id"]) for e in oth if e["type"] == "io.egm.job.started"), 10**12)
            checks["C: a job in another project starts while jot's queue is still busy"] = other_start < ends.get(ids[-1], 0)
            out["other"] = other_id

            # D. the token ceiling
            page.goto(f"{base}/p/tight")
            page.click("#new-feature summary")
            page.fill('#new-feature input[name="slug"]', "jot-tags")
            page.fill('#new-feature textarea[name="idea"]', "Let people tag notes and list only the notes with a given tag.")
            page.click("#new-feature button")
            page.wait_for_url(f"{base}/p/tight/jobs/*")
            first_tight = finished(page)
            page.goto(f"{base}/p/tight/f/jot-tags")
            page.fill("#say-form textarea", "Mostly me.")
            page.click("#say-form button")
            page.wait_for_url(f"{base}/p/tight/jobs/*")
            refused = finished(page)
            detail = page.inner_text("#job-detail") if page.locator("#job-detail").count() else ""
            checks["D: under the ceiling a job runs; at the ceiling the next is refused, saying why"] = first_tight == "done" and refused == "refused" \
                and "ceiling of 1" in detail
            tev = vev("/egm/tight")
            checks["D: the refusal is a job.finished event in Vesdb, and the job never started"] = any(
                e["type"] == "io.egm.job.finished" and e["data"]["data"]["status"] == "refused" for e in tev) \
                and sum(e["type"] == "io.egm.job.started" for e in tev) == 1
            page.goto(f"{base}/p/tight/metrics")
            checks["D: metrics show the month's tokens against the ceiling"] = "of the 1 ceiling" in page.inner_text("#month-tokens")
            ctx.close()

            # E. policy and input
            ctx, page = session("rex")
            sign_in(page, "gitlab")
            page.goto(f"{base}/p/jot")
            checks["E: a reviewer gets no New feature form"] = page.locator("#new-feature").count() == 0
            page.goto(f"{base}/p/jot/f/jot-tags")
            checks["E: a reviewer gets no run buttons or chat box"] = page.locator("#run-buttons").count() == 0 and page.locator("#say-form").count() == 0
            st = (post(page, "/p/jot/f/jot-tags/run", {"command": "gate"}), post(page, "/p/jot/new", {"slug": "x", "idea": "y"}))
            checks["E: a reviewer's run and new-feature requests are 403"] = st == (403, 403)
            page.goto(f"{base}/p/jot/jobs/{gate_id}")
            checks["E: a reviewer can read a job's log"] = "warming up" in page.inner_text("#log")
            ctx.close()
            ctx, page = session("otto")
            sign_in(page, "github")
            st = (page.goto(f"{base}/p/jot/jobs").status, page.goto(f"{base}/p/jot/jobs/{gate_id}").status,
                  page.request.get(f"{base}/p/jot/jobs/{gate_id}/log").status)
            checks["E: an outsider gets 403 on the jobs page, a job page and its log"] = st == (403, 403, 403)
            ctx.close()
            ctx, page = session("alice")
            sign_in(page, "github")
            st = {
                "unknown command": post(page, "/p/jot/f/jot-tags/run", {"command": "rm"}),
                "empty answer": post(page, "/p/jot/f/jot-tags/run", {"command": "say", "text": "  "}),
                "elephant on an existing feature": post(page, "/p/jot/f/jot-tags/run", {"command": "elephant", "text": "x"}),
                "bad name": post(page, "/p/jot/new", {"slug": "../x", "idea": "y"}),
                "duplicate name": post(page, "/p/jot/new", {"slug": "jot-tags", "idea": "y"}),
            }
            out["bad-input"] = st
            checks["E: bad input is 400: unknown command, empty answer, bad or duplicate name"] = set(st.values()) == {400}
            refusals = [e for e in vev() if e["type"] == "io.egm.policy.refused"]
            checks["E: the refusals are in Vesdb with the action"] = {e["data"]["data"]["action"] for e in refusals if e["data"]["actor"] == "gitlab:2002"} \
                >= {"run-job", "create-feature"} and any(e["data"]["actor"] == "github:1003" for e in refusals)

            # F. cost
            usage = [e for e in vev("/egm/jot/jot-tags") if e["type"] == "io.egm.usage.recorded"]
            page.goto(f"{base}/p/jot/f/jot-tags")
            shown = int(page.inner_text("#cost-tokens"))
            checks["F: the feature's tokens on the page equal its usage events in Vesdb"] = len(usage) > 5 and shown == sum(e["data"]["data"]["tokens"] for e in usage)
            checks["F: usage events carry counts, model and role, never text"] = all(
                set(e["data"]["data"]) >= {"role", "model", "tokens"} and "text" not in e["data"]["data"] for e in usage)
            checks["F: the domain events are recorded with the developer as actor"] = all(
                e["data"]["actor"] == "github:1001" for e in vev("/egm/jot/jot-tags") if e["type"] in ("io.egm.feature.created", "io.egm.build.completed"))

            # G. environment
            envs = [p.read_text() for p in envdir.glob("*.txt")]
            checks["G: every job ran with a cleared environment: no Vesdb token, no OAuth secret"] = len(envs) >= 12 and not any(
                "EGM_" in e or "should-not-leak" in e or "s3cret" in e for e in envs) and all("PATH=" in e for e in envs)

            # H. replayed state equals live state
            status = subprocess.run([EGM, "status"], cwd=co, capture_output=True, text=True).stdout
            cli_phase = next((l.split()[1] for l in status.splitlines() if l.startswith("jot-tags")), None)
            rp = subprocess.run([web.EGM_AUDIT, "replay-vesdb", "--project", "jot"], capture_output=True, text=True,
                                env=dict(os.environ, VESDB_URL=f"http://127.0.0.1:{vport}", VESDB_TOKEN=web.TOKEN))
            replay = rp.stdout
            out["replay"] = (rp.returncode, rp.stdout, rp.stderr)
            rep = dict(l.split("\t")[0:2] for l in replay.strip().splitlines() if "\t" in l)
            page.goto(f"{base}/p/jot/f/jot-tags")
            live_phase = page.get_attribute("span.phase[data-phase]", "data-phase")
            live_hash = page.locator('p.muted:has-text("doc hash")').first.inner_text().rsplit(" ", 1)[1]
            rep_hash = next((l.split("\t")[2] for l in replay.strip().splitlines() if l.startswith("jot-tags\t")), None)
            out["phases"] = (cli_phase, rep.get("jot-tags"), live_phase, live_hash, rep_hash)
            checks["H: egm-dsh's own status, the replay of Vesdb and the app show the same phase and doc"] = cli_phase == rep.get("jot-tags") == live_phase \
                and live_hash == rep_hash
            def snap():
                page.goto(f"{base}/p/jot/f/jot-tags")
                got = [page.get_attribute("span.phase[data-phase]", "data-phase")] + [page.inner_text(x) for x in ("#feature-jobs", "#chat-log", "#cost-table", "#history")]
                page.goto(f"{base}/p/jot/jobs")
                return got + [page.inner_text("#jobs")]
            before = snap()
            out["rebuild"] = team("rebuild")
            time.sleep(1.0)
            after = snap()
            out["snap-diff"] = [(i, str(b)[:600], str(a)[:600]) for i, (b, a) in enumerate(zip(before, after)) if b != a]
            checks["H: after rebuilding every view from Vesdb the feature, chat, jobs, cost and history are the same"] = before == after \
                and out["rebuild"].returncode == 0

            # I. durability
            page.goto(f"{base}/p/jot/f/jot-tags")
            page.click('#run-buttons button[data-command="gate"]')
            page.wait_for_url(f"{base}/p/jot/jobs/*")
            doomed = int(page.url.rsplit("/", 1)[1])
            page.goto(f"{base}/p/jot/f/jot-tags")
            page.click('#run-buttons button[data-command="gate"]')
            page.wait_for_url(f"{base}/p/jot/jobs/*")
            waiting = int(page.url.rsplit("/", 1)[1])
            for _ in range(50):
                if any(e["type"] == "io.egm.job.started" and e["data"]["data"]["job"] == doomed for e in vev()):
                    break
                time.sleep(0.2)
            procs["team"].send_signal(signal.SIGKILL)
            procs["team"].wait()
            time.sleep(1)
            orphans = subprocess.run(["pgrep", "-f", f"egm-team-jobs/{doomed}.jsonl"], capture_output=True, text=True).stdout.split()
            checks["I: the job's egm-dsh dies with the worker, so nothing changes the checkout unrecorded"] = orphans == []
            serve()
            page.goto(f"{base}/p/jot/jobs/{waiting}")
            waited = finished(page)
            page.goto(f"{base}/p/jot/jobs/{doomed}")
            doomed_state = page.inner_text("#job-state")
            checks["I: after a crash the running job is finished as abandoned"] = doomed_state == "abandoned" and "run it again" in page.inner_text("#job-detail")
            checks["I: the job queued behind it survives the restart and runs"] = waited in ("done", "failed (exit 1)")
            ctx.close()
            browser.close()
    finally:
        for p in procs.values():
            if p.poll() is None:
                p.terminate()
                try:
                    p.wait(timeout=10)
                except Exception:
                    p.kill()
        try:
            web.as_postgres(f"/usr/lib/postgresql/16/bin/pg_ctl -D {work}/pg/data -m fast stop")
        except Exception:
            pass

    for name, ok in checks.items():
        print(("PASS " if ok else "FAIL ") + name)
    print("SUMMARY " + json.dumps({"passed": sum(map(bool, checks.values())), "failed": sum(not v for v in checks.values())}))
    if not all(checks.values()) or len(checks) < 30:
        for k, v in out.items():
            print(f"--- {k}: {v}"[:1500])
        print("--- team.log:", (work / "team.log").read_text()[-4000:] if (work / "team.log").exists() else "")
        sys.exit(1)
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
