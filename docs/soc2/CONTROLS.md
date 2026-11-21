# SOC 2 control matrix for egm-team

Scope: **Security** (the common criteria, always in), **Availability** and **Confidentiality**, for a SOC 2 Type II report. The mapping is a reading of the 2017 Trust Services Criteria (revised points of focus, 2022). Confirm it with your auditor before the observation period starts.

How to read it:

- **egm-team** rows are controls the app runs and records. `egm-team evidence --from --to --out DIR` writes the evidence for the period, in the section named here. `crates/egm-web/src/soc2.rs` holds the same list, and a test keeps this file and the code in step.
- **Organisation** rows are yours alone. The app gives no evidence for them; you keep policies, records and sign-offs. Templates of what auditors expect are in the last column.
- **Proof or test** names the Lean theorem or the end-to-end check behind the claim.

## Controls egm-team evidences

| Control | Category | What the auditor looks for | How egm-team meets it | Evidence section | Proof or test |
|---|---|---|---|---|---|
| CC6.1 | Security | Identity and authentication | GitHub or GitLab OAuth sign-in (GitLab with PKCE), hashed `__Host-` sessions, every sign-in recorded before the session starts | `access` | `run_web_e2e.py` (sign-in, PKCE, forged state refused) |
| CC6.2 | Security | Access granted and removed through an authorised process | Roles change only through `egm-team grant`; each change is `member.role-changed` naming who granted it | `access` | `run_web_e2e.py` (grants, refusal of the worker role to a person) |
| CC6.3 | Security | Least privilege; separation of duties | A role-by-action policy on every page and route; an author never reviews or approves their own feature; high risk needs the approver | `separation_of_duties` | `EgmPolicy` (all theorems); `policy_lint.rs`; `run_web_e2e.py` |
| CC5.1 | Security | Control activities that mitigate risk | The `egm/merge-approval` commit status, required on the code host, is green only after an approval by someone other than the author, after the passing gate, for the current doc | `separation_of_duties` | `EgmHost.merge_success_other_approver`, `author_alone_never_merges`, `new_build_resets`; `run_host_e2e.py` |
| CC6.6 | Security | Protection at the system boundary | TLS at the reverse proxy; Postgres and Vesdb on a private network; Topcoat's origin policy refuses cross-site posts; strict CSP, no framing, no sniffing | `boundary` | `run_host_e2e.py` (security self-test: headers, cross-site post, XSS, ids of other projects, path traversal) |
| CC6.7 | Security | Data protected in transmission | HTTPS to the app with HSTS; HTTPS to Vesdb through its TLS proxy with a pinned CA; TLS to SMTP and the code hosts | `boundary` | `run_host_e2e.py` (Vesdb over TLS; a wrong CA refused) |
| CC6.8 | Security | Unauthorised or malicious software prevented | Model-written code and checks run under egm-dsh's sandbox; each job gets an empty environment plus an allowlist; a dependency audit runs in CI | `software` | `run_jobs_e2e.py` (job environments); CI `cargo deny check advisories` |
| CC7.2 | Security | Security events monitored | Every refusal by the policy or the gate, failed sign-in and bad webhook signature is an event in Vesdb | `monitoring` | `run_web_e2e.py`, `run_host_e2e.py` (bad signature refused and recorded) |
| CC7.3 | Security | Security events evaluated | The pack lists refusals and failed sign-ins per project, user and action, for the reviewer of the period | `monitoring` | `run_host_e2e.py` (the pack counts them) |
| CC8.1 | Security | Changes authorised, tested and approved before release | The Lean-proven evidence gate (frozen tests, seeded bugs, no drift); both commit statuses required; every merge recorded from the code host's webhook; exceptions listed | `change_management` | `EgmGate`, `EgmHost`; `run_host_e2e.py` (a merge with and without approval) |
| CC4.1 | Security | Monitoring of controls; logs that cannot be changed quietly | Vesdb's Merkle log, signed checkpoints, offline bundles; the pack carries a bundle of the period's change-management events | `log_integrity` | `VesdbLink.export_exact`; `run_host_e2e.py` (bundle verifies offline; a changed byte fails) |
| CC9.2 | Security | Vendors and business partners known | Models used with token counts, code hosts, sign-in providers, from the record | `vendors` | `run_host_e2e.py` |
| A1.1 | Availability | Capacity and health monitored | `/health/ready` checks Postgres and Vesdb for the load balancer; job queue and monthly token use per project | `availability` | `run_host_e2e.py` (ready, and not ready with Vesdb down) |
| A1.2 | Availability | Recovery infrastructure and backups | Vesdb is the record, with its own HA on a shared bucket and backups; every Postgres view is rebuilt from it | `availability` | `docs/soc2/OPERATIONS.md` |
| A1.3 | Availability | Recovery tested | `egm-team rebuild` is the restore drill; each run is recorded as `ops.restore-tested` | `availability` | `run_jobs_e2e.py` (pages unchanged after rebuild); `run_host_e2e.py` (drill recorded) |
| C1.1 | Confidentiality | Confidential information identified and retained as needed | The log holds hashes and verdicts, never code; people's words in their own events; `retention_days` per project | `confidentiality` | `run_host_e2e.py` (no source code in any event) |
| C1.2 | Confidentiality | Confidential information disposed of | Hourly purge of Postgres-only data; `egm-team redact` erases old words and sign-in details in Vesdb and is recorded as `ops.redacted`; proofs stay valid | `confidentiality` | `run_host_e2e.py` (words gone after redaction, actions kept, bundle still verifies) |

## Controls the organisation owns

| Area | Criteria | What you need | Notes |
|---|---|---|---|
| Governance and ethics | CC1.1–CC1.5 | Code of conduct, org chart, board or owner oversight, background checks, performance reviews | egm-team records who did what; it does not cover hiring or oversight |
| Communication | CC2.1–CC2.3 | Security policies published to staff and customers; a way to report issues | Point staff to `docs/USER_GUIDE.md` for how the controls work |
| Risk assessment | CC3.1–CC3.4 | A yearly risk assessment, including fraud risk and changes | Start from `docs/soc2/THREAT_MODEL.md` |
| Monitoring activities | CC4.2 | Deficiencies reported and fixed | Review each evidence pack's exceptions and refusals; record the follow-up |
| Policies and procedures | CC5.2–CC5.3 | Written policies for access, change, incident response, vendor management | The controls above implement parts of them |
| Access reviews | CC6.2 (review part), CC6.4, CC6.5 | A quarterly review of who holds which role; physical access (your cloud provider's report); disposal of devices | Use the pack's `access.members_now` as the review list |
| Incident response | CC7.4–CC7.5 | A plan, a drill each year, post-incident reviews | Vesdb holds the timeline of any incident in the app |
| Business continuity | CC9.1, A1.2 (site), A1.3 (plan) | A continuity plan; backups of Vesdb's bucket in a second zone | See `docs/soc2/OPERATIONS.md` |
| Vendor management | CC9.2 (contracts) | Contracts and SOC reports from the cloud provider, model provider and code hosts | The pack lists which vendors were used |
| Independent testing | CC4.1, CC7.1 | A penetration test by an independent firm each year | `docs/soc2/SECURITY_SELF_TEST.md` is a self-test, not a penetration test |
| Confidentiality agreements | C1.1 | NDAs with staff and contractors; data classification policy | |
