//! Budgets and agent sessions at run time.
//!
//! Every command's model work is an *activity* (`begin`), and every agent
//! session inside it is opened through `open`. Both go through
//! `RunState::step` (`run.rs`), the mirror of `rstep` in proofs/EgmGate.lean:
//! an over-budget activity or session is refused with a message naming the
//! limit, and an activity can only finish once all its sessions are closed.
//!
//! Sessions are closed whatever happens:
//! * `SessionGuard` closes on drop: normal return, `?` errors, and panics
//!   (unwinding runs drop). If the agent's process group is still alive, it
//!   is killed first.
//! * Ctrl-C, SIGTERM and SIGHUP: a handler kills every registered process
//!   group, empties the registry and exits 130.
//! * egm-dsh itself killed hard (SIGKILL, crash): dsh children are started
//!   with `PR_SET_PDEATHSIG` (Linux), so the kernel kills them with egm; the
//!   registry in `.egm-dsh/sessions.json` survives, and the next egm-dsh
//!   command kills any process group still alive from it and clears it.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, Once};

use serde::{Deserialize, Serialize};

use crate::agent::{Agent, Reply, Turn};
use crate::phase::{Event, Gate};
use crate::run::{raise_hint, Kind, Limits, RunEvent, RunRefusal, RunState, Spent};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub id: u64,
    pub feature: String,
    pub kind: Kind,
    pub role: String,
    /// The agent process (a dsh run); its process group has the same id.
    #[serde(default)]
    pub pid: Option<u32>,
    /// Process start time (clock ticks since boot), to tell a reused pid apart.
    #[serde(default)]
    pub start_ticks: Option<u64>,
    pub opened_secs: u64,
}

#[derive(Default)]
struct Registry {
    path: Option<PathBuf>,
    entries: Vec<Entry>,
}

struct Active {
    slug: String,
    state: RunState,
    spent: Spent,
    spent_path: Option<PathBuf>,
    limits: Limits,
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry { path: None, entries: Vec::new() });
static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);
static HANDLER: Once = Once::new();
/// Set by the Ctrl-C/SIGTERM/SIGHUP handler before it starts closing sessions.
static INTERRUPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Called by the main thread after an agent call and before exiting: once an interrupt
/// has begun, do nothing more and let the handler finish (it exits with 130). If it has
/// not exited within a few seconds, exit with 130 here.
pub fn halt_if_interrupted() {
    if INTERRUPTED.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_secs(5));
        std::process::exit(130);
    }
}

