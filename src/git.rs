//! The tasks repo's git: a batch of changes is pulled before, each change
//! committed, then the batch pulled and pushed together. Output is captured,
//! so a verb's stdout is its own and the dashboard's terminal stays clean; a
//! failure carries what git said.

use std::path::Path;
use std::process::{Command, Stdio};

pub struct Git<'a> {
    root: &'a Path,
    /// Told each time a git command finishes, whether it succeeded or not.
    ran: &'a dyn Fn(),
}

impl<'a> Git<'a> {
    pub fn new(root: &'a Path, ran: &'a dyn Fn()) -> Git<'a> {
        Git { root, ran }
    }

    /// Catch up before a change. A failure is no reason not to make it: the
    /// pull after the commit tries again.
    pub fn pull(&self) {
        let _ = self.run(&["pull", "--rebase"]);
    }

    /// Commit everything changed under the root.
    pub fn commit(&self, message: &str) -> Result<(), String> {
        self.run(&["add", "-A"])?;
        self.run(&["commit", "-m", message])
    }

    /// Push what was committed, on top of whatever landed meanwhile.
    pub fn push(&self) -> Result<(), String> {
        self.run(&["pull", "--rebase"])?;
        self.run(&["push"])
    }

    fn run(&self, args: &[&str]) -> Result<(), String> {
        let out = Command::new("git")
            .args(args)
            .current_dir(self.root)
            .stdin(Stdio::null())
            .output();
        (self.ran)();
        let out = out.map_err(|e| format!("error: cannot run git: {e}"))?;
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
