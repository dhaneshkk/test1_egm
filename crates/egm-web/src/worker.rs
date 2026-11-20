//! The worker: runs egm-dsh jobs that people queue in the app.
//!
//! A job is three events in Vesdb: `job.requested` (by the person, when they press a button),
//! `job.started` and `job.finished` (by the worker). The queue is the projection of those events,
//! so it survives a rebuild. Which job starts next is `egm_core::queue::pick` (proved in
//! `proofs/EgmQueue.lean`): one job at a time per project, oldest first, and none once the
//! project has reached its monthly token ceiling.
//!
//! Each job runs `egm-dsh --events <file> <command>` in the project's checkout, as its own
//! process, under egm-dsh's own sandbox for the agent. The process gets a cleared environment
//! plus an allowlist, so egm-team's secrets never reach a build. While it runs, the worker tails
//! the events file: terminal lines go to `job_log` (Postgres only: they may quote code), domain
//! events go to Vesdb with the person who queued the job as the actor.
//!
//! A per-project Postgres advisory lock, held on the job's own connection, keeps a second worker
//! process off a project that is busy; it is released when the job ends or the worker dies.

use crate::config::{Config, WorkerConfig};
use crate::db;
use egm_audit::{AuditEvent, AuditLog, Written};
use egm_core::policy::{allowed, Action, Role};
use egm_core::queue::{over_ceiling, pick, Job, JobState, Pick};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::Notify;
use tokio_postgres::Client;

/// The commands a person can queue, and who may queue them.
pub const COMMANDS: &[(&str, &str)] = &[
    ("elephant", "Start a feature: the elephant reads the idea and asks questions"),
    ("say", "Answer the elephant"),
    ("draft", "The elephant writes the design doc (or redrafts sections)"),
    ("goldfish", "Fresh reviewers test the doc"),
    ("build", "Build the goldfish-approved doc"),
    ("review", "A hostile review of the built code"),
    ("gate", "The evidence gate"),
];

pub const MAX_TEXT: usize = 4000;

/// What a job asks for, checked the same way when it is queued and when it starts.
pub fn action_for(command: &str) -> Action {
    if command == "elephant" { Action::CreateFeature } else { Action::RunJob }
}

/// The egm-dsh arguments for a job, or why the job is not valid.
/// Free text goes in as `--flag=value`, so text starting with `-` is never read as a flag.
pub fn argv(command: &str, feature: &str, args: &Value) -> Result<Vec<String>, String> {
    if !crate::config::valid_name(feature) {
        return Err("a feature name uses letters, digits, `-` and `_`".into());
    }
    let text = || -> Result<String, String> {
        let t = args["text"].as_str().unwrap_or("").trim();
        if t.is_empty() || t.len() > MAX_TEXT {
            return Err(format!("write 1 to {MAX_TEXT} characters"));
        }
        Ok(t.to_string())
    };
    let f = feature.to_string();
    Ok(match command {
        "elephant" => vec!["elephant".into(), f, format!("--idea={}", text()?)],
        "say" => vec!["elephant".into(), f, format!("--say={}", text()?)],
        "draft" => {
            let mut v = vec!["draft".into(), f];
            for s in args["sections"].as_array().into_iter().flatten() {
                let s = s.as_str().unwrap_or("");
                if !egm_core::doc::SECTIONS.contains(&s) {
                    return Err(format!("unknown section `{s}`"));
                }
                v.push(format!("--section={s}"));
            }
            v
        }
        "goldfish" | "build" | "review" | "gate" => vec![command.into(), f],
        _ => return Err(format!("unknown command `{command}`")),
    })
}

/// The environment a job gets: a fixed allowlist plus `pass_env`, nothing inherited otherwise.
pub fn job_env(w: &WorkerConfig, get: impl Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    let mut names: Vec<&str> = vec!["PATH", "HOME", "LANG", "LC_ALL", "TMPDIR", "USER"];
    names.extend(w.pass_env.iter().map(String::as_str).filter(|n| !n.starts_with("EGM_")));
    names.into_iter().filter_map(|n| get(n).map(|v| (n.to_string(), v))).collect()
}

fn job_state(s: &str) -> JobState {
    match s {
        "queued" => JobState::Queued,
        "running" => JobState::Running,
        _ => JobState::Done,
    }
}

async fn append(log: &Arc<dyn AuditLog>, key: String, ev: AuditEvent) -> anyhow::Result<Vec<Written>> {
    let log = log.clone();
    Ok(tokio::task::spawn_blocking(move || log.append(&key, &[ev])).await??)
}

