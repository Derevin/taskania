//! Taskania, the task tool: verbs that list and change tasks, and a dashboard
//! (`--watch`) showing one or two projects' queues live, with a cursor to peek,
//! edit, create, move and reprioritize tasks.

mod app;
mod args;
mod cli;
mod clock;
mod engine;
mod git;
mod listing;
mod project;
mod tasks;
#[cfg(test)]
mod testutil;
mod ui;
mod watch;

use std::collections::VecDeque;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event};
use ratatui::crossterm::{execute, terminal};
use ratatui::layout::Rect;

use app::{App, Config, Effect, Job, ListOpts};
use args::Parsed;
use clock::Date;
use engine::Engine;
use tasks::Tasks;

/// How often the dashboard checks for changed task files.
const POLL: Duration = Duration::from_secs(2);

/// Where Taskania looks: the tasks root, the cwd a project is resolved from,
/// and the conf mapping checkout names to projects.
pub struct Ctx {
    pub root: PathBuf,
    pub cwd: PathBuf,
    pub projects_conf: PathBuf,
}

impl Ctx {
    fn from_env() -> Ctx {
        Ctx::from_vars(|name| std::env::var(name).ok(), logical_cwd())
    }

    /// An empty variable counts as unset.
    fn from_vars(var: impl Fn(&str) -> Option<String>, cwd: PathBuf) -> Ctx {
        let var = |name| var(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = || var("HOME").unwrap_or_default();
        let root = var("TASKS_ROOT").unwrap_or_else(|| home().join("repos/tasks"));
        let projects_conf = var("TASKANIA_PROJECTS_CONF").unwrap_or_else(|| {
            var("XDG_CONFIG_HOME")
                .unwrap_or_else(|| home().join(".config"))
                .join("taskania/projects.conf")
        });
        Ctx {
            root,
            cwd,
            projects_conf,
        }
    }

    pub fn cwd_project(&self) -> Result<String, String> {
        project::resolve(&self.cwd, &self.projects_conf)
    }

    pub fn tasks(&self) -> Tasks {
        Tasks::new(self.root.clone())
    }
}

fn main() -> ExitCode {
    let parsed = match args::parse(std::env::args().skip(1)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("taskania: {e}");
            return ExitCode::FAILURE;
        }
    };
    let ctx = Ctx::from_env();
    let result = match parsed {
        Parsed::Help(text) => Ok(format!("{text}\n")),
        Parsed::Run { project, verb } => cli::run(&ctx, project, verb),
        Parsed::Watch { with } => dashboard(&ctx, with).map(|()| String::new()),
    };
    match result {
        Ok(out) => {
            // A reader that stopped early (`| head`) is no failure.
            let _ = io::stdout().write_all(out.as_bytes());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// The cwd as the shell spells it, symlinks kept, when `PWD` still names it.
fn logical_cwd() -> PathBuf {
    let real = std::env::current_dir().unwrap_or_default();
    match env_nonempty("PWD").map(PathBuf::from) {
        Some(pwd)
            if pwd.is_absolute() && fs::canonicalize(&pwd).ok() == fs::canonicalize(&real).ok() =>
        {
            pwd
        }
        _ => real,
    }
}

fn dashboard(ctx: &Ctx, with: Option<String>) -> Result<(), String> {
    let app = open(ctx, with)?;
    if !io::stdout().is_terminal() {
        return Err("error: stdout is not a terminal".into());
    }
    let terminal = ratatui::try_init().map_err(|e| {
        ratatui::restore();
        format!("error: cannot take over the terminal: {e}")
    })?;
    let result = live(terminal, app, Engine::new(ctx.tasks()));
    ratatui::restore();
    result.map_err(|e| format!("error: {e}"))
}

/// The dashboard over the cwd's project, `with` beside it.
fn open(ctx: &Ctx, with: Option<String>) -> Result<App, String> {
    let tasks = ctx.tasks();
    let primary = ctx.cwd_project()?;
    tasks.project_dir(&primary)?;
    // Naming the primary again collapses to one column, so a caller can pass
    // --with unconditionally.
    let second = with.filter(|w| *w != primary);
    if let Some(w) = &second {
        tasks.project_dir(w)?;
    }
    let editor = env_nonempty("VISUAL").or_else(|| env_nonempty("EDITOR"));
    let mut app = App::new(
        Config {
            primary,
            second,
            tasks_root: ctx.root.clone(),
            opts: ListOpts::default(),
            editor,
        },
        Instant::now(),
    );
    app.relist();
    Ok(app)
}

fn live(mut terminal: DefaultTerminal, mut app: App, engine: Engine) -> io::Result<()> {
    // Changes run off the UI thread, one at a time, in the order they were made.
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let (done_tx, done_rx) = mpsc::channel();
    let runner = engine.clone();
    let worker = thread::spawn(move || {
        for job in job_rx {
            let result = runner.run(&job.call);
            if done_tx.send((job, result)).is_err() {
                break;
            }
        }
    });

    let result = run_loop(&mut terminal, &mut app, &engine, &job_tx, &done_rx);

    // A change already queued finishes, also when the loop failed: one cut off
    // between its commit and its push would leave the tasks repo out of step
    // with its remote.
    drop(job_tx);
    if app.in_flight > 0 {
        ratatui::restore();
        eprintln!("taskania: finishing {} queued change(s)…", app.in_flight);
    }
    let _ = worker.join();
    result
}

fn run_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    engine: &Engine,
    job_tx: &mpsc::Sender<Job>,
    done_rx: &mpsc::Receiver<(Job, Result<String, String>)>,
) -> io::Result<()> {
    let dirs = app.project_dirs();
    let mut sig = watch::signature(&dirs);
    let mut next_poll = Instant::now() + POLL;
    loop {
        app.today = Date::today();
        terminal.draw(|f| {
            if let Some(pos) = ui::render(f.area(), f.buffer_mut(), app) {
                f.set_cursor_position(pos);
            }
        })?;

        // Wake for the next poll, for the cursor's timeout, and often while a
        // job runs, so its result shows as soon as it lands.
        let mut wait = next_poll
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(1));
        if app.in_flight > 0 {
            wait = wait.min(Duration::from_millis(100));
        }
        let mut effects = VecDeque::new();
        // A resize needs no handling: the next draw fits the new size.
        if event::poll(wait)?
            && let Event::Key(key) = event::read()?
        {
            effects.extend(app.on_key(key, Instant::now()));
        }
        while let Ok((job, result)) = done_rx.try_recv() {
            effects.extend(app.on_job_done(job, result, Instant::now()));
        }

        let mut changed = false;
        while let Some(effect) = effects.pop_front() {
            match effect {
                Effect::Quit => return Ok(()),
                Effect::Relist => changed = true,
                Effect::Run(job) => {
                    if let Err(mpsc::SendError(job)) = job_tx.send(job) {
                        let err = Err("job runner stopped".to_string());
                        effects.extend(app.on_job_done(job, err, Instant::now()));
                    }
                }
                Effect::Edit(edit) => {
                    let status = suspended(terminal, || engine.edit(&edit.editor, &edit.path))?;
                    effects.extend(app.after_edit(edit, status, Instant::now()));
                }
            }
        }

        let now = Instant::now();
        app.tick(now);
        if now >= next_poll {
            next_poll = now + POLL;
            changed |= watch::signature(&dirs) != sig;
            app.refresh_peek();
        }
        if changed {
            // Signature first: a change landing during the listing shows up
            // as a difference at the next poll rather than being missed.
            sig = watch::signature(&dirs);
            app.relist();
        }
    }
}

/// Hand the terminal to `f` (the editor), then take it back.
fn suspended<T>(terminal: &mut DefaultTerminal, f: impl FnOnce() -> T) -> io::Result<T> {
    terminal.show_cursor()?;
    ratatui::try_restore()?;
    let out = f();
    terminal::enable_raw_mode()?;
    execute!(io::stdout(), terminal::EnterAlternateScreen)?;
    // A resize clears and repaints everything on the next draw. Terminal::clear
    // would too, but it first asks the terminal where its cursor is, and a
    // terminal that never answers fails the whole app.
    let size = terminal.size()?;
    terminal.resize(Rect::new(0, 0, size.width, size.height))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TempDir, draw, git, lines, put};

