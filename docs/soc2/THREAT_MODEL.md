# egm-team threat model

STRIDE over egm-team's parts and the data that crosses between them. Written for the SOC 2 risk assessment (CC3.2) and as the starting point for the independent penetration test. Review it each year, and whenever a part or a trust boundary changes.

## Parts and trust boundaries

| Part | Holds | Trusts | Boundary |
|---|---|---|---|
| Browser | A session cookie | Nothing | Internet → reverse proxy (TLS) |
| Reverse proxy (Caddy or a load balancer) | The public TLS key | — | Public → private network |
| `egm-team serve` | OAuth client secrets, the Vesdb token, the SMTP password, sessions (hashed) | Postgres, Vesdb (pinned CA), the OAuth providers | Private network |
| Worker (in `serve` or `egm-team worker`) | Code-host tokens, the model key (passed to jobs) | The checkouts | Runs untrusted code: model-written builds and tests |
| egm-dsh job | Only the allowlisted environment | Its sandbox | One process per job; dies with the worker |
| Postgres | Views (rebuilt from Vesdb), sessions, job output | — | Private network only |
| Vesdb (two servers, shared bucket) and its witness | The record | Its signing key; the witness under another account | Private network only, TLS through the proxy |
| GitHub / GitLab | Code, pull and merge requests, statuses | egm-team's token and webhook secret | Internet, HTTPS |
| Model provider | Prompts (design docs, code under review) | The model key | Internet, HTTPS; a vendor in CC9.2 |

## Threats and mitigations

| # | STRIDE | Threat | Mitigation | Tested by | Residual risk |
|---|---|---|---|---|---|
| T1 | Spoofing | Someone signs in as another person | OAuth with the provider; `state` and PKCE (GitLab) checked; sessions are random, stored hashed, `__Host-` HttpOnly SameSite=Lax cookies | `run_web_e2e.py` (forged state, PKCE) | A provider account taken over takes over egm-team access: require 2FA at the provider |
| T2 | Spoofing | A forged webhook records a merge or a revert | HMAC-SHA256 signature (GitHub) or secret token (GitLab), compared in constant time; failures refused (401) and recorded | `run_host_e2e.py` (bad signature) | A leaked webhook secret: rotate it with the token |
| T3 | Tampering | Someone changes the record to hide a self-approval | Vesdb's Merkle log, signed receipts, witness-cosigned checkpoints; offline bundles verified by `vesdb-verify` | `run_audit_e2e.py`, `run_host_e2e.py` (changed byte fails) | One operator holding both the log key and the witness key: keep the witness under another account |
| T4 | Tampering | A build writes outside its files, or reads secrets | egm-dsh's sandbox (bubblewrap) for model-written code and checks; each job gets an empty environment plus an allowlist; the Implementation map limits changed files | `run_jobs_e2e.py` (job environments), egm-dsh's `run_safety_e2e.py` | The worker host itself: run it as an unprivileged user, in its own VM or container |
| T5 | Tampering | A push to the feature branch after approval slips into the merge | `egm/merge-approval` needs an approval after the latest gate verdict, and a new build resets both statuses (`EgmHost.new_build_resets`) | `host.rs` unit test, `EgmHost` | Pushes by people straight to the branch on the code host: protect `egm-dsh/*` branches so only egm-team's token can push |
| T6 | Repudiation | Someone denies approving or merging | Every action is an event naming its actor, written before it takes effect; merges come from the code host's signed webhook | All end-to-end suites | — |
| T7 | Information disclosure | A member of one project reads another's | The policy check is the first statement of every handler (`policy_lint.rs`); ids are checked against the project in the path | `run_host_e2e.py` (another project's records by id) | — |
| T8 | Information disclosure | Script injection through a comment or a doc | Topcoat escapes all text; job output is added with `textContent`; strict CSP with no inline script | `run_host_e2e.py` (script in a comment) | A Topcoat escaping bug: keep the CSP strict |
| T9 | Information disclosure | Source code or personal data in the audit log | Events carry hashes, counts and verdicts; people's words in their own events, erasable by `egm-team redact` | `run_host_e2e.py` (no source in any event; redaction) | Prompts sent to the model provider contain code: a vendor risk, covered by the provider's contract |
| T10 | Information disclosure | Traffic to Vesdb read on the network | TLS through the proxy with a pinned CA; Vesdb on a private network | `run_host_e2e.py` (wrong CA refused) | — |
| T11 | Denial of service | A burst of jobs or a runaway model bill | One job at a time per project; monthly token ceiling; egm-dsh's per-feature ceiling and budgets | `run_jobs_e2e.py` | The job that crosses the ceiling finishes |
| T12 | Denial of service | Vesdb down stops every write | Two Vesdb servers on a shared bucket with automatic takeover; `/health/ready` tells the load balancer | `run_audit_e2e.py` (failover), `run_host_e2e.py` (readiness) | A bucket outage stops writes: the provider's object storage SLA |
| T13 | Elevation of privilege | An author approves their own change | Lean-proven policy on every action; the required merge status refuses an author's approval | `EgmPolicy`, `EgmHost`, `run_web_e2e.py`, `run_host_e2e.py` | An admin with code-host rights who bypasses branch protection: the evidence pack lists every merge without a valid approval as an exception |
| T14 | Elevation of privilege | A cross-site form posts as the signed-in user | Topcoat's origin policy refuses cross-site state changes; SameSite=Lax cookies | `run_host_e2e.py` | — |
| T15 | Elevation of privilege | A vulnerable dependency | `cargo deny check advisories` in CI; reviewed exceptions in `deny.toml` with a reason and a date | CI `audit` job | Advisories not yet published |

## Out of scope here, for the organisation

Hosting and physical security (the cloud provider's SOC 2 report), staff devices, the code hosts' own security, the model provider's handling of prompts, and the independent penetration test.
