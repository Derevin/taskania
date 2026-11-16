//! Fixtures shared by the unit tests: temp dirs, recording stub scripts, and
//! an app over canned listings.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, Config, Effect};
use crate::clock::Date;
use crate::engine::{Engine, ListOpts};

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

/// Stub scripts placed first on PATH. A recording stub logs each call as one
/// line: its `TASK_PROJECT` (empty when unset), then its arguments.
pub struct Stubs {
    pub dir: PathBuf,
    logdir: PathBuf,
    _tmp: TempDir,
}

impl Stubs {
    pub fn new() -> Stubs {
        let tmp = TempDir::new();
        let dir = tmp.path().join("bin");
        let logdir = tmp.path().join("logs");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&logdir).unwrap();
        Stubs {
            dir,
            logdir,
            _tmp: tmp,
        }
    }

    pub fn add(&self, name: &str, body: &str) {
        write_script(&self.dir.join(name), &format!("#!/bin/bash\n{body}\n"));
    }

    pub fn record(&self, name: &str, prelude: &str) {
        let log = self.logdir.join(format!("{name}.log"));
        let log = log.display();
        self.add(
            name,
            &format!(
                "{prelude}\nprintf '%s\\t' \"${{TASK_PROJECT+TASK_PROJECT=$TASK_PROJECT}}\" \"$@\" >> '{log}'\nprintf '\\n' >> '{log}'\nexit 0"
            ),
        );
    }

    pub fn calls(&self, name: &str) -> Vec<Vec<String>> {
        let Ok(content) = fs::read_to_string(self.logdir.join(format!("{name}.log"))) else {
            return vec![];
        };
        content
            .lines()
            .map(|line| {
                line.strip_suffix('\t')
                    .unwrap_or(line)
                    .split('\t')
                    .map(String::from)
                    .collect()
            })
            .collect()
    }

    /// The line a recording stub logs for this call.
    pub fn call(&self, project: Option<&str>, args: &[&str]) -> Vec<String> {
        let mut v = vec![
            project
                .map(|p| format!("TASK_PROJECT={p}"))
                .unwrap_or_default(),
        ];
        v.extend(args.iter().map(|a| a.to_string()));
        v
    }

    /// An engine finding these stubs and the base system only: an unstubbed
    /// script fails rather than reaching an installed one and the real tasks
    /// repo.
    pub fn engine(&self) -> Engine {
        Engine::with_path(format!("{}:/usr/bin:/bin", self.dir.display()))
    }
}

/// The primary's listing in the app fixtures: an open queue, a blank line,
/// the done tail.
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

/// An app over the fixture listings: `proj`, and `widget` beside it when
/// `second`. The tasks root does not exist; tests needing files make one.
pub fn app(second: bool) -> App {
    let cfg = Config {
        primary: "proj".into(),
        second: second.then(|| "widget".into()),
        tasks_root: PathBuf::from("/nonexistent"),
        opts: ListOpts {
            recent: 7,
            dates: false,
            labs: false,
            stale: false,
        },
        editor: Some("micro".into()),
    };
    let mut a = App::new(cfg, Instant::now());
    a.today = Date {
        year: 2026,
        month: 11,
        day: 16,
    };
    a.set_listings(if second {
        vec![PROJ.into(), WIDGET.into()]
    } else {
        vec![PROJ.into()]
    });
    a
}

pub fn key(a: &mut App, code: KeyCode) -> Vec<Effect> {
    a.on_key(KeyEvent::from(code), Instant::now())
}
