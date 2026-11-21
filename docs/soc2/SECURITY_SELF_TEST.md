# Security self-test

An automated self-test of egm-team's main attack surfaces. It runs in CI on every push as part of `tests/e2e/run_host_e2e.py` (section I, and the webhook, TLS and redaction checks). Result on 20 Nov 2026: **every check passed, so there are no high-severity findings.**

**This is not a penetration test.** SOC 2 auditors expect an independent firm to test the deployed system at least once a year (CC4.1, CC7.1). Give them `docs/soc2/THREAT_MODEL.md` as the scope. This self-test only keeps known issues from coming back between those tests.

| Area | Check | Result |
|---|---|---|
| Headers | CSP (`script-src 'self'`, `frame-ancestors 'none'`), `X-Frame-Options: DENY`, `nosniff`, `Referrer-Policy` on pages and on error pages; HSTS when served over HTTPS | pass |
| CSRF | A form post with a cross-site `Origin` and `Sec-Fetch-Site: cross-site` is refused (403) | pass |
| XSS | `<script>` and `<img onerror>` in a comment are shown as text; no dialog opens; no element is created | pass |
| Access control | A member of one project gets 403 for another project's feature, export and job by id | pass |
| Path and id tricks | Encoded `../` in feature, revision and project segments, and SQL in a job id, give 400, 403 or 404 | pass |
| Session cookie | `__Host-` prefix, HttpOnly, SameSite=Lax, path `/` | pass |
| Webhooks | A bad HMAC signature gets 401 and is recorded; a redelivery is recorded once | pass |
| Transport | Vesdb over TLS with the pinned CA; a different CA is refused by egm-team and egm-audit | pass |
| Secrets | No job's environment holds `EGM_*`, the Vesdb token or an OAuth secret (`run_jobs_e2e.py`) | pass |
| Data minimisation | No event holds source code; redacted words are gone from Vesdb while proofs still verify | pass |
| Dependencies | `cargo deny check advisories`: no unreviewed advisory. Six advisories in `zip` 2.6.1 (through `plantuml-core`) are reviewed as unreachable and listed in `deny.toml` with a date to look again | pass (6 reviewed) |

Findings to watch, none high severity:

- **Rate limiting**: egm-team does not rate-limit sign-in or webhook endpoints. Put the reverse proxy's rate limiting in front of `/auth/` and `/hooks/`. Medium.
- **Code-host branch protection**: egm-team can only enforce what the code host's branch protection requires. Make both `egm/*` statuses required on the base branch, and allow only egm-team's token to push `egm-dsh/*`. Medium; an evidence-pack exception shows any merge without approval.
- **`style-src 'unsafe-inline'`**: the pages use inline styles. Script stays locked to the origin. Low.