thread_local! {
    /// The session the current thread's agent runs in, so `attach_pid` can find it.
    static CURRENT: Cell<Option<u64>> = const { Cell::new(None) };
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn persist(reg: &Registry) {
    // write-then-rename: a reader (or a crash) never sees a half-written registry
    if let Some(p) = &reg.path {
        let tmp = p.with_extension(format!("json.{}.tmp", std::process::id()));
        let text = serde_json::to_string_pretty(&reg.entries).unwrap_or_else(|_| "[]".into()) + "\n";
        if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, p).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

// ---------------------------------------------------------------------------------------
// processes

/// Start time of a process in clock ticks since boot (field 22 of /proc/<pid>/stat).
pub fn start_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = &stat[stat.rfind(')')? + 2..];
    after.split_whitespace().nth(19)?.parse().ok()
}

fn leader_alive(pid: u32, ticks: Option<u64>) -> bool {
    match (start_ticks(pid), ticks) {
        (Some(now), Some(then)) => now == then,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// Does the process group still have a running member? Zombies (exited, waiting to be
/// reaped) do not count: they hold no resources and run no code. Linux scans /proc;
/// elsewhere signal 0 is the best available answer.
fn group_alive(pgid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        if let Ok(dir) = std::fs::read_dir("/proc") {
            return dir.flatten().any(|d| {
                let Some(pid) = d.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { return false };
                let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { return false };
                let Some(i) = stat.rfind(')') else { return false };
                let mut f = stat[i + 2..].split_whitespace();
                let state = f.next().unwrap_or("Z");
                let pgrp = f.nth(1).and_then(|x| x.parse::<u32>().ok());
                pgrp == Some(pgid) && state != "Z" && state != "X"
            });
        }
    }
    unsafe { libc::kill(-(pgid as libc::pid_t), 0) == 0 }
}

/// Reap any of our own children in this group that have exited, so none is left a zombie.
fn reap_group(pgid: u32) {
    loop {
        let r = unsafe { libc::waitpid(-(pgid as libc::pid_t), std::ptr::null_mut(), libc::WNOHANG) };
        if r <= 0 {
            break;
        }
    }
}

/// Kill an agent's whole process group: SIGTERM, a short grace period, SIGKILL.
fn kill_group(pgid: u32) {
    if pgid <= 1 {
        return;
    }
    unsafe { libc::kill(-(pgid as libc::pid_t), libc::SIGTERM) };
    for _ in 0..10 {
        reap_group(pgid);
        if !group_alive(pgid) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    unsafe { libc::kill(-(pgid as libc::pid_t), libc::SIGKILL) };
    // SIGKILL cannot be ignored; wait (bounded) until the kernel has finished the group
    for _ in 0..100 {
        reap_group(pgid);
        if !group_alive(pgid) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Should this entry's process group be killed? Its leader is still the process we
/// started (same start time), or the leader is gone but processes it left in its
/// group (tool subprocesses) are still running.
fn needs_kill(e: &Entry) -> bool {
    match e.pid {
        Some(pid) => leader_alive(pid, e.start_ticks) || (start_ticks(pid).is_none() && group_alive(pid)),
        None => false,
    }
}

/// Prepare a dsh child: its own process group (so it and its tools can be killed
/// together), and on Linux the kernel kills it if egm-dsh dies.
pub fn prepare_child(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
    #[cfg(target_os = "linux")]
    unsafe {
        cmd.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Record the process of the session this thread is running.
pub fn attach_pid(pid: u32) {
    let Some(id) = CURRENT.with(|c| c.get()) else { return };
    let mut reg = lock(&REGISTRY);
    if let Some(e) = reg.entries.iter_mut().find(|e| e.id == id) {
        e.pid = Some(pid);
        e.start_ticks = start_ticks(pid);
    }
    persist(&reg);
}

// ---------------------------------------------------------------------------------------
// startup: registry path, crash recovery, signal handling

/// Call once a project is open (after its lock is taken). Cleans up sessions a
/// crashed earlier run left behind, and installs the Ctrl-C/SIGTERM/SIGHUP handler.
pub fn init(egm_dir: &Path) -> usize {
    let path = egm_dir.join("sessions.json");
    let leftover: Vec<Entry> = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let mut killed = 0;
    for e in &leftover {
        if needs_kill(e) {
            kill_group(e.pid.unwrap_or(0));
            killed += 1;
        }
    }
    if !leftover.is_empty() {
        crate::ui::warn(&format!(
            "cleaned up {} session(s) left by an earlier egm-dsh run that did not exit cleanly ({} process group(s) were still running)",
            leftover.len(),
            killed
        ));
    }
    crate::stage::sweep_stale();
    {
        let mut reg = lock(&REGISTRY);
        reg.path = Some(path);
        reg.entries.clear();
        persist(&reg);
    }
    HANDLER.call_once(|| {
        let _ = ctrlc::set_handler(|| {
            // First, so the main thread stops as soon as the killed agent call returns
            // instead of carrying on (saving a review, exiting 0) while we clean up.
            INTERRUPTED.store(true, std::sync::atomic::Ordering::SeqCst);
            let n = kill_all();
            crate::stage::remove_all();
            eprintln!("\negm-dsh: interrupted; closed {n} agent session(s)");
            std::process::exit(130);
        });
    });
    leftover.len()
}

/// Kill every registered session's process group and empty the registry.
pub fn kill_all() -> usize {
    let mut reg = lock(&REGISTRY);
    let n = reg.entries.len();
    for e in &reg.entries {
        if let Some(pid) = e.pid
            && needs_kill(e)
        {
            kill_group(pid);
        }
    }
    reg.entries.clear();
    persist(&reg);
    n
}

/// Live sessions right now (for tests and `doctor`).
pub fn live() -> Vec<Entry> {
    lock(&REGISTRY).entries.clone()
}

// ---------------------------------------------------------------------------------------
// activities

fn spent_path(egm_dir: &Path, slug: &str) -> PathBuf {
    egm_dir.join("spent").join(format!("{slug}.json"))
}

pub fn load_spent(egm_dir: &Path, slug: &str) -> Spent {
    std::fs::read_to_string(spent_path(egm_dir, slug)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_spent(path: &Option<PathBuf>, spent: &Spent) {
    if let Some(p) = path {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        // atomic, like the registry: a crash mid-write must not reset what was spent
        let tmp = p.with_extension(format!("json.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, serde_json::to_string_pretty(spent).unwrap_or_default() + "\n").is_ok()
            && std::fs::rename(&tmp, p).is_err()
        {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

fn refusal_message(slug: &str, r: &RunRefusal, kind: Option<Kind>, limits: &Limits, spent: &Spent) -> String {
    match r {
        RunRefusal::BudgetSpent(k) => {
            let (used, limit) = match k.counter() {
                crate::run::Counter::Goldfish => (spent.goldfish_rounds, limits.goldfish_rounds),
                crate::run::Counter::Redraft => (spent.redraft_turns, limits.redraft_turns),
                crate::run::Counter::Build => (spent.build_attempts, limits.build_attempts),
                crate::run::Counter::Review => (spent.reviews, limits.reviews),
            };
            format!(
                "refused: `{slug}` has used {used} of {limit} {}s allowed. Raise it with `egm-dsh set-budget {} {}` (all features) or add `--feature {slug}`",
                k.noun().split(" (").next().unwrap_or(""),
                raise_hint(Some(*k)),
                limit + limit.max(2) / 2
            )
        }
        RunRefusal::CallsSpent => format!(
            "refused: `{slug}` has used {} of {} model calls allowed (each agent session is one). Raise it with `egm-dsh set-budget --model-calls {}` (all features) or add `--feature {slug}`",
            spent.model_calls,
            limits.model_calls,
            limits.model_calls + limits.model_calls.max(2) / 2
        ),
        other => format!("refused: {other}{}", kind.map(|k| format!(" ({})", k.noun())).unwrap_or_default()),
    }
}

/// One command's model work. Ends with `finish`; dropped without it (error,
/// panic), it closes any session still open and cancels.
#[must_use]
pub struct Activity {
    done: bool,
}

/// Begin an activity for `slug` with the feature's current gate. Refused, with a
/// message naming the limit, when its budget is used up.
pub fn begin(egm_dir: Option<&Path>, slug: &str, gate: Gate, limits: Limits, kind: Kind) -> anyhow::Result<Activity> {
    let mut active = lock(&ACTIVE);
    if let Some(a) = active.as_ref() {
        anyhow::bail!("internal: activity for `{}` still open", a.slug);
    }
    let spent_path = egm_dir.map(|d| spent_path(d, slug));
    let mut spent = egm_dir.map(|d| load_spent(d, slug)).unwrap_or_default();
    let start = RunState::new(gate, spent.left(&limits));
    let state = start
        .step(RunEvent::Begin(kind))
        .map_err(|r| anyhow::anyhow!(refusal_message(slug, &r, Some(kind), &limits, &spent)))?;
    assert!(state.rank() < start.rank(), "run rank must drop on begin (rank_decreases)");
    spent.charge(kind.counter());
    save_spent(&spent_path, &spent);
    *active = Some(Active { slug: slug.to_string(), state, spent, spent_path, limits });
    Ok(Activity { done: false })
}

impl Activity {
    /// End the activity, applying gate event `o`. Refused while any session is open.
    /// Returns the gate after the event (the gate's own `step`, via `rstep`).
    pub fn finish(mut self, o: Option<Event>) -> anyhow::Result<Gate> {
        let mut active = lock(&ACTIVE);
        let a = active.as_mut().ok_or_else(|| anyhow::anyhow!("internal: no activity"))?;
        let next = a.state.step(RunEvent::Finish(o)).map_err(|r| anyhow::anyhow!("internal: {r}"))?;
        assert!(next.invariant() && next.rank() < a.state.rank(), "finish must keep the gate invariant and drop the rank");
        let gate = next.gate;
        if let Some(ev) = o
            && gate != a.state.gate
        {
            crate::events::gate_event(&a.slug, ev, &gate);
        }
        *active = None;
        self.done = true;
        Ok(gate)
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        // Close anything still open (a guard leaked by a thread that never returned), then cancel.
        let ids: Vec<u64> = lock(&ACTIVE).as_ref().map(|a| a.state.live.clone()).unwrap_or_default();
        for id in ids {
            close(id);
        }
        let mut active = lock(&ACTIVE);
        if let Some(a) = active.as_mut() {
            let _ = a.state.step(RunEvent::Cancel).map(|s| a.state = s);
        }
        *active = None;
    }
}

// ---------------------------------------------------------------------------------------
// sessions

/// An open agent session; closing it is guaranteed by `Drop`.
#[must_use]
pub struct SessionGuard {
    id: u64,
}

/// Open an agent session in the current activity. Spends one model call;
/// refused when none are left, or when no activity is in progress.
pub fn open(role: &str) -> anyhow::Result<SessionGuard> {
    let mut active = lock(&ACTIVE);
    let a = active.as_mut().ok_or_else(|| anyhow::anyhow!("internal: an agent session was started outside an activity"))?;
    let next = a.state.step(RunEvent::OpenSession).map_err(|r| anyhow::anyhow!(refusal_message(&a.slug, &r, a.state.active, &a.limits, &a.spent)))?;
    assert!(next.rank() < a.state.rank(), "run rank must drop on open (rank_decreases)");
    a.state = next;
    a.spent.model_calls += 1;
    save_spent(&a.spent_path, &a.spent);
    let id = a.state.live[0];
    let entry = Entry {
        id,
        feature: a.slug.clone(),
        kind: a.state.active.expect("open only inside an activity"),
        role: role.to_string(),
        pid: None,
        start_ticks: None,
        opened_secs: now_secs(),
    };
    drop(active);
    {
        let mut reg = lock(&REGISTRY);
        reg.entries.push(entry);
        persist(&reg);
    }
    CURRENT.with(|c| c.set(Some(id)));
    Ok(SessionGuard { id })
}

fn close(id: u64) {
    let entry = {
        let mut reg = lock(&REGISTRY);
        let pos = reg.entries.iter().position(|e| e.id == id);
        let e = pos.map(|i| reg.entries.remove(i));
        persist(&reg);
        e
    };
    if let Some(e) = entry
        && let Some(pid) = e.pid
        && needs_kill(&e)
    {
        kill_group(pid);
    }
    let mut active = lock(&ACTIVE);
    if let Some(a) = active.as_mut()
        && let Ok(s) = a.state.step(RunEvent::Close(id))
    {
        a.state = s;
    }
    CURRENT.with(|c| {
        if c.get() == Some(id) {
            c.set(None)
        }
    });
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        close(self.id);
    }
}

// ---------------------------------------------------------------------------------------
// every agent egm-dsh builds goes through this wrapper

/// Wraps an agent so every `run` is an open/close-guarded session.
pub struct Tracked {
    pub inner: Box<dyn Agent>,
    pub role: String,
}

impl Agent for Tracked {
    fn name(&self) -> String {
        self.inner.name()
    }
    fn model(&self) -> String {
        self.inner.model()
    }
    fn effort(&self) -> String {
        self.inner.effort()
    }
    fn reads_files(&self) -> bool {
        self.inner.reads_files()
    }
    fn can_write(&self) -> bool {
        self.inner.can_write()
    }
    fn run(&self, turn: Turn) -> anyhow::Result<Reply> {
        let _session = open(&self.role)?;
        let reply = self.inner.run(turn);
        halt_if_interrupted();   // an interrupt killed this call: stop here, report nothing
        reply
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::Budget;

    /// The activity/session API keeps the registry and the run state in step,
    /// closes on error and panic, and refuses over-budget work. (The global
    /// state means these checks run in one test.)
    #[test]
    fn sessions_close_on_every_path_and_budgets_refuse() {
        let dir = crate::safety::scratch_dir("sess");
        std::fs::create_dir_all(&dir).unwrap();
        init(&dir);
        let limits = Limits { goldfish_rounds: 2, redraft_turns: 0, build_attempts: 0, reviews: 0, model_calls: 3 };
        let g = Gate::new(7);

        // normal path: open two sessions, close them, finish with a gate event
        let act = begin(Some(&dir), "f", g, limits, Kind::Goldfish).unwrap();
        {
            let _a = open("goldfish").unwrap();
            let _b = open("goldfish-critic").unwrap();
            assert_eq!(live().len(), 2);
            let on_disk: Vec<Entry> = serde_json::from_str(&std::fs::read_to_string(dir.join("sessions.json")).unwrap()).unwrap();
            assert_eq!(on_disk.len(), 2, "the registry is persisted while sessions are open");
        }
        assert!(live().is_empty());
        let g2 = act.finish(Some(Event::Goldfish(true))).unwrap();
        assert_eq!(g2.passed_hash, Some(7));

        // error path: an activity dropped by `?` closes its sessions and cancels
        let r: anyhow::Result<()> = (|| {
            let _act = begin(Some(&dir), "f", g2, limits, Kind::Goldfish)?;
            let _s = open("goldfish")?;
            anyhow::bail!("reviewer failed")
        })();
        assert!(r.is_err());
        assert!(live().is_empty() && lock(&ACTIVE).is_none());

        // panic path: unwinding drops the guards
        let p = std::panic::catch_unwind(|| {
            let _act = begin(Some(&dir), "g", Gate::new(1), Limits::default(), Kind::Review).unwrap();
            let _s = open("reviewer").unwrap();
            panic!("boom");
        });
        assert!(p.is_err());
        assert!(live().is_empty() && lock(&ACTIVE).is_none(), "a panic leaves nothing open");

        // budgets: both goldfish rounds and all 3 calls are spent on `f`
        let spent = load_spent(&dir, "f");
        assert_eq!((spent.goldfish_rounds, spent.model_calls), (2, 3));
        let err = begin(Some(&dir), "f", g2, limits, Kind::Goldfish).err().unwrap().to_string();
        assert!(err.contains("used 2 of 2 goldfish rounds") && err.contains("set-budget --goldfish-rounds"), "{err}");
        let more = Limits { goldfish_rounds: 5, ..limits };
        let act = begin(Some(&dir), "f", g2, more, Kind::Goldfish).unwrap();
        let err = open("goldfish").err().unwrap().to_string();
        assert!(err.contains("used 3 of 3 model calls") && err.contains("--model-calls"), "{err}");
        drop(act);
        assert!(lock(&ACTIVE).is_none());
        let _ = Budget::default();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn start_ticks_identify_a_process() {
        let me = std::process::id();
        assert!(start_ticks(me).is_some());
        assert!(leader_alive(me, start_ticks(me)));
        assert!(!leader_alive(me, start_ticks(me).map(|t| t + 1)), "a reused pid is not mistaken for ours");
    }
}
