# Operating egm-team: availability, backup and recovery

For SOC 2 A1 (Availability) and the confidentiality disposal procedure (C1.2). The deployment in `deploy/` is cloud-neutral; the table at the end maps each part to AWS, Google Cloud and Azure.

## Targets

| Measure | Target | Why it is achievable |
|---|---|---|
| Availability of the app | 99.5% a month (about 3.6 hours down) | Stateless app servers behind a load balancer; Vesdb takes over in 10–12 s with its defaults |
| RPO (data that may be lost) | 0 for the record; job output since the last database backup | Vesdb acknowledges a write only after the bucket holds it; everything else is rebuilt from Vesdb |
| RTO (time to recover) | 1 hour | A new database and `egm-team rebuild` (seconds for thousands of events) |

egm-team's availability cannot exceed Vesdb's. Vesdb's own HA is described in its README; a second region needs a replicated bucket and is a manual step there.

## Health

- `GET /health`: the process is up.
- `GET /health/ready`: Postgres answers and Vesdb answers a read within 3 s. Point the load balancer here. A 503 says which is down.
- Vesdb: `/health/writer` (200 only on the writer) and `/health/ready` on each server.

## Backups

| Data | Where | Backup |
|---|---|---|
| The record | Vesdb's bucket | Object Lock (compliance mode) on log and checkpoint objects; bucket replication to a second zone or region; `vesdb-verify bucket` monthly |
| Vesdb signing key, witness key | Secrets store | Two copies, under different accounts |
| Postgres | Managed database | Daily snapshots for job output only; everything else rebuilds from Vesdb |
| Checkouts | The worker's disk | None needed: clone again from the code host; unpushed work is at most one job |

## Restore drill (quarterly, for A1.3)

1. Point a fresh database at a copy of the configuration (`EGM_DATABASE_URL`).
2. Run `egm-team --config egm-team.toml rebuild`. It replays every event and records `ops.restore-tested` with the count and the seconds taken.
3. Open a few features and compare with production; run `egm-team evidence` for the quarter and check the drill is listed under `availability.restore_drills`.
4. For Vesdb, follow its README: start a server on a copy of the bucket and run `vesdb-verify bucket`.

## Retention and disposal (C1.2)

- Set `retention_days` per project. The worker deletes job output, sent mail and expired sessions older than that every hour.
- Once a quarter, an admin erases people's words older than the period from Vesdb:
  ```sh
  egm-team --config egm-team.toml redact --project field-test            # lists what would go
  EGM_VESDB_REDACT_TOKEN=... egm-team --config egm-team.toml redact --project field-test --apply --by github:1234
  ```
  The redact token has only the `redact` scope and is not in the app's environment. Vesdb records the redaction and keeps every proof valid; egm-team records `ops.redacted` and rebuilds its views.

## Evidence for the auditor

```sh
egm-team --config egm-team.toml evidence --from 2027-01-01 --to 2027-04-01 --out evidence-2027-q1
vesdb-verify bundle evidence-2027-q1/bundle.json --log-key log.key.pub --witness-key witness.key.pub
```

`evidence.md` lists every control with its evidence; `evidence.json` holds the data; `bundle.json` lets the auditor check the period's change-management events offline, trusting no server.

## Mapping to a cloud provider

| Part in `deploy/` | AWS | Google Cloud | Azure |
|---|---|---|---|
| Caddy (public TLS) | Application Load Balancer + ACM | External HTTPS load balancer + managed certificate | Application Gateway + Key Vault certificate |
| Caddy (Vesdb TLS, internal CA) | Internal ALB + ACM Private CA | Internal HTTPS load balancer + Certificate Authority Service | Internal Application Gateway + private CA |
| Shared bucket | S3 with Object Lock | Cloud Storage with retention lock | Blob Storage with immutability policies |
| Postgres | RDS for PostgreSQL 16 | Cloud SQL for PostgreSQL 16 | Azure Database for PostgreSQL 16 |
| `.env` | Secrets Manager | Secret Manager | Key Vault |
| Witness under another account | Separate AWS account | Separate project | Separate subscription |
