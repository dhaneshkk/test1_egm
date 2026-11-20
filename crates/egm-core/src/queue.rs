//! Which queued job the team app's worker starts next. Mirrored line for line by
//! `proofs/EgmQueue.lean`, which proves for every queue:
//!
//! * `start_keeps_one_per_project`: starting the picked job never makes two jobs of one project
//!   run at once (egm-dsh keeps its state in files in one checkout, so two jobs would collide);
//! * `start_is_oldest`: the picked job is the oldest queued job of its project;
//! * `start_under_ceiling` / `refuse_only_over`: a job starts only while its project is under
//!   its token ceiling, and is refused only when it is not.
//!
//! The ceiling is checked before a job starts, against what the project has already used this
//! month. A job's own use is not known in advance, so the last job started under the ceiling can
//! take the project over it; every job after that is refused.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job<'a> {
    pub id: i64,
    pub project: &'a str,
    pub state: JobState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Start(i64),
    Refuse(i64),
}

/// A project is over its ceiling once it has used at least that many tokens.
pub fn over_ceiling(spent: u64, ceiling: Option<u64>) -> bool {
    ceiling.is_some_and(|c| spent >= c)
}

/// `running` in Lean: some job of the project is running.
fn running(jobs: &[Job], project: &str) -> bool {
    jobs.iter().any(|j| j.project == project && j.state == JobState::Running)
}

/// `eligible` in Lean: queued, its project runs nothing, and no older job of its project waits.
fn eligible(jobs: &[Job], j: &Job) -> bool {
    j.state == JobState::Queued
        && !running(jobs, j.project)
        && jobs.iter().find(|k| k.project == j.project && k.state == JobState::Queued).map(|k| k.id) == Some(j.id)
}

/// The next job to start or refuse. `jobs` must be in queue order (oldest first);
/// `over(project)` says whether the project is at or over its token ceiling.
pub fn pick(jobs: &[Job], over: impl Fn(&str) -> bool) -> Option<Pick> {
    let j = jobs.iter().find(|j| eligible(jobs, j))?;
    Some(if over(j.project) { Pick::Refuse(j.id) } else { Pick::Start(j.id) })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROJECTS: [&str; 2] = ["a", "b"];
    const STATES: [JobState; 3] = [JobState::Queued, JobState::Running, JobState::Done];

    /// Every queue of up to 4 jobs over 2 projects and 3 states, ids in order.
    fn queues() -> Vec<Vec<Job<'static>>> {
        let mut out = vec![vec![]];
        for n in 1..=4 {
            let combos = (PROJECTS.len() * STATES.len()).pow(n as u32);
            for mut c in 0..combos {
                let mut q = vec![];
                for id in 0..n {
                    let x = c % 6;
                    c /= 6;
                    q.push(Job { id: id as i64, project: PROJECTS[x / 3], state: STATES[x % 3] });
                }
                out.push(q);
            }
        }
        out
    }

    fn one_per_project(q: &[Job]) -> bool {
        PROJECTS.iter().all(|p| q.iter().filter(|j| j.project == *p && j.state == JobState::Running).count() <= 1)
    }

    #[test]
    fn the_picked_job_keeps_one_per_project_and_is_the_oldest() {
        let mut checked = 0;
        for q in queues() {
            for over_a in [false, true] {
                let over = |p: &str| p == "a" && over_a;
                match pick(&q, over) {
                    Some(Pick::Start(id)) => {
                        let j = q.iter().find(|j| j.id == id).unwrap();
                        assert_eq!(j.state, JobState::Queued);
                        assert!(!over(j.project), "started over the ceiling: {q:?}");
                        assert!(!q.iter().any(|k| k.id < id && k.project == j.project && k.state == JobState::Queued), "not the oldest: {q:?}");
                        if one_per_project(&q) {
                            let after: Vec<Job> = q.iter().map(|k| if k.id == id { Job { state: JobState::Running, ..k.clone() } } else { k.clone() }).collect();
                            assert!(one_per_project(&after), "two running after starting {id}: {q:?}");
                        }
                    }
                    Some(Pick::Refuse(id)) => {
                        let j = q.iter().find(|j| j.id == id).unwrap();
                        assert!(over(j.project));
                        assert_eq!(j.state, JobState::Queued);
                    }
                    None => {
                        // nothing waits that could run: every queued job's project is busy or has an older waiter
                        for j in q.iter().filter(|j| j.state == JobState::Queued) {
                            assert!(running(&q, j.project) || q.iter().any(|k| k.id < j.id && k.project == j.project && k.state == JobState::Queued), "{q:?}");
                        }
                    }
                }
                checked += 1;
            }
        }
        assert!(checked > 2000, "{checked}");
    }

    #[test]
    fn ceiling() {
        assert!(!over_ceiling(10, None));
        assert!(!over_ceiling(9, Some(10)));
        assert!(over_ceiling(10, Some(10)));
        assert!(over_ceiling(0, Some(0)));
    }

    #[test]
    fn a_busy_project_does_not_hold_up_another() {
        let q = [
            Job { id: 1, project: "a", state: JobState::Running },
            Job { id: 2, project: "a", state: JobState::Queued },
            Job { id: 3, project: "b", state: JobState::Queued },
        ];
        assert_eq!(pick(&q, |_| false), Some(Pick::Start(3)));
        assert_eq!(pick(&q, |p| p == "b"), Some(Pick::Refuse(3)));
    }
}
