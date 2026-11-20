//! `egm-team`: the egm-team web server.
//!
//!   egm-team serve   --config egm-team.toml     serve the app and keep the views current
//!   egm-team rebuild --config egm-team.toml     drop the views and rebuild them from Vesdb
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
    Rebuild,
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

fn vesdb(cfg: &Config) -> Arc<dyn AuditLog> {
    Arc::new(VesdbLog::new(&cfg.vesdb_url, &cfg.vesdb_token, &cfg.source))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let cfg = Config::load(&cli.config)?;
    match cli.cmd {
        Cmd::Serve => {
            let log = vesdb(&cfg);
            let client = db::connect(&cfg.database_url).await?;
            let wake = Arc::new(tokio::sync::Notify::new());
            tokio::spawn(egm_web::projector(cfg.clone(), log.clone(), wake.clone()));
            let listener = tokio::net::TcpListener::bind(&cfg.listen).await?;
            eprintln!("egm-team: serving {} on {}", cfg.public_url, cfg.listen);
            let state = App { cfg, db: Arc::new(client), log, refresh: wake };
            topcoat::serve(listener, app::router(state)).await?;
        }
        Cmd::Rebuild => {
            let mut client = db::connect(&cfg.database_url).await?;
            let n = db::rebuild(&mut client, vesdb(&cfg)).await?;
            println!("rebuilt the views from {n} event(s)");
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
