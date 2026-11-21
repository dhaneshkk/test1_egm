# egm-dsh and egm-team: user guide

This guide is task-first. Find your role, follow the steps. The [README](../README.md) explains how everything works and why; this guide says what to type and what to expect.

| You are | You use | Start at |
|---|---|---|
| A developer designing and building features | the `egm-dsh` command line | [Part 1](#part-1-developers-the-egm-dsh-command-line) |
| A reviewer, approver or teammate following the work | the `egm-team` web app | [Part 2](#part-2-the-team-web-app) |
| The person who runs egm-team for the team | `egm-team`, Postgres, Vesdb | [Part 3](#part-3-running-egm-team) |
| An auditor checking the record | `egm-audit`, `vesdb-verify` | [Part 4](#part-4-auditors-checking-the-record) |

What exists today: the command line; the audit path into Vesdb; and the team web app. The web app covers sign-in, projects, features, docs, reviews and evidence, plus running egm-dsh from the browser, comments, alarm labels, reviews and merge approval with separation of duties. It also connects to GitHub and GitLab (pull and merge requests, commit statuses, webhooks), shows metrics and cost, and exports evidence for SOC 2. All five phases of the [implementation plan](https://claude.ai/artifact/J4NTLcJfrnrnbUGuH6ERpW) are built; the production field test is next. Every screen of the web app is shown in [The pages, one by one](#the-pages-one-by-one).

## Contents

- [Install](#install)
- [Part 1: developers, the egm-dsh command line](#part-1-developers-the-egm-dsh-command-line)
- [Part 2: the team web app](#part-2-the-team-web-app) (with [every page, one by one](#the-pages-one-by-one))
- [Part 3: running egm-team](#part-3-running-egm-team)
- [Part 4: auditors, checking the record](#part-4-auditors-checking-the-record)
- [Reference](#reference)
- [Troubleshooting](#troubleshooting)

## Install

**egm-dsh** (the command line) needs Rust 1.97 or newer, git, and for real runs [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) (`dsh`, a Node package) and a `DEEPSEEK_API_KEY`. On Linux, install `bubblewrap` so model-written tests run in a sandbox.

```sh
git clone https://github.com/dhaneshkk/test1_egm && cd test1_egm
cargo install --path .                 # egm-dsh
cargo install --path crates/egm-audit  # egm-audit: events into Vesdb, receipts, bundles
```

**egm-team** (the web app) needs Rust 1.99 or newer (Topcoat's minimum), PostgreSQL 14 or newer, and a running [Vesdb](https://github.com/dhaneshkk/test1_vesdb).

```sh
cargo install --path crates/egm-web    # installs the egm-team binary
```

Everything at once, as CI does: `cargo build --workspace --release`. The single-cell Colab notebook builds the workspace, checks every Lean proof and runs every test suite.

## Part 1: developers, the egm-dsh command line

### 1. Try it offline first

```sh
egm-dsh demo
```

This creates `./egm-dsh-demo`, a tiny note-taking app, and adds a feature with a scripted model: the first goldfish review fails on a real gap in the design, you answer, the doc is redrafted, a fresh goldfish passes, and only then is code built. It takes seconds and costs nothing.

### 2. Set up your project

```sh
cd my-project
export DEEPSEEK_API_KEY=sk-...
egm-dsh init --agent dsh --preset balanced --acceptance cargo test   # or pytest, npm test, ...
egm-dsh doctor                                                       # sandbox, git, tools, key present
```

`init` creates `.egm-dsh/`. Commit it, except what its own `.gitignore` excludes (sessions, the lock). Presets choose a model and effort for each role: `economy` is cheapest, `balanced` is the default, `quality` uses the strongest model everywhere. Change one later with `egm-dsh preset economy`, or one role with `egm-dsh set-role`.

### 3. Design a feature with the elephant

Each feature gets a short name (a *slug*, such as `rate-limit`) and one design doc under `docs/designs/`.

```sh
egm-dsh elephant rate-limit --idea "Limit each API key to 100 requests a minute"
egm-dsh elephant rate-limit --say "Keys are per customer; bursts up to 20 are fine"
egm-dsh draft rate-limit
egm-dsh show rate-limit          # read the doc
```

The elephant reads your repository but cannot change it. Keep talking with `--say` until the doc says what you mean.

Already have a design doc? Put it under the gate instead: `egm-dsh adopt rate-limit --doc docs/designs/rate-limit.md`.

### 4. Let the goldfish test the doc

```sh
egm-dsh goldfish rate-limit
```

Fresh reviewers with no memory read only the doc and the files it links. They explain it back, criticize it, check it is the simplest design, and say whether they could build it. The result is READY or NOT READY.

When it is NOT READY:

```sh
egm-dsh escalate rate-limit      # the elephant answers what the repository settles; the rest become decisions
egm-dsh decide rate-limit        # list the open decisions
egm-dsh decide rate-limit 2 B    # pick option B for decision 2 (or: --other "your answer")
egm-dsh redraft rate-limit       # the decisions go into the doc
egm-dsh goldfish rate-limit      # a fresh goldfish reads the new revision
```

`egm-dsh loop rate-limit` runs those rounds for you and stops whenever it needs you. It never builds.

### 5. Freeze the acceptance tests (optional, recommended)

```sh
egm-dsh set-spec --tests-first on
egm-dsh tests rate-limit         # tests are written first; each must fail now; a goldfish checks each one
egm-dsh check rate-limit         # run the executable spec at any time
```

Frozen tests cannot be edited by the build, and the build must turn them green.

### 6. Build, review, keep docs and code in step

```sh
egm-dsh build rate-limit         # refused unless a goldfish passed this exact revision of the doc
egm-dsh review rate-limit        # a fresh, deliberately hostile review of the code
egm-dsh drift                    # exit 2 if built code changed without a design change
egm-dsh status                   # the phase of every feature
```

A feature moves through four phases:

| Phase | Means | Moves on when |
|---|---|---|
| `designing` | the doc is being shaped | a goldfish passes the current revision |
| `ready` | a goldfish passed this exact doc | the build succeeds and its checks pass |
| `built` | code written from an approved doc, checks green | the hostile review finds nothing blocking or major |
| `hardened` | reviewed | — |

Editing the doc at any point sends the feature back to `designing`. This rule is proved in Lean (`proofs/EgmGate.lean`).

### 7. Tiny fixes

For a one-line bug with a test that shows it:

```sh
egm-dsh fix gap-ids "Removed notes let add reuse an id" --files jot/cli.py --check tests/test_jot.py::GapTest::test_add_after_gap
```

The fix lane writes a short doc, checks the test fails, builds, checks it passes and reviews the diff. If the change grows beyond the fix limits it is reverted and moved to the full lane.

### 8. Evidence for the pull request

```sh
egm-dsh gate rate-limit                       # every evidence check; exit 1 on any failure (CI-friendly, no model)
egm-dsh evidence rate-limit --out PR.md       # the pull request description: gate, criteria, uncovered lines, seeded bugs, risk
egm-dsh risk rate-limit                       # risk tier and the suggested reviewer
egm-dsh log-time rate-limit 15 --what "read the pull request"
egm-dsh ci-template > .github/workflows/egm-gate.yml
```

### 9. Watch the cost

```sh
egm-dsh estimate rate-limit      # sessions and tokens the next goldfish round would use
egm-dsh usage                    # tokens and cost per role and model so far
egm-dsh set-ceiling 2000000      # refuse a round whose estimate would cross this many tokens
```

### 10. Send your work to the team app

The simplest way is to work in the team app itself (Part 2, "Run egm-dsh from the browser"): every step is then recorded for you. If you run egm-dsh on your own machine instead, record events while you work and push them to the audit log:

```sh
egm-dsh --events events.jsonl goldfish rate-limit     # --events works with every command
egm-dsh --events events.jsonl build rate-limit
VESDB_URL=https://vesdb.example VESDB_TOKEN=... \
  egm-audit push events.jsonl --project my-project --job "$(git rev-parse --short HEAD)"
```

`--events` changes nothing on screen. `push` is safe to repeat with the same `--job`: events already written are not written again. Push your commits too; the web app reads docs, reviews and evidence from the team's checkout.

## Part 2: the team web app

### Sign in

Open the address your admin gave you and choose **Sign in with GitHub** or **Sign in with GitLab**. egm-team asks the provider only who you are; it never sees your password or your repositories. If you see "You are not a member of any project yet", ask an admin to add you (Part 3, step 6).

### A feature from idea to merge

The usual path, with who does each step. Everything is on the feature's page unless it says otherwise.

| Step | Who | What they do | What the app records |
|---|---|---|---|
| 1 | Developer | **New feature** on the board: a name and the idea | `job.requested`; the elephant's questions appear in the Design chat |
| 2 | Developer | Answers the elephant in the **Design chat**, then **Draft the design doc** | one job per answer; the doc lands in the checkout |
| 3 | Developer | **Goldfish**: fresh reviewers test the doc; answers what they could not, redrafts the sections they name, runs Goldfish again until it is ready | `goldfish.verdict`; the phase moves to *ready* |
| 4 | Reviewer | Labels each goldfish alarm **Real** or **Noise** | `alarm.labelled`; this scores the goldfish (Metrics) |
| 5 | Developer | **Build**, **Review**, **Gate** | `build.completed`, `review.completed`, `gate.verdict`; on GitHub or GitLab the branch is pushed, a pull or merge request opened, `egm/evidence-gate` set |
| 6 | Reviewer or approver | Finds the feature in the **Review inbox**, comments, then **Approve** (or **Request changes** with a note) | `review.submitted`; the author is mailed |
| 7 | Reviewer (low or medium risk) or approver (high risk) | **Approve merge**, once the panel's four checks are green | `merge.approved`; `egm/merge-approval` turns green on the code host |
| 8 | Anyone with merge rights on the code host | Merges the pull or merge request | `merge.completed` from the webhook; a later revert or a **Report a defect** counts against the gate |

The author can do steps 1, 2, 3 and 5, and comment, but never 6 or 7 on their own feature.

### The pages, one by one

The screenshots come from the browser tests (`tests/e2e/run_web_e2e.py`, `run_jobs_e2e.py`, `run_host_e2e.py`). Those tests run real egm-dsh output, the worker, Vesdb and Postgres, with fake GitHub and GitLab servers. Every page has the same header: **Review inbox**, your login, and **Sign out**. Every project page has the same tabs: **Features**, **Jobs**, **Metrics**, **Members** and **Audit trail**.

#### Sign in (`/sign-in`)

![Sign in](screenshots/01-sign-in.png)

Choose **Sign in with GitHub** or **Sign in with GitLab**. egm-team asks only who you are. Sign-in links expire after ten minutes and work only in the browser tab that started them.

#### Projects (`/`)

![Projects](screenshots/02-projects.png)

The projects you hold a role in, with their feature count and code host. Signed in without any role, you see a note to ask an admin instead:

![Signed in, no role yet](screenshots/09-outsider-no-projects.png)

#### Feature board (`/p/<project>`)

![Feature board](screenshots/03-feature-board.png)

One column per phase (*designing*, *ready*, *built*, *hardened*), newest change first. A red line on a card means the feature's history in the audit log contains a step the gate refuses; tell your admin. Developers see **New feature** above the board, and any running or queued jobs:

![Starting a feature](screenshots/19-new-feature.png)

#### Feature page (`/p/<project>/f/<feature>`)

The page you will use most. From top to bottom:

![The whole feature page](screenshots/28-feature-page-full.png)

1. **Phase, doc path and doc hash.**
2. **Design chat** (features started from the app): your messages and the elephant's replies, and a box to answer it. **Run**: Draft (tick sections to redraft only those), Goldfish, Build, Review and Gate. **Jobs**: this feature's last ten jobs. **Cost**: tokens and dollars by role and model.

   ![Design chat](screenshots/21-design-chat.png)

   ![Run buttons, jobs and cost](screenshots/22-run-jobs-cost.png)

3. **Design doc** as it is in the checkout, and its **revisions** (git commits that changed it).
4. **Goldfish reviews**: each round's verdict and how many alarms it kept.
5. **Goldfish alarms**: one row per alarm, with **Real** and **Noise** buttons for reviewers and approvers. **Comments** follow, on the whole doc, a section or an alarm.

   ![Alarm labels and comments](screenshots/15-labels-comments.png)

6. **Reviews** and the **Merge approval** panel: the four checks, who approved, and why you cannot approve if you cannot.

   ![Reviews and the merge panel](screenshots/16-review-merge.png)

   The author sees why they cannot review their own work:

   ![The author's view](screenshots/18-author-view.png)

7. **Code host**: the pull or merge request, both commit statuses now and as last set on the host, the merge, any revert, reported defects, **Report a defect**, and **Export the audit bundle**.

   ![Code host](screenshots/26-code-host.png)

8. **Evidence**: the evidence gate's checks, risk tier, seeded-bug score and drift. **History**: every event of the feature.

   ![Feature page, top](screenshots/04-feature-page.png)

#### The doc at a revision (`/p/<project>/f/<feature>/rev/<commit>`)

![Doc at a revision](screenshots/05-doc-at-revision.png)

The design doc exactly as it was at that commit, linked from the feature's revision list.

#### Jobs (`/p/<project>/jobs`) and a job (`/p/<project>/jobs/<id>`)

![Job queue](screenshots/23-jobs-queue.png)

Every job run from the app, newest first, with its state (queued, running, done, failed, refused, abandoned) and tokens. The line at the top shows this month's tokens and the ceiling. One job runs at a time per project; a waiting job says how many run before it.

![A running job](screenshots/20-job-running.png)

A job page follows the job as it runs, line by line, and keeps the output afterwards. When a job is refused, the page says why:

![Refused at the ceiling](screenshots/24-ceiling-refused.png)

#### Review inbox (`/inbox`)

![Review inbox](screenshots/14-review-inbox.png)

Features waiting for your review in every project where you review. Each one is built, its evidence gate passed for the current doc, nobody has approved it yet, and you did not write it. Highest risk first.

#### Metrics (`/p/<project>/metrics`)

![Metrics](screenshots/06-metrics.png)

From top to bottom:

- features by phase, and goldfish and gate verdict counts;
- the **goldfish MCC** from reviewers' labels, with its confusion matrix and the 29-per-class warning;
- **human minutes** per feature;
- **review times**: medians with 95% intervals;
- **gate outcomes** after merge;
- **cost** by feature, and this month's tokens against the ceiling;
- event counts.

![Goldfish MCC and human minutes](screenshots/17-metrics-mcc.png)

![Review times and gate outcomes](screenshots/27-review-gate-metrics.png)

![Cost and the ceiling](screenshots/25-metrics-cost.png)

#### Members (`/p/<project>/members`)

![Members](screenshots/07-members.png)

Who holds which roles, and who granted them. Admins change roles with `egm-team grant` (Part 3); the page is read-only.

#### Audit trail (`/p/<project>/audit`)

![Audit trail](screenshots/08-audit-trail.png)

Every event of the project, newest first: who, what, when, and the event's data (hashes, counts and verdicts, never code). Each is in Vesdb's tamper-evident log. With a code host connected, the statuses egm-team set, merges, reverts and refused webhooks appear here too:

![Audit trail with code-host events](screenshots/29-audit-trail-code-host.png)

#### When you may not see something

![No access](screenshots/10-outsider-no-access.png)

![Not found](screenshots/11-not-found.png)

A project you hold no role in gives **No access** (403), and the refusal is recorded. A feature, revision or job that does not exist gives **Not found** (404).

#### On a phone

| | |
|---|---|
| ![Board on a phone](screenshots/12-board-phone.png) | ![Feature page on a phone](screenshots/13-feature-phone.png) |

Every page fits a phone's width; wide tables scroll sideways inside the page.

### Run egm-dsh from the browser

Developers drive a feature from the app instead of a terminal. Each button queues a **job**. A worker runs egm-dsh in the team's checkout and writes everything to the audit log, with you as the actor. One job runs at a time per project, oldest first. A job page follows the job as it runs and keeps its output afterwards.

1. **Start a feature.** On the board, open **New feature**, give it a name and the idea. The elephant reads the repository and asks its questions.
2. **Answer in the Design chat** on the feature page. Each answer is a job; the elephant's reply appears in the chat when it is done.
3. **Draft the design doc.** With nothing ticked the elephant writes the whole doc; tick sections to redraft only those after the goldfish found a gap.
4. **Goldfish, Build, Review, Gate** run the same steps as the command line, with the same rules. egm-dsh still decides whether a step may run: a build of a doc no goldfish has passed fails in its job, saying why.

**Cost.** Every model call is recorded as `usage.recorded` with its role, model, token counts and price. The feature page shows the feature's tokens and cost by role. The Metrics page shows each feature's total and the project's tokens this month. With the offline mock agent the counts are estimates, and the page says so.

**Monthly ceiling.** An admin can give a project a token ceiling per calendar month (UTC). The worker checks it before each job starts and refuses the job once the month's use has reached the ceiling; the refusal is recorded with the reason. A job is not stopped part-way, so the job that crosses the ceiling finishes.

### Work together on a feature

Everything below is on the feature page, under the goldfish reviews. Each action is written to the audit log first; if Vesdb does not accept it, nothing happens.

- **Comment.** Pick what the comment is about (the whole doc, one `##` section, or one alarm) and write it. Developers, reviewers and approvers can comment, the author included. The author is mailed when someone else comments.
- **Label alarms** (reviewers and approvers). For each goldfish alarm, say whether it was a **Real** gap in the doc or **Noise**. Click again to change your mind; the latest label counts. Notes are not listed: they never count against a doc, so there is nothing to score. Your labels score the goldfish on the Metrics page: an alarm the goldfish kept is its prediction of a real gap, your label is the truth. Below 29 labels in either class the MCC carries a warning; treat it as a sketch until then.
- **Review** (reviewers and approvers, once the feature is built). **Approve**, or **Request changes** with a note saying what must change. A review is for the doc as it is now; after the doc changes, earlier reviews are shown as "an earlier revision" and no longer count.
- **Approve the merge.** The panel lists the four things a merge approval needs and which are missing: the author is recorded, the feature is built, the evidence gate passed for this doc, and someone other than the author approved it in a review. A low- or medium-risk feature can then be approved by a reviewer; a high-risk one, or one whose risk was never assessed, only by an approver. The author never can, whatever roles they hold. You then merge on GitHub or GitLab, where the `egm/merge-approval` status turns green (below).

### On GitHub or GitLab

When the project has a repository configured, everything above shows up on the code host too:

- The feature's branch `egm-dsh/<feature>` is pushed after each build, and a pull request (GitHub) or merge request (GitLab) is opened for it. Its description shows the evidence gate's checks and how to get the audit bundle. egm-team rewrites it as things change, so edit the discussion, not the description.
- Two statuses sit on the branch head. **`egm/evidence-gate`** is green when the evidence gate passed after the latest build, for the current doc. **`egm/merge-approval`** is green when, after that, someone other than the author approved the merge in egm-team. Your admin makes both required, so the merge button stays locked until both are green. A new push to the branch turns both back to waiting.
- When the change is merged, egm-team records who merged it. If the merge commit is later reverted, or someone uses **Report a defect** on the feature page, that counts against the gate on the Metrics page (Gate outcomes).
- **Export the audit bundle** (approvers, admins, auditors) downloads every event of the feature with Vesdb's proofs, for an auditor to check offline.

**Mail.** When mail is configured (Part 3), egm-team mails reviewers and approvers when a feature's evidence gate passes, and mails the author when someone else comments, reviews, or approves the merge. Nobody is mailed about their own action. The address is the one your GitHub or GitLab account shows publicly; with none, you get no mail.

**Minutes.** Each time you open a feature or act on it, egm-team adds the time since your previous visit or action, at most 30 minutes (a longer gap is time away), the same way egm-dsh counts human minutes. Nothing else about your activity is kept.

### What you can do with each role

| Role | Can |
|---|---|
| developer | read, comment, report a defect, start features, run egm-dsh jobs |
| reviewer | read, comment, report a defect, label alarms, review, approve low- and medium-risk merges |
| approver | everything a reviewer can, approve high-risk merges, export the audit bundle |
| admin | read, export the audit bundle; roles change with `egm-team grant` on the server, not in the app |
| auditor | read, export the audit bundle |

Nobody can review or approve a feature they wrote, whatever roles they hold.

### When something says no

- **403 Forbidden**: you have no role in that project, or your roles do not allow that action (reviewing your own feature, approving a high-risk merge without the approver role). The refusal is recorded in the audit log with your id and the action.
- **A job says "failed"**: egm-dsh stopped with an error, shown at the end of the job's output (for example a build refused because no goldfish passed the current doc). Fix what it names and run the step again.
- **A job says "refused"**: it never started. The reason is on the job page: the project reached its monthly token ceiling, or you no longer hold a role that may run it.
- **A job says "abandoned"**: the worker stopped while it ran (a restart or crash). Its egm-dsh process stopped with it, so nothing changed unrecorded; run the step again.
- **"cannot approve yet: …"**: the merge panel's reason, such as no approving review by someone else yet, or a gate that passed for an older doc. The attempt is recorded too.
- **Sign-in failed**: start again from the sign-in page in the same browser tab. Sign-in links expire after ten minutes and work only in the browser that started them.
- **The page is out of date**: the app reads new events every second or so; reload. A running job's page updates by itself.

## Part 3: running egm-team

### What runs where

| Part | Role |
|---|---|
| `egm-team serve` | The web app. Stateless apart from Postgres, so you can run more than one. With a `[worker]` section it also runs the worker |
| `egm-team worker` | The worker alone, for running jobs on another machine. It needs the checkouts, egm-dsh and the model key; run one per deployment (a per-project database lock keeps a second one off a busy project) |
| PostgreSQL | Views of the audit log, sessions, job output and the mail outbox. `egm-team rebuild` recreates every view from Vesdb; only job output and sessions live in Postgres alone |
| Vesdb | The record. Run it highly available as its README describes: two servers on a shared bucket (`--role auto`), at least one witness under a separate account |
| The project checkout | The git clone egm-dsh works in. The worker owns it: jobs run there, and the app reads docs, reviews and evidence from it. Initialise it once with `egm-dsh init`, and give it a git identity for build commits |
| GitHub and/or GitLab OAuth apps | Sign-in |

Put egm-team behind TLS (a load balancer or reverse proxy) and set `public_url` to the `https://` address; session and sign-in cookies are then marked Secure.

### 1. Register the OAuth apps

- **GitHub**: Settings → Developer settings → OAuth Apps → New. Homepage: your `public_url`. Authorization callback URL: `<public_url>/auth/github/callback`.
- **GitLab**: User or group Settings → Applications. Redirect URI: `<public_url>/auth/gitlab/callback`. Scope: `read_user` only. Confidential: yes.

Keep each client secret for step 3.

### 2. Prepare Postgres and Vesdb

```sh
createdb egm                       # egm-team creates its tables on start
```

Create a Vesdb token with the read and write scopes (add audit if the same token will fetch receipts and bundles).

### 3. Write the configuration

`egm-team.toml`:

```toml
listen       = "127.0.0.1:8080"
public_url   = "https://egm-team.example"
database_url = "host=/run/postgresql dbname=egm"
vesdb_url    = "https://vesdb.internal:8443"                # Vesdb behind its TLS proxy (or both servers)
vesdb_ca     = "/etc/egm-team/vesdb-ca.pem"                 # the CA that proxy's certificate comes from
admins       = ["github:1234"]                              # admin role in every project
refresh_ms   = 1000                                         # how often to read new events

[[project]]
name     = "my-project"            # letters, digits, - and _
checkout = "/srv/egm/my-project"
host     = "github"                # or "gitlab"
monthly_tokens = 20000000          # optional: refuse jobs once the month's use reaches this
repo        = "acme/my-project"    # optional: GitHub owner/repo or GitLab project path
base_branch = "main"               # what pull and merge requests target
retention_days = 365               # optional: how long people's words and job output are kept

[github]
client_id = "Iv1.abc"
# base_url and api_url only for GitHub Enterprise

[gitlab]
client_id = "abc"
base_url  = "https://gitlab.com"   # or your own GitLab

[worker]                           # run egm-dsh jobs from the app
egm_dsh     = "/usr/local/bin/egm-dsh"
pass_env    = ["DEEPSEEK_API_KEY"] # what a job may see, beyond PATH, HOME, LANG, LC_ALL, TMPDIR, USER
timeout_s   = 1800                 # a job still running after this is stopped
max_parallel = 2                   # jobs at once across projects; always one per project

[mail]                             # optional: notifications
host     = "smtp.example"
port     = 587
tls      = "starttls"              # or "tls" (port 465), or "none" (a local relay only)
from     = "egm-team <egm-team@example>"
username = "egm-team"              # optional; the password comes from EGM_SMTP_PASSWORD
```

Secrets come from the environment only:

| Variable | Holds |
|---|---|
| `EGM_VESDB_TOKEN` | The Vesdb token |
| `EGM_GITHUB_CLIENT_SECRET` | Needed when `[github]` is configured |
| `EGM_GITLAB_CLIENT_SECRET` | Needed when `[gitlab]` is configured |
| `EGM_SMTP_PASSWORD` | Needed when `[mail]` has a `username` |
| `EGM_DATABASE_URL` | Optional: replaces `database_url`, so a database password stays out of the file |
| `EGM_HOST_TOKEN_<PROJECT>` | For a project with a `repo`: a token that may push `egm-dsh/*` branches, open pull or merge requests and set commit statuses (GitHub fine-grained: contents, pull requests, commit statuses; GitLab: a project access token with `api`). `<PROJECT>` is the project name in capitals, `-` as `_` |
| `EGM_WEBHOOK_SECRET_<PROJECT>` | For a project with a `repo`: the webhook secret, at least 20 characters |
| `EGM_VESDB_REDACT_TOKEN` | Only when an admin runs `egm-team redact --apply`; never in the server's environment |

A job's process starts with an empty environment plus the allowlist above, so none of egm-team's own secrets (`EGM_*`) can reach a build; the configuration refuses an `EGM_` name in `pass_env`.

A user id is the provider and the provider's numeric id, such as `github:1234` or `gitlab:5678`; it stays the same if they rename their account. Find a GitHub id at `https://api.github.com/users/<login>`, a GitLab id on the user's profile page.

### 4. Connect GitHub or GitLab (optional)

For each project with a `repo`:

1. **Webhook.** GitHub: repository Settings → Webhooks → Add: payload URL `<public_url>/hooks/github/<project>`, content type `application/json`, the secret, events **Pull requests** and **Pushes**. GitLab: project Settings → Webhooks: URL `<public_url>/hooks/gitlab/<project>`, the secret token, **Merge request events** and **Push events**.
2. **Branch protection** on the base branch: require the status checks `egm/evidence-gate` and `egm/merge-approval` (GitHub: required status checks; GitLab: "Pipelines must succeed", which counts external statuses). Allow only egm-team's token to push `egm-dsh/*`.
3. The checkout's `origin` is not used: the worker pushes to `push_url` (by default the host's HTTPS URL) with the token, passed in git's environment, never on a command line.

### 5. Start

```sh
egm-team --config egm-team.toml serve
curl -s https://egm-team.example/health      # ok
```

### 6. Give people roles

```sh
egm-team --config egm-team.toml grant --project my-project --user github:5678 --roles developer --by github:1234
egm-team --config egm-team.toml grant --project my-project --user gitlab:777 --roles reviewer,approver --by github:1234
egm-team --config egm-team.toml grant --project my-project --user gitlab:777 --roles ""   # remove
```

Each grant is written to Vesdb as `member.role-changed`, so the history of who had access, and who gave it, is part of the record. The `worker` role cannot be given to a person.

A field-test team of four could be two developers, one reviewer and one approver. With a single reviewer, the approver also reviews when the reviewer is away; the policy allows it because the approver is never the author.

### 7. Day to day

| Task | How |
|---|---|
| Rebuild the views (after a restore, or if in doubt) | `egm-team --config egm-team.toml rebuild` |
| Back up | Vesdb is the record; follow its backup guidance. Postgres needs no backup: every view is rebuilt from Vesdb, and losing it only signs people out and resets the activity minutes. The mail outbox survives `rebuild`, so nobody is mailed twice |
| See what jobs did | The project's Jobs page; each job keeps its full output. Output lives in Postgres only (it may quote code) and survives `rebuild` |
| See failed mail | `SELECT to_addr, subject, error FROM outbox WHERE error IS NOT NULL` in Postgres; egm-team's log names each failure |
| See refusals and sign-ins | The project's Audit trail page, or read `/egm/_system` and `/egm/<project>` from Vesdb |
| Restore drill (quarterly) | `egm-team rebuild` on a fresh database; it records `ops.restore-tested` (see `docs/soc2/OPERATIONS.md`) |
| Erase old words (quarterly) | `egm-team redact --project P` lists them; with `--apply` and `EGM_VESDB_REDACT_TOKEN` it erases them from Vesdb and records `ops.redacted` |
| Evidence for the auditor | `egm-team evidence --from 2027-01-01 --to 2027-04-01 --out DIR`: `evidence.md`, `evidence.json` and `bundle.json` |
| Readiness for the load balancer | `GET /health/ready`: 200 when Postgres and Vesdb answer, 503 naming which does not |
| Upgrade | Topcoat is pinned to 0.12.1 and lives only in `crates/egm-web`; upgrade it there and run the browser test (`tests/e2e/run_web_e2e.py`) |

If Vesdb is unreachable, nobody can sign in (a sign-in is recorded before the session starts) and refusals are reported in egm-team's log instead of the audit log. Pages people are already signed in to keep working from the views.

## Part 4: auditors, checking the record

### What is recorded

Every change to a feature, every sign-in and failed sign-in, every role change and every refusal by the policy, as events under `/egm/<project>/<feature>`, `/egm/<project>` and `/egm/_system`. Events carry hashes, counts, verdicts and user ids; never source code or document text. The full list is in the [reference](#event-types).

### Check one event, or a whole feature

```sh
export VESDB_URL=https://vesdb.example VESDB_TOKEN=...   # a token with the audit scope
egm-audit receipt 42                     # the signed receipt and inclusion proof for event 42
egm-audit bundle 40 41 42 > bundle.json  # an offline evidence bundle
vesdb-verify bundle bundle.json --log-key log.pub --witness-key witness.pub
```

`vesdb-verify` needs only the public keys; it does not trust any server. To check the whole log in its bucket: `vesdb-verify bucket s3://bucket/prefix --log-key log.pub`.

### The evidence pack

`egm-team evidence --from --to --out DIR` (run by the operator) writes, for the period:

- `evidence.md`: every SOC 2 control in scope, how egm-team meets it, and a summary;
- `evidence.json`: the data behind each control. It includes every merge, with whether the gate had passed and someone other than the author had approved it. Merges without a valid approval are listed as **exceptions**. It also includes access changes, refusals per user and action, restore drills, redactions and the models used;
- `bundle.json`: an offline bundle of the period's change-management events. Check it with `vesdb-verify bundle bundle.json --log-key log.pub`.

`docs/soc2/CONTROLS.md` maps each control, including the ones the organisation owns. `THREAT_MODEL.md` scopes the penetration test, and `OPERATIONS.md` covers backups, recovery targets and retention.

### Check that the app shows the record

```sh
egm-audit replay-vesdb --project my-project    # each feature's phase, rebuilt from the log alone
```

The phase printed for each feature must match the board. That it always does is proved in Lean (`EgmReplay.replay_emitted`), and the audit export holds exactly the events the app replays (`VesdbLink.export_exact`).

### The guarantees behind the controls

| Control | Where it is proved or tested |
|---|---|
| Nobody reviews or approves their own work; only an approver approves a high-risk change; the worker never approves | `proofs/EgmPolicy.lean` |
| A merge approval needs a built feature, a gate passed for the current doc and an approving review by someone else | `EgmPolicy.merge_never_self`, `merge_needs_gate`, `merge_needs_other_review`, `merge_high_needs_approver`; `policy.rs` checks every combination; `run_web_e2e.py` (the author's attempts and the reviewer's high-risk approval are refused and recorded) |
| The goldfish MCC counts every reviewer label exactly once, the latest per alarm | `proofs/EgmLabels.lean`: `confusion_total`, `perfect_agreement`, `relabel_moves_one`; `run_web_e2e.py` (MCC on the page equals the MCC of the clicks, before and after a rebuild) |
| A person with no role in a project can read nothing in it | `EgmPolicy.read_iff_member`; `run_web_e2e.py` (outsider gets 403 on every page, each refusal recorded) |
| One job at a time per project, oldest first, and none started at or over the monthly token ceiling | `proofs/EgmQueue.lean`: `start_keeps_one_per_project`, `finish_keeps_one_per_project`, `start_is_oldest`, `start_under_ceiling`, `refuse_only_over`; `queue.rs` checks every queue of up to four jobs; `run_jobs_e2e.py` (no two jot jobs overlap in Vesdb; a job at the ceiling is refused) |
| The state the app shows from jobs is the state the log replays to | `run_jobs_e2e.py`: egm-dsh's own `status`, `egm-audit replay-vesdb` and the app agree on phase and doc hash; after `rebuild` the feature, chat, jobs, cost and history are the same |
| A job never sees egm-team's secrets | `worker.rs` unit test of the job environment; `run_jobs_e2e.py` reads every job's environment |
| The code host merges only what egm-team's statuses allow | `proofs/EgmHost.lean`; `run_host_e2e.py` (GitHub and GitLab) |
| An exported bundle verifies offline, before and after redaction | `run_host_e2e.py` with `vesdb-verify bundle` |
| Every page checks the policy first | `crates/egm-web/tests/policy_lint.rs` fails the build otherwise |
| No build without a goldfish pass of the current doc | `proofs/EgmGate.lean` |
| The record cannot be changed unnoticed | Vesdb's Merkle log, witness-signed checkpoints and offline bundles (proved in Vesdb's repository); `run_audit_e2e.py` (one changed byte fails verification) |

## Reference

### egm-dsh commands by task

| Task | Commands |
|---|---|
| Set up | `init`, `doctor`, `preset`, `roles`, `set-role`, `set-safety`, `set-spec`, `set-verification` |
| Design | `elephant`, `draft`, `adopt`, `show`, `set-scope`, `set-draft-words`, `set-diagrams`, `diagrams` |
| Review the design | `goldfish`, `escalate`, `decide`, `redraft`, `loop`, `set-review`, `set-reviewers` |
| Steps and size | `after`, `set-limits`, `set-fix-limits` |
| Tests first | `tests`, `check` |
| Build | `build`, `review`, `fix`, `drift`, `status` |
| Evidence | `gate`, `mutate`, `evidence`, `risk`, `log-time`, `ci-template`, `set-gate` |
| Cost | `estimate`, `usage`, `budget`, `set-budget`, `set-ceiling` |
| Learning | `label`, `metrics`, `set-triage` |

`egm-dsh <command> --help` gives every option. The README's command table explains each one.

### Roles and actions

`crates/egm-core/src/policy.rs`, mirrored by `proofs/EgmPolicy.lean`:

| Action | developer | reviewer | approver | admin | auditor | worker |
|---|---|---|---|---|---|---|
| read | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| create feature, run job | ✓ | | | | | |
| comment | ✓ | ✓ | ✓ | | | |
| label alarm, submit review, approve merge | | ✓ | ✓ | | | |
| record job | | | | | | ✓ |
| change roles, change settings | | | | ✓ | | |
| export | | | ✓ | ✓ | ✓ | |

Two rules apply on top: an author can never submit a review of, or approve, their own feature; a high-risk merge needs the approver role.

### Event types

Stored in Vesdb as `io.egm.<type>`.

| Type | Written by | Subject | Data |
|---|---|---|---|
| `feature.created` | egm-dsh | feature | doc path, doc hash, dependencies |
| `doc.revised` | egm-dsh | feature | doc hash, phase |
| `goldfish.verdict` | egm-dsh | feature | ready or not, doc hash, phase |
| `decision.recorded` | egm-dsh | feature | decision number, chosen option, doc hash |
| `criteria.frozen` | egm-dsh | feature | doc hash, per criterion: check, test file hash, red |
| `build.completed` | egm-dsh | feature | doc hash, phase |
| `review.completed` | egm-dsh | feature | clean or not, doc hash, phase |
| `gate.verdict` | egm-dsh | feature | pass, checks, seeded bugs caught, risk tier |
| `member.role-changed` | `egm-team grant` | project | user, roles (empty: removed) |
| `auth.signed-in` | egm-team | `_system` | user id, login, name, provider |
| `auth.sign-in-failed` | egm-team | `_system` | provider, reason |
| `policy.refused` | egm-team | project | action, reason; the actor is the refused user |
| `comment.added` | egm-team | feature | target (doc, section or alarm), words key, doc hash |
| `alarm.labelled` | egm-team | feature | round, alarm, valid or noise, whether the goldfish kept it, doc hash |
| `review.submitted` | egm-team | feature | approve or changes, words key, doc hash |
| `merge.approved` | egm-team | feature | doc hash, risk tier |
| `job.requested` | egm-team | feature | command, its arguments (an idea or answer is the person's words, like a comment); the actor is the person |
| `job.started` | worker | feature | job id, worker name |
| `job.finished` | worker | feature | job id, status (ok, failed, refused, timeout, abandoned, error), exit code, tokens, reason |
| `usage.recorded` | egm-dsh | feature | role, agent, model, token counts, price in dollars (none for an unpriced model) |
| `words.recorded` | egm-team | feature | a person's words for the action that follows it (comment, review note, idea, answer, defect note), under a random key; erased by `egm-team redact` |
| `host.pr-opened` | worker | feature | pull or merge request number, URL, branch |
| `host.status-set` | worker | feature | commit, status name, state, description |
| `merge.completed` | webhook | feature | request number, merge commit, base branch, who merged |
| `change.reverted` | webhook | feature | merge commit, revert commit |
| `defect.reported` | egm-team | feature | words key (the note is in `words.recorded`) |
| `export.created` | egm-team | feature | events in the bundle, its SHA-256 |
| `ops.restore-tested` | `egm-team rebuild` | `_system` | events replayed, seconds |
| `ops.redacted` | `egm-team redact` | project | events erased, the cutoff |

People's words enter the log only in `words.recorded` events, so retention can erase them and keep the actions. Not written yet: `project.settings-changed` (settings live in the configuration file).

### Files egm-dsh keeps

| Path | Holds |
|---|---|
| `.egm-dsh/config.json` | Agent, presets, roles, gate and review settings |
| `.egm-dsh/features/<slug>.json` | Each feature's gate state, phase, frozen spec |
| `.egm-dsh/reviews/<slug>-<n>.json` | Each goldfish review round |
| `.egm-dsh/evidence/<slug>.json` | The last evidence gate record |
| `.egm-dsh/events.jsonl`, `usage.jsonl`, `ledger.jsonl` | Command timings, token usage, decisions |
| `docs/designs/<slug>.md` | The design docs |

## Troubleshooting

| Message or symptom | What it means | What to do |
|---|---|---|
| `build` is refused: no goldfish passed | The doc changed since the last pass, or never passed | Run `goldfish` again on the current doc |
| `redraft` is refused: a decision is open | An escalated question still needs your choice | `decide <f>` to list them, then choose |
| `goldfish` is refused: over the ceiling | The round's estimate would cross `set-ceiling` | Raise the ceiling, or `preset economy` |
| `drift` exits 2 | Built files changed without a design change | Update the doc and go through the gate, or revert the change |
| `gate` fails "seeded bugs caught" | The tests miss bugs planted in the changed lines | Strengthen the tests; `mutate <f>` lists the bugs that survived |
| `egm-audit push`: "set VESDB_URL" | The environment is missing | Export `VESDB_URL` and `VESDB_TOKEN` |
| egm-team won't start: "set EGM_..." | A secret is missing from the environment | Export the variable it names |
| A feature card shows a red line | The audit log holds a gate step the gate refuses: written by something other than egm-dsh | Check the feature's audit history; report it |
| A built feature is not in the review inbox | The gate has not passed for the current doc, the merge is already approved, or you wrote it | Open the feature: the merge panel says which |
| No mail arrives | `[mail]` is not configured, your provider account shows no public email, or the SMTP server refused it | Ask the admin to check the outbox (Part 3, step 7) |
| Jobs stay "queued" | No worker runs: add `[worker]` to the configuration, or start `egm-team worker` | The Jobs page says when the server runs no worker |
| Every job of one project is refused | The project reached `monthly_tokens` this month | Raise it in the configuration, or wait for the next month (UTC) |
| `egm/merge-approval` stays pending | No approval yet by someone other than the author after the latest gate, or a new push reset it | Open the feature in egm-team: the Code host panel says why |
| A pull request never appears | No `repo`, a missing `EGM_HOST_TOKEN_<PROJECT>`, or the token cannot push | egm-team's log names the failing call |
| Webhook deliveries fail with 401 | The secret differs from `EGM_WEBHOOK_SECRET_<PROJECT>` | Set the same secret on both sides; each failure is a refusal in the audit trail |
| Everyone is signed out after a restart | Sessions live in Postgres; a new database means new sessions | Sign in again; nothing in the record is lost |
