//! Fixtures shared by the unit tests: temp dirs, a tasks repo with a remote,
//! and an app over task files on disk.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::CellWidth;
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, Config, Effect, ListOpts};
use crate::clock::Date;
use crate::ui;

/// A directory removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new() -> TempDir {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("taskania-test-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Write an executable script through a child process, so this one never
/// holds it open for writing. A test thread forking meanwhile would carry that
/// descriptor into its child, and running the script would then fail with
/// "Text file busy".
pub fn write_script(path: &Path, content: &str) {
    let mut child = Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(content.as_bytes())
        .unwrap();
    assert!(
        child.wait().unwrap().success(),
        "cannot write {}",
        path.display()
    );
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A tasks repo cloned from a bare origin, every project with all its status
/// directories.
pub struct TasksRepo {
    tmp: TempDir,
}

impl TasksRepo {
    pub fn new(projects: &[&str]) -> TasksRepo {
        let tmp = TempDir::new();
        let origin = tmp.path().join("origin.git");
        git(
            tmp.path(),
            &["init", "-q", "--bare", origin.to_str().unwrap()],
        );
        git(
            tmp.path(),
            &["clone", "-q", origin.to_str().unwrap(), "tasks"],
        );
        let r = TasksRepo { tmp };
        let root = r.root();
        git(&root, &["config", "user.email", "t@t"]);
        git(&root, &["config", "user.name", "T"]);
        for p in projects {
            for s in crate::listing::Status::ALL {
                fs::create_dir_all(root.join(p).join(s.dir())).unwrap();
            }
        }
        fs::write(root.join("README.md"), "tasks\n").unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "init"]);
        git(&root, &["branch", "-M", "main"]);
        git(&root, &["push", "-qu", "origin", "main"]);
        r
    }

    pub fn root(&self) -> PathBuf {
        self.tmp.path().join("tasks")
    }

    /// A task file, committed and pushed.
    pub fn seed(&self, project: &str, status: &str, file: &str, body: &str) {
        fs::write(self.root().join(project).join(status).join(file), body).unwrap();
        git(&self.root(), &["add", "-A"]);
        git(&self.root(), &["commit", "-qm", &format!("seed {file}")]);
        git(&self.root(), &["push", "-q"]);
    }

    pub fn at(&self, project: &str, status: &str, file: &str) -> bool {
        self.root().join(project).join(status).join(file).is_file()
    }

    pub fn subject(&self) -> String {
        git(&self.root(), &["log", "-1", "--format=%s"])
    }

    pub fn pushed(&self) -> bool {
        git(&self.root(), &["rev-parse", "HEAD"])
            == git(&self.root(), &["rev-parse", "origin/main"])
    }

    /// The last `n` commit subjects, newest first.
    pub fn subjects(&self, n: usize) -> Vec<String> {
        let n = format!("-{n}");
        let log = git(&self.root(), &["log", &n, "--format=%s"]);
        log.lines().map(String::from).collect()
    }

    /// Have origin count the pushes it receives from now on.
    pub fn count_pushes(&self) {
        self.hook("post-receive", "echo >> ../pushes");
    }

    pub fn pushes(&self) -> usize {
        let log = self.tmp.path().join("pushes");
        fs::read_to_string(log).map_or(0, |s| s.lines().count())
    }

    /// Have origin refuse every push from now on.
    pub fn refuse_pushes(&self) {
        self.hook("pre-receive", "echo refused >&2; exit 1");
    }

    fn hook(&self, name: &str, body: &str) {
        let path = self.tmp.path().join("origin.git/hooks").join(name);
        write_script(&path, &format!("#!/bin/sh\n{body}\n"));
    }
}

/// What the app fixture's primary lists: an open queue, a blank line, the
/// done tail.
pub const PROJ: &str = "\
ACTIVE (1)
  H010 busy
TODO (3)
  N001 alpha
  N002 beta
  N003 gamma

DONE (1)
  N050 shipped";

pub const WIDGET: &str = "\
TODO (2)
  N007 left-pad
  N008 right-pad";

/// Put a task file in the fixture's tasks root.
pub fn put(root: &Path, project: &str, status: &str, file: &str, body: &str) {
    let dir = root.join(project).join(status);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(file), body).unwrap();
}

/// An app over task files listing as PROJ, and as WIDGET beside it when
/// `second`. The files go away with the returned dir.
pub fn app(second: bool) -> (App, TempDir) {
    let tmp = TempDir::new();
    for (project, status, file, body) in [
        ("proj", "active", "H010-busy.md", "# Busy\n\nbody\n"),
        ("proj", "todo", "N001-alpha.md", "# Alpha\n\nfirst line\n"),
        ("proj", "todo", "N002-beta.md", ""),
        ("proj", "todo", "N003-gamma.md", "# Gamma\n"),
        ("proj", "done", "N050-shipped.md", "# Shipped\n"),
        ("widget", "todo", "N007-left-pad.md", "# Pad\n"),
        ("widget", "todo", "N008-right-pad.md", "# Right\n"),
    ] {
        put(tmp.path(), project, status, file, body);
    }
    let cfg = Config {
        primary: "proj".into(),
        second: second.then(|| "widget".into()),
        tasks_root: tmp.path().to_path_buf(),
        opts: ListOpts::default(),
        editor: Some("micro".into()),
    };
    let mut a = App::new(cfg, Instant::now());
    a.today = Date {
        year: 2026,
        month: 11,
        day: 16,
    };
    a.relist();
    (a, tmp)
}

pub fn key(a: &mut App, code: KeyCode) -> Vec<Effect> {
    a.on_key(KeyEvent::from(code), Instant::now())
}

/// One frame of the app, `w` by `h`.
pub fn draw(a: &mut App, w: u16, h: u16) -> Terminal<TestBackend> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| {
        if let Some(pos) = ui::render(f.area(), f.buffer_mut(), a) {
            f.set_cursor_position(pos);
        }
    })
    .unwrap();
    t
}

/// The frame's rows as text, trailing blanks trimmed.
pub fn lines(t: &Terminal<TestBackend>) -> Vec<String> {
    let buf = t.backend().buffer();
    (0..buf.area.height)
        .map(|y| {
            let mut s = String::new();
            let mut hidden = 0;
            for x in 0..buf.area.width {
                if hidden > 0 {
                    hidden -= 1;
                    continue;
                }
                let sym = buf[(x, y)].symbol();
                s.push_str(sym);
                hidden = sym.cell_width().saturating_sub(1);
            }
            s.trim_end().to_string()
        })
        .collect()
}
