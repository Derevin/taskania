//! The binary from outside: flags, startup errors, and the `--once` frame over
//! a stub `task-list.sh`.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_taskania");

/// A temp dir holding a tasks root and a dir of stub scripts.
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new() -> Sandbox {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("taskania-cli-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("tasks")).unwrap();
        Sandbox { root }
    }

    fn project(&self, name: &str) -> &Self {
        fs::create_dir_all(self.root.join("tasks").join(name)).unwrap();
        self
    }

    /// Written through a child process, so this one never holds the script
    /// open for writing: a test thread forking meanwhile would carry that
    /// descriptor along, and running the script would fail with "Text file busy".
    fn stub(&self, name: &str, body: &str) -> &Self {
        let p = self.root.join("bin").join(name);
        let mut child = Command::new("sh")
            .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
            .arg(&p)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let script = format!("#!/bin/bash\n{body}\n");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success());
        self
    }

    /// Run with only the stubs and the base system on PATH, stdin closed.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .env("TASKS_ROOT", self.root.join("tasks"))
            .env("LINES", "8")
            .env("COLUMNS", "60")
            .env_remove("TASK_PROJECT")
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A task-list.sh printing a canned queue for its last argument.
const LIST: &str = r#"
case "${@: -1}" in
  proj) printf 'TODO (2)\n  H003 fix-sensor-bug\n  N004 tidy\n' ;;
  widget) printf 'TODO (1)\n  N001 tweak-conf\n' ;;
  *) echo "error: tasks dir not found" >&2; exit 1 ;;
esac"#;

#[test]
fn help_prints_usage_and_needs_nothing_on_path() {
    let s = Sandbox::new();
    let out = s.run(&["--help"]);
    assert!(out.status.success());
    let help = text(&out.stdout);
    assert!(help.lines().next().unwrap().starts_with("Taskania"));
    assert!(help.lines().any(|l| l.starts_with("Usage: taskania")));
}

#[test]
fn without_the_task_scripts_it_says_so() {
    let s = Sandbox::new();
    s.project("proj");
    let out = s.run(&["--project", "proj", "--once"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("task-list.sh is not on PATH"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn once_prints_one_frame_sized_from_the_environment() {
    let s = Sandbox::new();
    s.project("proj")
        .project("widget")
        .stub("task-list.sh", LIST);
    let out = s.run(&["--project", "proj", "--with", "widget", "--once"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let frame = text(&out.stdout);
    let lines: Vec<&str> = frame.lines().collect();
    assert_eq!(lines.len(), 4, "{frame}");
    // The second column is its longest line wide, flush with the 60th cell.
    assert_eq!(lines[0], format!("{:<43}{}", "TODO (2)", "TODO (1)"));
    assert_eq!(
        lines[1],
        format!("{:<43}{}", "  H003 fix-sensor-bug", "  N001 tweak-conf")
    );
    assert_eq!(lines[2], "  N004 tidy");
    assert!(
        lines[3].starts_with("proj, done between ") && lines[3].ends_with(" widget"),
        "{}",
        lines[3]
    );
    assert!(frame.ends_with('\n'));
    assert!(!frame.contains('\x1b'), "plain text, no escapes");
}

#[test]
fn once_passes_the_toggles_to_task_list() {
    let s = Sandbox::new();
    let log = s.root.join("list.log");
    s.project("proj").stub(
        "task-list.sh",
        &format!("echo \"$*\" >> '{}'\necho '(none)'", log.display()),
    );
    assert!(s.run(&["--project", "proj", "--once"]).status.success());
    assert!(
        s.run(&[
            "--project",
            "proj",
            "--recent",
            "3",
            "--dates",
            "--labs",
            "--stale",
            "--once"
        ])
        .status
        .success()
    );
    assert_eq!(
        fs::read_to_string(&log).unwrap(),
        "--no-header --recent 7 --no-workers --no-stale proj\n--no-header --recent 3 --dates proj\n"
    );
}

#[test]
fn the_primary_defaults_to_find_projects_answer() {
    let s = Sandbox::new();
    s.project("widget")
        .stub("task-list.sh", LIST)
        .stub("find-project.sh", "echo widget");
    let out = s.run(&["--once"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).starts_with("TODO (1)\n  N001 tweak-conf\nwidget, done between"));

    s.stub(
        "find-project.sh",
        "echo 'error: not in a git repo' >&2; exit 1",
    );
    let out = s.run(&["--once"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("not in a git repo"));
}

#[test]
fn with_naming_the_primary_is_one_column() {
    let s = Sandbox::new();
    s.project("proj").stub("task-list.sh", LIST);
    let one = s.run(&["--project", "proj", "--once"]);
    let same = s.run(&["--project", "proj", "--with", "proj", "--once"]);
    assert_eq!(text(&same.stdout), text(&one.stdout));
}

#[test]
fn a_missing_project_or_value_is_an_error() {
    let s = Sandbox::new();
    s.project("proj").stub("task-list.sh", LIST);
    for (args, why) in [
        (&["--project", "nope", "--once"][..], "tasks dir not found"),
        (&["--project", "proj", "--with", "nope", "--once"], "tasks dir not found"),
        (&["--project", "proj", "--with"], "--with needs a value"),
        (&["--project", "proj", "--poll", "x"], "--poll takes"),
        (&["--bogus"], "unexpected argument: --bogus"),
    ] {
        let out = s.run(args);
        let err = text(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(err.starts_with("taskania: ") && err.contains(why), "{args:?}: {err}");
    }
}

#[test]
fn the_live_view_needs_a_terminal() {
    let s = Sandbox::new();
    s.project("proj").stub("task-list.sh", LIST);
    let out = s.run(&["--project", "proj"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("not a terminal"));
}