/// Write one of the worker's own events and apply it to the views at once.
async fn record(st: &State, key: String, kind: &str, job: &db::JobRow, data: Value) -> anyhow::Result<()> {
    let ev = AuditEvent { kind: kind.into(), project: job.project.clone(), feature: job.feature.clone(), actor: st.actor.clone(), data };
    for w in append(&st.log, key, ev).await? {
        db::apply(&*st.db, &w).await?;
    }
    Ok(())
}

struct State {
    cfg: Config,
    w: WorkerConfig,
    log: Arc<dyn AuditLog>,
    db: Arc<Client>,
    actor: String,
    /// Jobs this process is running.
    active: Mutex<HashSet<i64>>,
    done: Notify,
}

/// Run the worker until the process ends. `wake` is notified when someone queues a job.
pub async fn run(cfg: Config, log: Arc<dyn AuditLog>, wake: Arc<Notify>) {
    let Some(w) = cfg.worker.clone() else { return };
    let mut client = loop {
        match db::connect(&cfg.database_url).await {
            Ok(c) => break c,
            Err(e) => {
                eprintln!("egm-team worker: database: {e}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    };
    if let Err(e) = db::sync(&mut client, log.clone()).await {
        eprintln!("egm-team worker: {e}");
    }
    let st = Arc::new(State {
        actor: format!("worker:{}", w.name),
        cfg,
        w,
        log,
        db: Arc::new(client),
        active: Mutex::new(HashSet::new()),
        done: Notify::new(),
    });
    if let Err(e) = recover(&st).await {
        eprintln!("egm-team worker: recovering jobs: {e}");
    }
    let mut sync_client = match db::connect(&st.cfg.database_url).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("egm-team worker: database: {e}");
            return;
        }
    };
    loop {
        if let Err(e) = db::sync(&mut sync_client, st.log.clone()).await {
            eprintln!("egm-team worker: {e}");
        }
        if let Err(e) = schedule(&st).await {
            eprintln!("egm-team worker: {e}");
        }
        tokio::select! {
            _ = wake.notified() => {}
            _ = st.done.notified() => {}
            _ = tokio::time::sleep(Duration::from_millis(1000)) => {}
        }
    }
}

/// Jobs left `running` by a worker that died: finish them as abandoned, unless another worker
/// still holds the project's lock.
async fn recover(st: &Arc<State>) -> anyhow::Result<()> {
    for job in db::open_jobs(&*st.db).await?.into_iter().filter(|j| j.state == "running") {
        let Some(lock) = try_lock(&st.cfg.database_url, &job.project).await? else { continue };
        record(st, format!("job/{}/finished", job.id), "job.finished", &job,
               json!({"job": job.id, "status": "abandoned", "detail": "the worker stopped while this job ran; run it again", "tokens": 0})).await?;
        drop(lock);
    }
    Ok(())
}

/// A connection holding the project's advisory lock, or `None` when another holds it.
async fn try_lock(url: &str, project: &str) -> anyhow::Result<Option<Client>> {
    let c = db::connect(url).await?;
    let got: bool = c.query_one("SELECT pg_try_advisory_lock(hashtext('egm-job:' || $1))", &[&project]).await?.get(0);
    Ok(got.then_some(c))
}

/// Start (or refuse) every job the rule allows, up to `max_parallel`.
async fn schedule(st: &Arc<State>) -> anyhow::Result<()> {
    let open = db::open_jobs(&*st.db).await?;
    let mut states: HashMap<i64, JobState> = open.iter().map(|j| (j.id, job_state(&j.state))).collect();
    let mut spent: HashMap<String, i64> = HashMap::new();
    for j in &open {
        if !spent.contains_key(&j.project) {
            spent.insert(j.project.clone(), db::month_tokens(&*st.db, &j.project).await?);
        }
    }
    loop {
        if st.active.lock().unwrap().len() >= st.w.max_parallel {
            return Ok(());
        }
        let jobs: Vec<Job> = open.iter().map(|j| Job { id: j.id, project: &j.project, state: states[&j.id] }).collect();
        let over = |p: &str| {
            let ceiling = st.cfg.project(p).and_then(|c| c.monthly_tokens);
            over_ceiling(spent.get(p).copied().unwrap_or(0).max(0) as u64, ceiling)
        };
        let Some(next) = pick(&jobs, over) else { return Ok(()) };
        let id = match next {
            Pick::Start(id) | Pick::Refuse(id) => id,
        };
        let job = open.iter().find(|j| j.id == id).cloned().expect("picked from the list");
        match next {
            Pick::Refuse(_) => {
                states.insert(id, JobState::Done);
                let ceiling = st.cfg.project(&job.project).and_then(|c| c.monthly_tokens).unwrap_or(0);
                let used = spent.get(&job.project).copied().unwrap_or(0);
                record(st, format!("job/{id}/finished"), "job.finished", &job, json!({"job": id, "status": "refused", "tokens": 0,
                    "detail": format!("the project has used {used} tokens this month, at or over its ceiling of {ceiling}")})).await?;
            }
            Pick::Start(_) => {
                states.insert(id, JobState::Running);
                if let Some(why) = refusal(st, &job).await? {
                    states.insert(id, JobState::Done);
                    record(st, format!("job/{id}/finished"), "job.finished", &job, json!({"job": id, "status": "refused", "tokens": 0, "detail": why})).await?;
                    continue;
                }
                // a second worker process may be running this project: leave it alone
                let Some(lock) = try_lock(&st.cfg.database_url, &job.project).await? else { continue };
                record(st, format!("job/{id}/started"), "job.started", &job, json!({"job": id, "worker": st.w.name})).await?;
                st.active.lock().unwrap().insert(id);
                let st2 = st.clone();
                tokio::spawn(async move {
                    let out = execute(&st2, &job).await;
                    let data = match out {
                        Ok(d) => d,
                        Err(e) => json!({"job": job.id, "status": "error", "detail": e.to_string(), "tokens": 0}),
                    };
                    if let Err(e) = record(&st2, format!("job/{}/finished", job.id), "job.finished", &job, data).await {
                        eprintln!("egm-team worker: job {}: {e}", job.id);
                    }
                    drop(lock);
                    st2.active.lock().unwrap().remove(&job.id);
                    st2.done.notify_one();
                });
            }
        }
    }
}

/// Checked again when the job starts: the project still exists, the command is valid, and the
/// person who queued it still holds a role that allows it.
async fn refusal(st: &State, job: &db::JobRow) -> anyhow::Result<Option<String>> {
    if st.cfg.project(&job.project).is_none() {
        return Ok(Some("the project is no longer configured".into()));
    }
    if let Err(e) = argv(&job.command, &job.feature, &job.args) {
        return Ok(Some(e));
    }
    let roles: Vec<Role> = db::roles(&*st.db, &job.project, &job.requester)
        .await?
        .iter()
        .filter_map(|n| serde_json::from_value(json!(n)).ok())
        .collect();
    if let Err(d) = allowed(&roles, false, egm_core::evidence::Tier::Low, action_for(&job.command)) {
        return Ok(Some(format!("the person who queued it may no longer: {d}")));
    }
    Ok(None)
}

fn events_path(id: i64) -> PathBuf {
    std::env::temp_dir().join("egm-team-jobs").join(format!("{id}.jsonl"))
}

/// Run one job to the end; returns the data of its `job.finished` event.
async fn execute(st: &Arc<State>, job: &db::JobRow) -> anyhow::Result<Value> {
    let checkout = st.cfg.project(&job.project).map(|p| p.checkout.clone()).ok_or_else(|| anyhow::anyhow!("no project"))?;
    let args = argv(&job.command, &job.feature, &job.args).map_err(|e| anyhow::anyhow!(e))?;
    let path = events_path(job.id);
    std::fs::create_dir_all(path.parent().unwrap())?;
    let _ = std::fs::remove_file(&path);
    std::fs::write(&path, "")?;
    let mut cmd = tokio::process::Command::new(&st.w.egm_dsh);
    cmd.arg("--events").arg(&path).args(&args).current_dir(&checkout).env_clear()
        .envs(job_env(&st.w, |n| std::env::var(n).ok()))
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| anyhow::anyhow!("cannot start {}: {e}", st.w.egm_dsh.display()))?;
    let mut stderr = child.stderr.take().expect("piped");
    let err_task = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });
    let mut tail = Tail { path: path.clone(), offset: 0, seq: 0, n: 0, tokens: 0, partial: String::new() };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(st.w.timeout_s);
    let status = loop {
        tail.read(st, job).await?;
        tokio::select! {
            r = child.wait() => break Some(r?),
            _ = tokio::time::sleep(Duration::from_millis(200)) => {}
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = child.kill().await;
            break None;
        }
    };
    tail.read(st, job).await?;
    let err = err_task.await.unwrap_or_default();
    let err = err.trim();
    if !err.is_empty() {
        let keep: String = err.chars().rev().take(4000).collect::<Vec<_>>().into_iter().rev().collect();
        tail.seq += 1;
        db::job_log_append(&*st.db, job.id, tail.seq, "error", &keep).await?;
    }
    let _ = std::fs::remove_file(&path);
    Ok(match status {
        None => json!({"job": job.id, "status": "timeout", "tokens": tail.tokens, "detail": format!("stopped after {} s", st.w.timeout_s)}),
        Some(s) => {
            let code = s.code();
            let ok = s.success();
            json!({"job": job.id, "status": if ok { "ok" } else { "failed" }, "exit_code": code, "tokens": tail.tokens,
                   "detail": if ok { String::new() } else { last_line(err) }})
        }
    })
}

