//! `egm-audit`: move egm-dsh's events into Vesdb and check them.
//!
//!   egm-audit replay EVENTS.jsonl            gate phase of each feature, from an `--events` file
//!   egm-audit push EVENTS.jsonl --project P --job J [--actor A]
//!                                            write its domain events to Vesdb, one write per event,
//!                                            idempotency key `egm/J/<n>` (safe to run again)
//!   egm-audit replay-vesdb --project P       gate phase of each feature, from Vesdb alone
//!   egm-audit receipt ID                     signed receipt and inclusion proof
//!   egm-audit bundle ID...                   offline evidence bundle for an auditor
//!
//! Vesdb is named by `VESDB_URL` and `VESDB_TOKEN` (never on the command line).

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use egm_audit::{key, replay, AuditEvent, AuditLog, VesdbLog, Written};
use egm_core::events::{DomainEvent, TYPES};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "egm-audit", version, about = "Move egm-dsh's events into Vesdb and check them")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Replay { events: PathBuf },
    Push {
        events: PathBuf,
        #[arg(long)]
        project: String,
        #[arg(long)]
        job: String,
        #[arg(long, default_value = "worker")]
        actor: String,
    },
    ReplayVesdb {
        #[arg(long)]
        project: String,
    },
    Receipt { id: u64 },
    Bundle { ids: Vec<u64> },
}

fn vesdb() -> anyhow::Result<VesdbLog> {
    let url = std::env::var("VESDB_URL").context("set VESDB_URL")?;
    let token = std::env::var("VESDB_TOKEN").context("set VESDB_TOKEN")?;
    let source = std::env::var("EGM_SOURCE").unwrap_or_else(|_| "https://egm-team.local".into());
    Ok(VesdbLog::new(&url, &token, &source))
}

/// The domain events in an `--events` file, in order.
fn domain_events(path: &PathBuf) -> anyhow::Result<Vec<DomainEvent>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line).with_context(|| format!("line {}", n + 1))?;
        if v["type"] != "domain" {
            continue;
        }
        let kind = v["kind"].as_str().unwrap_or_default();
        let Some(kind) = TYPES.iter().find(|t| **t == kind) else { bail!("line {}: unknown event type {kind}", n + 1) };
        out.push(DomainEvent { kind, feature: v["feature"].as_str().unwrap_or_default().into(), data: v["data"].clone() });
    }
    Ok(out)
}

fn print_phases(by_feature: BTreeMap<String, Vec<Written>>) -> anyhow::Result<()> {
    for (f, evs) in by_feature {
        match replay::gate(&evs) {
            Ok(Some(g)) => println!("{f}\t{}\t{}", serde_json::to_value(g.phase)?.as_str().unwrap_or("?"), g.doc_hash),
            Ok(None) => {}
            Err(e) => bail!("{f}: {e}"),
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().cmd {
        Cmd::Replay { events } => {
            let mut by: BTreeMap<String, Vec<Written>> = BTreeMap::new();
            for (i, d) in domain_events(&events)?.into_iter().enumerate() {
                if d.feature.is_empty() {
                    continue;
                }
                by.entry(d.feature.clone()).or_default().push(Written { id: i as u64, subject: String::new(), kind: d.kind.into(), actor: String::new(), data: d.data, time: String::new() });
            }
            print_phases(by)
        }
        Cmd::Push { events, project, job, actor } => {
            let log = vesdb()?;
            let evs = domain_events(&events)?;
            let mut first = None;
            for (n, d) in evs.iter().enumerate() {
                let w = log.append(&key(&job, n as u64), &[AuditEvent::from_domain(&project, &actor, d)])?;
                first.get_or_insert(w.first().map(|w| w.id));
            }
            println!("wrote {} event(s) for {project}", evs.len());
            Ok(())
        }
        Cmd::ReplayVesdb { project } => {
            let log = vesdb()?;
            let prefix = format!("/egm/{project}/");
            let mut by: BTreeMap<String, Vec<Written>> = BTreeMap::new();
            for w in log.history(&project, None)? {
                if let Some(f) = w.subject.strip_prefix(&prefix) {
                    by.entry(f.to_string()).or_default().push(w);
                }
            }
            print_phases(by)
        }
        Cmd::Receipt { id } => {
            println!("{}", serde_json::to_string_pretty(&vesdb()?.receipt(id)?)?);
            Ok(())
        }
        Cmd::Bundle { ids } => {
            println!("{}", serde_json::to_string_pretty(&vesdb()?.bundle(&ids)?)?);
            Ok(())
        }
    }
}
