//! The tasks repo's git: every change is pulled before, then committed and
//! pushed. Output is captured, so a verb's stdout is its own and the dashboard's
//! terminal stays clean; a failure carries what git said.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Git {
    root: PathBuf,
}

impl Git {
    pub fn new(root: &Path) -> Git {
        Git {
            root: root.to_path_buf(),
        }
    }

    /// Catch up before a change. A failure is no reason not to make it: the
    /// pull after the commit tries again.
    pub fn pull(&self) {
        let _ = self.run(&["pull", "--rebase"]);
    }

    /// Commit everything changed under the root and push it.
    pub fn publish(&self, message: &str) -> Result<(), String> {
        self.run(&["add", "-A"])?;
        self.run(&["commit", "-m", message])?;
        self.run(&["pull", "--rebase"])?;
        self.run(&["push"])
    }

    fn run(&self, args: &[&str]) -> Result<(), String> {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("error: cannot run git: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        // `git commit` with nothing to commit says so on stdout.
        let said = [&out.stderr, &out.stdout]
            .into_iter()
            .map(|b| String::from_utf8_lossy(b).trim().to_string())
            .find(|s| !s.is_empty());
        Err(said.unwrap_or_else(|| format!("error: git {} failed ({})", args[0], out.status)))
    }
}
