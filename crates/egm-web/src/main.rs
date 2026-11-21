//! `egm-team`: the egm-team web server.
//!
//!   egm-team serve   --config egm-team.toml     serve the app and keep the views current
//!   egm-team worker  --config egm-team.toml     run only the worker (needs [worker])
//!   egm-team rebuild --config egm-team.toml     drop the views and rebuild them from Vesdb
//!   egm-team evidence --from 2026-11-01 --to 2026-12-01 --out DIR
//!                                               the SOC 2 evidence pack for a period
//!   egm-team redact  --project P [--before DATE] [--apply]
//!                                               erase people's words older than the retention
//!                                               period from Vesdb (EGM_VESDB_REDACT_TOKEN)
//!   egm-team grant   --config egm-team.toml --project P --user github:123 --roles developer,reviewer
//!                                               set someone's roles (recorded in Vesdb;
//!                                               an empty --roles removes them)

use clap::{Parser, Subcommand};
use egm_audit::{AuditEvent, AuditLog, VesdbLog};
use egm_core::policy::Role;
use egm_web::{app, config::Config, db, App};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "egm-team", version, about = "egm-team: the team web app over egm-dsh")]
struct Cli {
    #[arg(long, global = true, default_value = "egm-team.toml")]
    config: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Serve,
    Worker,
    Rebuild,
    Evidence {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        out: PathBuf,
    },
    Redact {
        #[arg(long)]
        project: String,
        /// Erase words older than this date (default: today minus the project's retention_days).
        #[arg(long)]
        before: Option<String>,
        /// Without it, only list what would be erased.
        #[arg(long)]
        apply: bool,
        /// Who runs it, for the record (default: the local user name).
        #[arg(long)]
        by: Option<String>,
    },
    Grant {
        #[arg(long)]
        project: String,
        #[arg(long)]
        user: String,
        #[arg(long, default_value = "")]
        roles: String,
        /// Who is granting, for the record (default: the local user name).
        #[arg(long)]
        by: Option<String>,
    },
}

fn vesdb_log(cfg: &Config, token: &str) -> anyhow::Result<VesdbLog> {
    let ca = match &cfg.vesdb_ca {
        Some(p) => Some(std::fs::read(p).map_err(|e| anyhow::anyhow!("vesdb_ca {}: {e}", p.display()))?),
        None => None,
    };
    Ok(VesdbLog::with_ca(&cfg.vesdb_url, token, &cfg.source, ca.as_deref())?)
}

