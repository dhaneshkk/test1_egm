//! One egm-dsh command at a time per project. `.egm-dsh/lock` holds the pid
//! of the running command; a lock whose process is gone is stale and removed.

use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct Lock {
    path: PathBuf,
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn alive(pid: u32) -> bool {
    if cfg!(target_os = "linux") {
        Path::new(&format!("/proc/{pid}")).exists()
    } else {
        true // elsewhere only age makes a lock stale
    }
}

fn stale(path: &Path) -> bool {
    let pid = std::fs::read_to_string(path).ok().and_then(|s| s.trim().parse::<u32>().ok());
    let old = std::fs::metadata(path).and_then(|m| m.modified()).ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() > 24 * 3600);
    match pid {
        Some(p) => !alive(p) || old,
        None => old,
    }
}

pub fn acquire(egm_dir: &Path) -> anyhow::Result<Lock> {
    let path = egm_dir.join("lock");
    for _ in 0..2 {
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                write!(f, "{}", std::process::id())?;
                return Ok(Lock { path });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if stale(&path) {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                let pid = std::fs::read_to_string(&path).unwrap_or_default();
                anyhow::bail!("another egm-dsh command (pid {}) is running in this project; wait for it, or delete {} if it is gone", pid.trim(), path.display());
            }
            Err(e) => return Err(e.into()),
        }
    }
    anyhow::bail!("could not take {}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_holder_is_refused_and_stale_locks_are_taken_over() {
        let d = crate::safety::scratch_dir("lock");
        std::fs::create_dir_all(&d).unwrap();
        let first = acquire(&d).unwrap();
        assert!(acquire(&d).unwrap_err().to_string().contains("another egm-dsh command"));
        drop(first);
        let again = acquire(&d).unwrap();
        drop(again);
        // a lock left by a process that no longer exists is stale
        std::fs::write(d.join("lock"), "999999999").unwrap();
        assert!(acquire(&d).is_ok());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
