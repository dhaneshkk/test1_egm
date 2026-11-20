//! egm-team's configuration: one TOML file, with secrets from the environment only.
//!
//! ```toml
//! listen = "127.0.0.1:8080"
//! public_url = "https://egm-team.example"      # OAuth redirects come back here
//! database_url = "host=/run/postgresql dbname=egm"
//! vesdb_url = "http://vesdb-a:3000,http://vesdb-b:3000"
//! admins = ["github:1234"]                      # may grant roles before anyone else can
//!
//! [[project]]
//! name = "jot"
//! checkout = "/srv/egm/jot"                    # read-only: docs, reviews, evidence, git history
//! host = "github"                               # or "gitlab"
//!
//! [github]
//! client_id = "Iv1.abc"                          # secret: EGM_GITHUB_CLIENT_SECRET
//! [gitlab]
//! client_id = "abc"                              # secret: EGM_GITLAB_CLIENT_SECRET
//! base_url = "https://gitlab.com"                # or your own GitLab
//! ```
//!
//! Environment: `EGM_VESDB_TOKEN` (write and audit scopes), `EGM_GITHUB_CLIENT_SECRET`,
//! `EGM_GITLAB_CLIENT_SECRET`, `EGM_SESSION_KEY` is not needed (session tokens are random and
//! stored hashed).

use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_listen")]
    pub listen: String,
    pub public_url: String,
    pub database_url: String,
    pub vesdb_url: String,
    /// PEM file of the CA to trust when `vesdb_url` is `https://` (Vesdb behind a TLS proxy
    /// with a private CA). Without it, the usual public roots.
    pub vesdb_ca: Option<PathBuf>,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default)]
    pub admins: Vec<String>,
    #[serde(default, rename = "project")]
    pub projects: Vec<ProjectConfig>,
    pub github: Option<OAuthApp>,
    pub gitlab: Option<OAuthApp>,
    /// Mail notifications (review requests, reviews, approvals, comments). Off when absent.
    pub mail: Option<MailConfig>,
    /// The worker that runs egm-dsh jobs. `serve` runs it in the same process when this is
    /// present; `egm-team worker` runs it alone.
    pub worker: Option<WorkerConfig>,
    /// How often the projector reads new events from Vesdb, in milliseconds.
    #[serde(default = "default_refresh")]
    pub refresh_ms: u64,
    #[serde(skip)]
    pub vesdb_token: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub name: String,
    pub checkout: PathBuf,
    #[serde(default = "default_host")]
    pub host: String,
    /// Most tokens the project's jobs may use in a calendar month (UTC). A job is refused once
    /// the month's use has reached it; the job that crosses it is not stopped.
    pub monthly_tokens: Option<u64>,
    /// GitHub `owner/repo` or GitLab project path. Turns on the code-host integration: commit
    /// statuses, a pull or merge request per feature, branch pushes and webhooks. The token comes
    /// from `EGM_HOST_TOKEN_<NAME>` and the webhook secret from `EGM_WEBHOOK_SECRET_<NAME>`
    /// (`<NAME>`: the project name in capitals, `-` as `_`).
    pub repo: Option<String>,
    /// Where the worker pushes `egm-dsh/<feature>` branches; by default the host's HTTPS URL.
    pub push_url: Option<String>,
    /// The branch pull and merge requests target.
    #[serde(default = "default_base_branch")]
    pub base_branch: String,
    /// Days to keep job output and other Postgres-only data, and after which `egm-team redact`
    /// erases people's words from Vesdb. Off when absent.
    pub retention_days: Option<u32>,
    #[serde(skip)]
    pub host_token: String,
    #[serde(skip)]
    pub webhook_secret: String,
}

fn default_base_branch() -> String {
    "main".into()
}