fn last_line(s: &str) -> String {
    s.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(300).collect()
}

/// Reads what egm-dsh appended to its events file since the last read.
struct Tail {
    path: PathBuf,
    offset: u64,
    /// Lines written to `job_log`.
    seq: i32,
    /// Domain events written to Vesdb.
    n: u64,
    tokens: i64,
    partial: String,
}

impl Tail {
    async fn read(&mut self, st: &State, job: &db::JobRow) -> anyhow::Result<()> {
        let chunk = read_from(&self.path, self.offset)?;
        if chunk.is_empty() {
            return Ok(());
        }
        self.offset += chunk.len() as u64;
        self.partial.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(i) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=i).collect();
            let Ok(ev) = serde_json::from_str::<Value>(line.trim()) else { continue };
            match ev["type"].as_str() {
                Some("line") => {
                    self.seq += 1;
                    db::job_log_append(&*st.db, job.id, self.seq, ev["speaker"].as_str().unwrap_or("out"), ev["text"].as_str().unwrap_or("")).await?;
                }
                Some("domain") => {
                    let kind = ev["kind"].as_str().unwrap_or("").to_string();
                    if !egm_core::events::TYPES.contains(&kind.as_str()) {
                        continue;
                    }
                    if kind == "usage.recorded" {
                        self.tokens += ev["data"]["tokens"].as_i64().unwrap_or(0);
                    }
                    let feature = ev["feature"].as_str().unwrap_or("").to_string();
                    let a = AuditEvent { kind, project: job.project.clone(), feature, actor: job.requester.clone(), data: ev["data"].clone() };
                    self.n += 1;
                    for w in append(&st.log, format!("job/{}/ev/{}", job.id, self.n), a).await? {
                        db::apply(&*st.db, &w).await?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn read_from(path: &Path, offset: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![];
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(pass: &[&str]) -> WorkerConfig {
        WorkerConfig { egm_dsh: "egm-dsh".into(), pass_env: pass.iter().map(|s| s.to_string()).collect(), timeout_s: 1, max_parallel: 1, name: "w".into() }
    }

    #[test]
    fn a_job_gets_only_the_allowlist() {
        let env: HashMap<&str, &str> = [("PATH", "/bin"), ("HOME", "/h"), ("EGM_VESDB_TOKEN", "secret"), ("EGM_SMTP_PASSWORD", "pw"),
            ("DEEPSEEK_API_KEY", "k"), ("AWS_SECRET_ACCESS_KEY", "aws")].into_iter().collect();
        let got = job_env(&w(&["DEEPSEEK_API_KEY", "EGM_VESDB_TOKEN"]), |n| env.get(n).map(|v| v.to_string()));
        let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["PATH", "HOME", "DEEPSEEK_API_KEY"]);
    }

    #[test]
    fn arguments_are_checked_and_text_cannot_become_a_flag() {
        assert_eq!(argv("say", "f", &json!({"text": "--force yes"})).unwrap(), ["elephant", "f", "--say=--force yes"]);
        assert_eq!(argv("elephant", "f", &json!({"text": " idea "})).unwrap(), ["elephant", "f", "--idea=idea"]);
        assert_eq!(argv("draft", "f", &json!({"sections": ["Technical plan"]})).unwrap(), ["draft", "f", "--section=Technical plan"]);
        assert_eq!(argv("gate", "f", &json!({})).unwrap(), ["gate", "f"]);
        assert!(argv("draft", "f", &json!({"sections": ["Nope"]})).is_err());
        assert!(argv("say", "f", &json!({"text": "  "})).is_err());
        assert!(argv("say", "f", &json!({"text": "x".repeat(MAX_TEXT + 1)})).is_err());
        assert!(argv("rm", "f", &json!({})).is_err());
        assert!(argv("gate", "../x", &json!({})).is_err());
        assert!(argv("gate", "a b", &json!({})).is_err());
    }

    #[test]
    fn who_may_queue_what() {
        assert_eq!(action_for("elephant"), Action::CreateFeature);
        for (c, _) in COMMANDS.iter().filter(|(c, _)| *c != "elephant") {
            assert_eq!(action_for(c), Action::RunJob);
        }
    }
}