    /// A tasks root holding `proj` and `widget`, and a checkout of `proj`.
    fn ctx(tmp: &TempDir) -> Ctx {
        let root = tmp.path().join("tasks");
        put(&root, "proj", "todo", "H003-fix-sensor-bug.md", "");
        put(&root, "proj", "todo", "N004-tidy.md", "");
        put(&root, "widget", "todo", "N001-tweak-conf.md", "");
        let cwd = tmp.path().join("Proj-hlab-012-x");
        fs::create_dir_all(&cwd).unwrap();
        git(&cwd, &["init", "-q"]);
        Ctx {
            projects_conf: root.join("projects.conf"),
            root,
            cwd,
        }
    }

    #[test]
    fn the_dashboard_opens_on_the_cwds_project_with_the_second_beside_it() {
        let tmp = TempDir::new();
        let mut app = open(&ctx(&tmp), Some("widget".into())).unwrap();
        app.today = Date {
            year: 2026,
            month: 11,
            day: 16,
        };
        let got = lines(&draw(&mut app, 60, 8));
        // The second column is its longest line wide, flush with the 60th cell.
        assert_eq!(got[0], format!("{:<43}{}", "TODO (2)", "TODO (1)"));
        assert_eq!(
            got[1],
            format!("{:<43}{}", "  H003 fix-sensor-bug", "  N001 tweak-conf")
        );
        assert_eq!(got[2], "  N004 tidy");
        assert!(got[3..7].iter().all(String::is_empty));
        assert!(
            got[7].starts_with("proj, done between Mo 09-11 and 16-11")
                && got[7].ends_with(" widget"),
            "{}",
            got[7]
        );
    }