fn vesdb(cfg: &Config) -> Arc<dyn AuditLog> {
    Arc::new(vesdb_log(cfg, &cfg.vesdb_token).expect("vesdb_ca was read when the configuration loaded"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let cfg = Config::load(&cli.config)?;
    vesdb_log(&cfg, &cfg.vesdb_token)?; // a bad CA file fails here, not at the first write
    match cli.cmd {
        Cmd::Serve => {
            let log = vesdb(&cfg);
            let client = db::connect(&cfg.database_url).await?;
            let wake = Arc::new(tokio::sync::Notify::new());
            tokio::spawn(egm_web::projector(cfg.clone(), log.clone(), wake.clone()));
            let listener = tokio::net::TcpListener::bind(&cfg.listen).await?;
            eprintln!("egm-team: serving {} on {}", cfg.public_url, cfg.listen);
            let client = Arc::new(client);
            if let Some(m) = cfg.mail.clone() {
                let mail_db = Arc::new(db::connect(&cfg.database_url).await?);
                tokio::spawn(egm_web::mail::sender(m, cfg.public_url.clone(), mail_db));
            }
            let work = Arc::new(tokio::sync::Notify::new());
            if cfg.worker.is_some() {
                tokio::spawn(egm_web::worker::run(cfg.clone(), log.clone(), work.clone()));
                eprintln!("egm-team: the worker runs in this process");
            }
            let state = App { cfg, db: client, log, refresh: wake, work };
            topcoat::serve(listener, app::router(state)).await?;
        }
        Cmd::Worker => {
            anyhow::ensure!(cfg.worker.is_some(), "add a [worker] section to {}", cli.config.display());
            eprintln!("egm-team: worker started");
            egm_web::worker::run(cfg.clone(), vesdb(&cfg), Arc::new(tokio::sync::Notify::new())).await;
        }
        Cmd::Rebuild => {
            let mut client = db::connect(&cfg.database_url).await?;
            let t0 = std::time::Instant::now();
            let n = db::rebuild(&mut client, vesdb(&cfg)).await?;
            let secs = t0.elapsed().as_secs_f64();
            println!("rebuilt the views from {n} event(s) in {secs:.1} s");
            // the restore drill is evidence for SOC 2 A1.3
            let ev = AuditEvent {
                kind: "ops.restore-tested".into(),
                project: egm_web::SYSTEM.into(),
                feature: String::new(),
                actor: format!("cli:{}", std::env::var("USER").unwrap_or_else(|_| "admin".into())),
                data: serde_json::json!({"events": n, "seconds": (secs * 10.0).round() / 10.0}),
            };
            let key = format!("ops/{}", egm_web::oauth::random_hex(16)?);
            let log = vesdb(&cfg);
            let written = tokio::task::spawn_blocking(move || log.append(&key, &[ev])).await??;
            for w in &written {
                db::apply(&client, w).await?;
            }
        }
        Cmd::Evidence { from, to, out } => {
            let client = db::connect(&cfg.database_url).await?;
            db::sync(&mut db::connect(&cfg.database_url).await?, vesdb(&cfg)).await?;
            let (pack, bundle) = egm_web::soc2::pack(&cfg, &client, vesdb(&cfg), &from, &to).await?;
            std::fs::create_dir_all(&out)?;
            std::fs::write(out.join("evidence.json"), serde_json::to_string_pretty(&pack)?)?;
            std::fs::write(out.join("evidence.md"), egm_web::soc2::markdown(&pack))?;
            if let Some(b) = bundle {
                std::fs::write(out.join("bundle.json"), serde_json::to_string_pretty(&b)?)?;
            }
            println!("wrote the evidence pack for {from} to {to} in {}", out.display());
        }
        Cmd::Redact { project, before, apply, by } => {
            let pc = cfg.project(&project).ok_or_else(|| anyhow::anyhow!("no project `{project}`"))?;
            let before = match before {
                Some(b) => b,
                None => {
                    let days = pc.retention_days.ok_or_else(|| anyhow::anyhow!("project `{project}` has no retention_days; pass --before"))?;
                    let client = db::connect(&cfg.database_url).await?;
                    client.query_one("SELECT to_char((now() - make_interval(days => $1)) AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS')", &[&(days as i32)]).await?.get(0)
                }
            };
            let log = vesdb(&cfg);
            let (l1, l2, p2) = (log.clone(), log.clone(), project.clone());
            let evs = tokio::task::spawn_blocking(move || l1.history(&p2, None)).await??;
            let signs = tokio::task::spawn_blocking(move || l2.history(egm_web::SYSTEM, None)).await??;
            let ids = egm_web::retention::to_redact(&evs, &signs, &before);
            println!("{} event(s) of `{project}` hold words or sign-in details from before {before}", ids.len());
            if apply && !ids.is_empty() {
                let token = std::env::var("EGM_VESDB_REDACT_TOKEN").map_err(|_| anyhow::anyhow!("set EGM_VESDB_REDACT_TOKEN (a Vesdb token with the redact scope)"))?;
                let r = vesdb_log(&cfg, &token)?;
                let (ids2, reason) = (ids.clone(), format!("egm-team retention for {project}: words before {before}"));
                tokio::task::spawn_blocking(move || r.redact(&ids2, &reason)).await??;
                let by = by.unwrap_or_else(|| format!("cli:{}", std::env::var("USER").unwrap_or_else(|_| "admin".into())));
                let ev = AuditEvent { kind: "ops.redacted".into(), project: project.clone(), feature: String::new(), actor: by,
                                      data: serde_json::json!({"events": ids.len(), "before": before}) };
                let key = format!("ops/{}", egm_web::oauth::random_hex(16)?);
                tokio::task::spawn_blocking(move || log.append(&key, &[ev])).await??;
                let mut client = db::connect(&cfg.database_url).await?;
                let n = db::rebuild(&mut client, vesdb(&cfg)).await?;
                println!("erased {} event(s); rebuilt the views from {n} event(s)", ids.len());
            } else if !apply {
                println!("nothing erased; run again with --apply");
            }
        }
        Cmd::Grant { project, user, roles, by } => {
            anyhow::ensure!(cfg.project(&project).is_some(), "no project `{project}` in {}", cli.config.display());
            let mut parsed = vec![];
            for r in roles.split(',').map(str::trim).filter(|r| !r.is_empty()) {
                let role: Role = serde_json::from_value(serde_json::json!(r)).map_err(|_| anyhow::anyhow!("unknown role `{r}`"))?;
                anyhow::ensure!(role != Role::Worker, "the worker role belongs to egm-worker, not to people");
                parsed.push(role);
            }
            let by = by.unwrap_or_else(|| format!("cli:{}", std::env::var("USER").unwrap_or_else(|_| "admin".into())));
            let ev = AuditEvent {
                kind: "member.role-changed".into(),
                project: project.clone(),
                feature: String::new(),
                actor: by,
                data: serde_json::json!({"user": user, "roles": parsed}),
            };
            let key = format!("grant/{}", egm_web::oauth::random_hex(16)?);
            let log = vesdb(&cfg);
            tokio::task::spawn_blocking(move || log.append(&key, &[ev])).await??;
            println!("{user} in {project}: {}", if parsed.is_empty() { "removed".to_string() } else { roles });
        }
    }
    Ok(())
}
