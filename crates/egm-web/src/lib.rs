//! egm-team: the web app over egm-dsh, so developers and reviewers work on features together.
//!
//! Phase 2 is the read-only team view; phase 3 adds collaboration ([`collab`]): alarm labels,
//! comments, reviews, merge approval with separation of duties, and mail ([`mail`]); phase 4
//! runs egm-dsh from the browser ([`jobs`]) through a worker with a durable queue ([`worker`]). The app reads two things: Postgres views rebuilt from
//! the Vesdb audit log ([`db`]), and the project checkout egm-dsh works in ([`checkout`]).
//! People sign in with GitHub or GitLab ([`oauth`]); every page checks the policy first
//! ([`authz`]).

pub mod app;
pub mod authz;
pub mod checkout;
pub mod collab;
pub mod config;
pub mod db;
pub mod jobs;
pub mod mail;
pub mod oauth;
pub mod worker;

use std::sync::Arc;

/// Turn an `anyhow` error into a Topcoat one inside handlers: `db::features(..).await.ah()?`.
pub trait Ah<T> {
    fn ah(self) -> topcoat::Result<T>;
}

impl<T> Ah<T> for anyhow::Result<T> {
    fn ah(self) -> topcoat::Result<T> {
        self.map_err(topcoat::Error::from_anyhow)
    }
}

/// The pseudo-project for events that belong to no project (sign-ins).
pub const SYSTEM: &str = "_system";

pub struct App {
    pub cfg: config::Config,
    pub db: Arc<tokio_postgres::Client>,
    pub log: Arc<dyn egm_audit::AuditLog>,
    /// Wake the projector now (after a sign-in, so the new user's name shows at once).
    pub refresh: Arc<tokio::sync::Notify>,
    /// Wake the worker now (after someone queues a job), when it runs in this process.
    pub work: Arc<tokio::sync::Notify>,
}

/// Keep the views current: read new events from Vesdb every `refresh_ms`, or when woken.
pub async fn projector(cfg: config::Config, log: Arc<dyn egm_audit::AuditLog>, wake: Arc<tokio::sync::Notify>) {
    let mut client = loop {
        match db::connect(&cfg.database_url).await {
            Ok(c) => break c,
            Err(e) => {
                eprintln!("egm-team projector: database: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    };
    loop {
        if let Err(e) = db::sync(&mut client, log.clone()).await {
            eprintln!("egm-team projector: {e}");
        }
        tokio::select! {
            _ = wake.notified() => {}
            _ = tokio::time::sleep(std::time::Duration::from_millis(cfg.refresh_ms)) => {}
        }
    }
}