    #[test]
    fn the_projects_conf_defaults_to_the_xdg_config_dir() {
        let conf = |vars: &[(&str, &str)]| {
            let var = |name: &str| {
                vars.iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            };
            Ctx::from_vars(var, PathBuf::new()).projects_conf
        };
        let home = ("HOME", "/h");
        assert_eq!(
            conf(&[home]),
            PathBuf::from("/h/.config/taskania/projects.conf")
        );
        assert_eq!(
            conf(&[home, ("XDG_CONFIG_HOME", "")]),
            PathBuf::from("/h/.config/taskania/projects.conf")
        );
        assert_eq!(
            conf(&[home, ("XDG_CONFIG_HOME", "/x")]),
            PathBuf::from("/x/taskania/projects.conf")
        );
        assert_eq!(
            conf(&[
                home,
                ("XDG_CONFIG_HOME", "/x"),
                ("TASKANIA_PROJECTS_CONF", "/p.conf")
            ]),
            PathBuf::from("/p.conf")
        );
    }

    #[test]
    fn naming_the_primary_again_is_one_column() {
        let tmp = TempDir::new();
        let app = open(&ctx(&tmp), Some("proj".into())).unwrap();
        assert_eq!(app.columns.len(), 1);
    }

    #[test]
    fn the_dashboard_needs_its_projects() {
        let tmp = TempDir::new();
        let c = ctx(&tmp);
        let err = |c: &Ctx, with: Option<&str>| open(c, with.map(String::from)).err().unwrap();
        let nope = c.root.join("nope");
        assert_eq!(
            err(&c, Some("nope")),
            format!("error: tasks dir not found: {}", nope.display())
        );
        let elsewhere = Ctx {
            cwd: tmp.path().to_path_buf(),
            ..ctx(&tmp)
        };
        assert_eq!(err(&elsewhere, None), "error: not in a git repo");
        fs::write(c.root.join("projects.conf"), "Proj = gadget\n").unwrap();
        assert!(err(&c, None).starts_with("error: tasks dir not found"));
    }
}
