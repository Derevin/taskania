//! What the dashboard does outside its own state: the changes, made off the UI
//! thread, and the editor.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::{Command, ExitStatus};

use crate::listing::Status;
use crate::tasks::Tasks;

/// A change, as one verb makes it.
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

#[derive(Clone)]
pub struct Engine {
    tasks: Tasks,
    /// The editor's PATH, when not the inherited one.
    path: Option<OsString>,
}

impl Engine {
    pub fn new(tasks: Tasks) -> Engine {
        Engine { tasks, path: None }
    }

    /// Run the editor with `path` as its PATH.
    #[cfg(test)]
    pub fn with_path(tasks: Tasks, path: impl Into<OsString>) -> Engine {
        Engine {
            tasks,
            path: Some(path.into()),
        }
    }

    /// Make a change. Ok holds what the verb prints; Err the last line of its
    /// error, which is what fits the bar.
    pub fn run(&self, call: &Call) -> Result<String, String> {
        let t = &self.tasks;
        let result = match call {
            Call::Move { project, file, to } => t.move_task(project, file, to.dir()),
            Call::Create {
                project,
                letter,
                title,
            } => t.create(project, false, &letter.to_string(), title),
            Call::Reprioritize {
                project,
                file,
                letter,
            } => t.reprioritize(project, file, &letter.to_string()),
            Call::Commit { message } => t.commit(message),
        };
        result.map_err(|e| {
            e.lines()
                .map(str::trim)
                .rfind(|l| !l.is_empty())
                .unwrap_or("")
                .to_string()
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TasksRepo, TempDir, write_script};
    use std::fs;

    #[test]
    fn each_change_lands_in_its_project_and_is_pushed() {
        let r = TasksRepo::new(&["proj", "widget"]);
        r.seed("proj", "todo", "N001-x.md", "# X\n");
        r.seed("widget", "todo", "N002-y.md", "");
        let e = Engine::new(Tasks::new(r.root()));
        let calls = [
            Call::Move {
                project: "proj".into(),
                file: "N001-x.md".into(),
                to: Status::Done,
            },
            Call::Reprioritize {
                project: "widget".into(),
                file: "N002-y.md".into(),
                letter: 'H',
            },
            Call::Create {
                project: "widget".into(),
                letter: 'U',
                title: "fix the sensor".into(),
            },
        ];
        let outs: Vec<_> = calls.iter().map(|c| e.run(c)).collect();
        assert_eq!(
            outs,
            vec![
                Ok("--- Moved: N001-x.md todo -> done ---\n".into()),
                Ok("--- N002-y.md -> H002-y.md (todo) ---\n".into()),
                Ok("U003-fix-the-sensor.md\n".into()),
            ]
        );
        assert!(r.at("proj", "done", "N001-x.md"));
        assert!(r.at("widget", "todo", "H002-y.md"));
        assert!(r.at("widget", "todo", "U003-fix-the-sensor.md"));

        fs::write(r.root().join("proj/done/N001-x.md"), "# X, edited\n").unwrap();
        let commit = Call::Commit {
            message: "Edit: x".into(),
        };
        assert_eq!(e.run(&commit), Ok(String::new()));
        assert_eq!(r.subject(), "Edit: x");
        assert!(r.pushed());
    }

    #[test]
    fn a_failed_change_returns_its_last_line() {
        let r = TasksRepo::new(&["proj"]);
        r.seed("proj", "todo", "N001-x.md", "");
        let e = Engine::new(Tasks::new(r.root()));
        let stale = Call::Move {
            project: "proj".into(),
            file: "N001-x.md".into(),
            to: Status::Stale,
        };
        assert_eq!(
            e.run(&stale),
            Err("error: only an active task goes stale (N001-x.md is in todo/)".into())
        );
        let nothing = Call::Commit {
            message: "m".into(),
        };
        assert_eq!(
            e.run(&nothing),
            Err("nothing to commit, working tree clean".into())
        );
    }

    #[test]
    fn the_editor_is_a_shell_command_given_the_file() {
        let tmp = TempDir::new();
        let log = tmp.path().join("log");
        write_script(
            &tmp.path().join("myedit"),
            &format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n", log.display()),
        );
        let file = tmp.path().join("N001 x.md");
        let path = format!("{}:/usr/bin:/bin", tmp.path().display());
        let e = Engine::with_path(Tasks::new(tmp.path().into()), path);
        let status = e.edit("myedit --wait", &file).unwrap();
        assert!(status.success());
        assert_eq!(
            fs::read_to_string(&log).unwrap(),
            format!("--wait\n{}\n", file.display())
        );
    }
}