impl ProjectConfig {
    /// `EGM_HOST_TOKEN_<NAME>` and `EGM_WEBHOOK_SECRET_<NAME>` for this project.
    pub fn env_name(&self, prefix: &str) -> String {
        format!("{prefix}{}", self.name.to_uppercase().replace('-', "_"))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerConfig {
    /// The egm-dsh binary.
    #[serde(default = "default_egm_dsh")]
    pub egm_dsh: PathBuf,
    /// Environment variables passed to egm-dsh, beyond PATH, HOME, LANG, LC_ALL, TMPDIR and USER
    /// (for example DEEPSEEK_API_KEY). Nothing else reaches a job: not the Vesdb token, the
    /// OAuth secrets or the SMTP password.
    #[serde(default)]
    pub pass_env: Vec<String>,
    /// A job still running after this many seconds is stopped.
    #[serde(default = "default_timeout")]
    pub timeout_s: u64,
    /// Jobs running at once across projects (always one per project).
    #[serde(default = "default_parallel")]
    pub max_parallel: usize,
    /// Recorded as the actor of the worker's own events (`worker:<name>`).
    #[serde(default = "default_worker_name")]
    pub name: String,
}

fn default_egm_dsh() -> PathBuf {
    "egm-dsh".into()
}
fn default_timeout() -> u64 {
    1800
}
fn default_parallel() -> usize {
    2
}
fn default_worker_name() -> String {
    "worker".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthApp {
    pub client_id: String,
    /// GitHub: `https://github.com` (API at `https://api.github.com`); GitLab: the instance URL.
    pub base_url: Option<String>,
    /// GitHub only: the REST API base, when not `https://api.github.com`.
    pub api_url: Option<String>,
    #[serde(skip)]
    pub client_secret: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailConfig {
    pub host: String,
    #[serde(default = "default_smtp_port")]
    pub port: u16,
    /// `starttls` (default), `tls`, or `none` (a local relay only).
    #[serde(default = "default_tls")]
    pub tls: String,
    pub from: String,
    /// With a username, the password comes from `EGM_SMTP_PASSWORD`.
    pub username: Option<String>,
    #[serde(skip)]
    pub password: String,
}

fn default_smtp_port() -> u16 {
    587
}
fn default_tls() -> String {
    "starttls".into()
}

fn default_listen() -> String {
    "127.0.0.1:8080".into()
}
fn default_source() -> String {
    "https://egm-team.local".into()
}
fn default_host() -> String {
    "github".into()
}
fn default_refresh() -> u64 {
    1000
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Config> {
        let text = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
        let mut c: Config = toml::from_str(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        c.vesdb_token = std::env::var("EGM_VESDB_TOKEN").map_err(|_| anyhow::anyhow!("set EGM_VESDB_TOKEN (Vesdb token with write and audit scopes)"))?;
        if let Some(g) = &mut c.github {
            g.client_secret = std::env::var("EGM_GITHUB_CLIENT_SECRET").map_err(|_| anyhow::anyhow!("set EGM_GITHUB_CLIENT_SECRET"))?;
        }
        if let Some(g) = &mut c.gitlab {
            g.client_secret = std::env::var("EGM_GITLAB_CLIENT_SECRET").map_err(|_| anyhow::anyhow!("set EGM_GITLAB_CLIENT_SECRET"))?;
        }
        anyhow::ensure!(c.github.is_some() || c.gitlab.is_some(), "configure [github] or [gitlab] sign-in");
        if let Some(m) = &mut c.mail {
            anyhow::ensure!(["starttls", "tls", "none"].contains(&m.tls.as_str()), "[mail] tls must be starttls, tls or none");
            if m.username.is_some() {
                m.password = std::env::var("EGM_SMTP_PASSWORD").map_err(|_| anyhow::anyhow!("set EGM_SMTP_PASSWORD"))?;
            }
        }
        for p in &mut c.projects {
            anyhow::ensure!(valid_name(&p.name) && p.name != crate::SYSTEM, "project name `{}`: use letters, digits, `-` and `_`", p.name);
            anyhow::ensure!(p.host == "github" || p.host == "gitlab", "project `{}`: host must be github or gitlab", p.name);
            if let Some(repo) = &p.repo {
                anyhow::ensure!(
                    !repo.is_empty() && repo.split('/').all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))),
                    "project `{}`: repo must look like owner/name", p.name
                );
                let (t, s) = (p.env_name("EGM_HOST_TOKEN_"), p.env_name("EGM_WEBHOOK_SECRET_"));
                p.host_token = std::env::var(&t).map_err(|_| anyhow::anyhow!("project `{}` has a repo: set {t}", p.name))?;
                p.webhook_secret = std::env::var(&s).map_err(|_| anyhow::anyhow!("project `{}` has a repo: set {s}", p.name))?;
                anyhow::ensure!(p.webhook_secret.len() >= 20, "{s} must have at least 20 characters");
                anyhow::ensure!(if p.host == "github" { c.github.is_some() } else { c.gitlab.is_some() },
                    "project `{}`: configure [{}] (its API address)", p.name, p.host);
            }
            anyhow::ensure!(valid_name(&p.base_branch.replace('/', "_")), "project `{}`: base_branch", p.name);
        }
        if let Some(w) = &c.worker {
            for v in &w.pass_env {
                anyhow::ensure!(!v.starts_with("EGM_"), "[worker] pass_env: `{v}` is one of egm-team's own secrets and never reaches a job");
            }
            anyhow::ensure!(w.max_parallel >= 1 && w.timeout_s >= 1, "[worker] max_parallel and timeout_s must be at least 1");
            anyhow::ensure!(valid_name(&w.name), "[worker] name: use letters, digits, `-` and `_`");
        }
        c.public_url = c.public_url.trim_end_matches('/').to_string();
        Ok(c)
    }

    pub fn project(&self, name: &str) -> Option<&ProjectConfig> {
        self.projects.iter().find(|p| p.name == name)
    }
}

/// Project names, feature slugs and user ids appear in paths and subjects: keep them plain.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_name("jot") && valid_name("gap-ids") && valid_name("a_b1"));
        assert!(!valid_name("") && !valid_name("../x") && !valid_name("a/b") && !valid_name("a b"));
    }
}
