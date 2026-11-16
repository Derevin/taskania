//! Every call Taskania makes outside itself: the task scripts, `find-project.sh`
//! and the editor. Taskania owns no task logic; the scripts do every change.
//!
//! Scripts resolve through PATH. A call that acts on a project gets it as
//! `TASK_PROJECT` in that child's environment only; every other script call has
//! the variable removed, so an exported one can't steer it.

use std::ffi::OsString;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use crate::listing::Status;

/// What `task-list.sh` is asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListOpts {
    pub recent: u32,
    pub dates: bool,
    pub labs: bool,
    pub stale: bool,
}

/// A change, made by one script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    Move {
        project: String,
        file: String,
        to: Status,
    },
    Create {
        project: String,
        letter: char,
        title: String,
    },
    Reprioritize {
        project: String,
        file: String,
        letter: char,
    },
    Commit {
        message: String,
    },
}

impl Call {
    /// The script, its arguments, and the project it acts on.
    pub fn argv(&self) -> (&'static str, Vec<String>, Option<&str>) {
        match self {
            Call::Move { project, file, to } => (
                "task-move.sh",
                vec![file.clone(), to.dir().to_string()],
                Some(project),
            ),
            Call::Create {
                project,
                letter,
                title,
            } => (
                "task-create.sh",
                vec!["-p".into(), letter.to_string(), title.clone()],
                Some(project),
            ),
            Call::Reprioritize {
                project,
                file,
                letter,
            } => (
                "task-reprioritize.sh",
                vec![file.clone(), letter.to_string()],
                Some(project),
            ),
            Call::Commit { message } => ("task-commit.sh", vec![message.clone()], None),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Engine {
    path: Option<OsString>,
}

impl Engine {
    pub fn new() -> Engine {
        Engine { path: None }
    }

    /// Resolve scripts through `path` instead of the inherited PATH.
    #[cfg(test)]
    pub fn with_path(path: impl Into<OsString>) -> Engine {
        Engine {
            path: Some(path.into()),
        }
    }

    fn search_path(&self) -> Option<OsString> {
        self.path.clone().or_else(|| std::env::var_os("PATH"))
    }

    fn script(&self, name: &str, project: Option<&str>) -> Command {
        let mut cmd = Command::new(name);
        if let Some(path) = &self.path {
            cmd.env("PATH", path);
        }
        match project {
            Some(p) => cmd.env("TASK_PROJECT", p),
            None => cmd.env_remove("TASK_PROJECT"),
        };
        cmd.stdin(Stdio::null());
        cmd
    }

    /// The executable `name` would resolve to.
    pub fn which(&self, name: &str) -> Option<PathBuf> {
        let path = self.search_path()?;
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|p| is_executable(p))
    }

    /// The cwd's project, as `find-project.sh` names it.
    pub fn find_project(&self) -> Result<String, String> {
        let out = self
            .script("find-project.sh", None)
            .output()
            .map_err(|e| format!("cannot run find-project.sh: {e}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let name = stdout.trim();
        if !out.status.success() {
            return Err(last_line(&out.stderr)
                .unwrap_or_else(|| format!("find-project.sh failed ({})", out.status)));
        }
        if name.is_empty() {
            return Err("find-project.sh named no project".into());
        }
        Ok(name.to_string())
    }

    pub fn list_args(project: &str, opts: &ListOpts) -> Vec<String> {
        let mut args = vec![
            "--no-header".to_string(),
            "--recent".into(),
            opts.recent.to_string(),
        ];
        if opts.dates {
            args.push("--dates".into());
        }
        if !opts.labs {
            args.push("--no-workers".into());
        }
        if !opts.stale {
            args.push("--no-stale".into());
        }
        args.push(project.to_string());
        args
    }

    /// A project's listing, stdout and stderr interleaved as the terminal would
    /// show them: an error from the script is rendered like any other line.
    pub fn list(&self, project: &str, opts: &ListOpts) -> String {
        self.list_merged(project, opts)
            .unwrap_or_else(|e| format!("cannot run task-list.sh: {e}"))
    }

    fn list_merged(&self, project: &str, opts: &ListOpts) -> io::Result<String> {
        let (mut reader, writer) = io::pipe()?;
        let mut child = {
            let mut cmd = self.script("task-list.sh", None);
            cmd.args(Self::list_args(project, opts))
                .stdout(writer.try_clone()?)
                .stderr(writer);
            cmd.spawn()?
            // The command drops here with its copies of the write end, so the
            // read below ends when the child exits.
        };
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        child.wait()?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Run a change. Ok holds the script's stdout; Err the last line of its
    /// stderr, or why it could not run.
    pub fn run(&self, call: &Call) -> Result<String, String> {
        let (name, args, project) = call.argv();
        let out = self
            .script(name, project)
            .args(&args)
            .output()
            .map_err(|e| format!("cannot run {name}: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(last_line(&out.stderr).unwrap_or_else(|| format!("{name} failed ({})", out.status)))
        }
    }

    /// Run the editor on `file` in this terminal. The value is a shell command,
    /// as git takes `$EDITOR`, so it may carry its own flags.
    pub fn edit(&self, editor: &str, file: &Path) -> io::Result<ExitStatus> {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(format!("{editor} \"$@\""))
            .arg(editor)
            .arg(file);
        if let Some(path) = &self.path {
            cmd.env("PATH", path);
        }
        cmd.status()
    }
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn last_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Stubs;

    const OPTS: ListOpts = ListOpts {
        recent: 7,
        dates: false,
        labs: false,
        stale: false,
    };

    #[test]
    fn list_asks_for_what_the_toggles_say() {
        let s = Stubs::new();
        s.record("task-list.sh", "echo 'TODO (1)'; echo '  N001 x'");
        let e = s.engine();
        assert_eq!(e.list("proj", &OPTS), "TODO (1)\n  N001 x\n");
        e.list(
            "widget",
            &ListOpts {
                recent: 3,
                dates: true,
                labs: true,
                stale: true,
            },
        );
        assert_eq!(
            s.calls("task-list.sh"),
            vec![
                s.call(
                    None,
                    &[
                        "--no-header",
                        "--recent",
                        "7",
                        "--no-workers",
                        "--no-stale",
                        "proj"
                    ]
                ),
                s.call(None, &["--no-header", "--recent", "3", "--dates", "widget"]),
            ]
        );
    }

    #[test]
    fn list_shows_the_scripts_errors_in_order() {
        let s = Stubs::new();
        s.add(
            "task-list.sh",
            "echo 'TODO (1)'; echo 'error: oops' >&2; echo '  N001 x'; exit 1",
        );
        assert_eq!(
            s.engine().list("proj", &OPTS),
            "TODO (1)\nerror: oops\n  N001 x\n"
        );
    }

    #[test]
    fn list_without_the_script_says_so() {
        let s = Stubs::new();
        let e = Engine::with_path(s.dir.as_os_str());
        assert!(e.list("proj", &OPTS).starts_with("cannot run task-list.sh"));
        assert_eq!(e.which("task-list.sh"), None);
        s.add("task-list.sh", "true");
        assert_eq!(e.which("task-list.sh"), Some(s.dir.join("task-list.sh")));
    }

    #[test]
    fn task_project_is_set_for_a_project_call_and_removed_otherwise() {
        let e = Engine::new();
        let envs = |cmd: &Command| {
            cmd.get_envs()
                .filter(|(k, _)| *k == "TASK_PROJECT")
                .map(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            envs(&e.script("task-move.sh", Some("proj"))),
            vec![Some("proj".to_string())]
        );
        assert_eq!(envs(&e.script("task-list.sh", None)), vec![None]);
    }

    #[test]
    fn each_change_runs_its_script_with_its_project() {
        let s = Stubs::new();
        for name in [
            "task-move.sh",
            "task-create.sh",
            "task-reprioritize.sh",
            "task-commit.sh",
        ] {
            s.record(name, "");
        }
        let e = s.engine();
        let calls = [
            Call::Move {
                project: "proj".into(),
                file: "N001-x.md".into(),
                to: Status::Done,
            },
            Call::Create {
                project: "widget".into(),
                letter: 'H',
                title: "fix the sensor".into(),
            },
            Call::Reprioritize {
                project: "proj".into(),
                file: "N001-x.md".into(),
                letter: 'U',
            },
            Call::Commit {
                message: "Edit: x".into(),
            },
        ];
        for c in &calls {
            assert!(e.run(c).is_ok());
        }
        assert_eq!(
            s.calls("task-move.sh"),
            vec![s.call(Some("proj"), &["N001-x.md", "done"])]
        );
        assert_eq!(
            s.calls("task-create.sh"),
            vec![s.call(Some("widget"), &["-p", "H", "fix the sensor"])]
        );
        assert_eq!(
            s.calls("task-reprioritize.sh"),
            vec![s.call(Some("proj"), &["N001-x.md", "U"])]
        );
        assert_eq!(s.calls("task-commit.sh"), vec![s.call(None, &["Edit: x"])]);
    }

    #[test]
    fn a_change_returns_stdout_or_the_last_stderr_line() {
        let s = Stubs::new();
        s.add(
            "task-create.sh",
            "echo 'Already up to date.'; echo N007-x.md",
        );
        s.add("task-move.sh", "echo 'remote: noise' >&2; echo 'error: only an active task goes stale' >&2; echo; exit 1");
        s.add("task-commit.sh", "exit 3");
        let e = s.engine();
        let create = Call::Create {
            project: "p".into(),
            letter: 'N',
            title: "x".into(),
        };
        assert_eq!(
            e.run(&create),
            Ok("Already up to date.\nN007-x.md\n".into())
        );
        let mv = Call::Move {
            project: "p".into(),
            file: "N001-x.md".into(),
            to: Status::Stale,
        };
        assert_eq!(
            e.run(&mv),
            Err("error: only an active task goes stale".into())
        );
        let commit = Call::Commit {
            message: "m".into(),
        };
        assert!(
            e.run(&commit)
                .unwrap_err()
                .starts_with("task-commit.sh failed")
        );
    }

    #[test]
    fn find_project_prints_the_name_or_the_failure() {
        let s = Stubs::new();
        s.add("find-project.sh", "echo widget");
        assert_eq!(s.engine().find_project(), Ok("widget".into()));
        s.add(
            "find-project.sh",
            "echo 'error: not in a git repo' >&2; exit 1",
        );
        assert_eq!(
            s.engine().find_project(),
            Err("error: not in a git repo".into())
        );
    }

    #[test]
    fn the_editor_is_a_shell_command_given_the_file() {
        let s = Stubs::new();
        s.record("myedit", "");
        let file = s.dir.join("N001 x.md");
        let status = s.engine().edit("myedit --wait", &file).unwrap();
        assert!(status.success());
        assert_eq!(
            s.calls("myedit"),
            vec![s.call(None, &["--wait", file.to_str().unwrap()])]
        );
    }
}
