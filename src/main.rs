//! Taskania, the task dashboard: the live view of one or two projects' task
//! queues, with a cursor to peek, edit, create, move and reprioritize tasks.
//! It owns no task logic; every change is a task script's, called through
//! `engine`.

mod app;
mod args;
mod clock;
mod engine;
mod listing;
#[cfg(test)]
mod testutil;
mod ui;
mod watch;

use std::collections::VecDeque;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{self, Event};
use ratatui::crossterm::{execute, terminal};
use ratatui::layout::Rect;

use app::{App, Config, Effect, Job};
use args::{Args, Parsed};
use clock::Date;
use engine::{Engine, ListOpts};

fn main() -> ExitCode {
    let args = match args::parse(std::env::args().skip(1)) {
        Ok(Parsed::Help) => {
            println!("{}", args::HELP);
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Run(args)) => args,
        Err(e) => {
            eprintln!("taskania: {e}");
            return ExitCode::FAILURE;
        }
    };
    match start(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("taskania: {e}");
            ExitCode::FAILURE
        }
    }
}

fn start(args: Args) -> Result<(), String> {
    let engine = Engine::new();
    if engine.which("task-list.sh").is_none() {
        return Err(
            "task-list.sh is not on PATH; Taskania needs the task scripts \
             (scripts/ in github.com/Derevin/dotfiles-public)"
                .into(),
        );
    }
    let tasks_root = env_nonempty("TASKS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env_nonempty("HOME").unwrap_or_default()).join("repos/tasks")
        });
    let primary = match args.project {
        Some(p) => p,
        None => engine.find_project()?,
    };
    check_dir(&tasks_root, &primary)?;
    // Naming the primary again collapses to one column, so a caller can pass
    // --with unconditionally.
    let second = args.with.filter(|w| *w != primary);
    if let Some(w) = &second {
        check_dir(&tasks_root, w)?;
    }
    let editor = env_nonempty("VISUAL").or_else(|| env_nonempty("EDITOR"));
    let opts = ListOpts {
        recent: args.recent,
        dates: args.dates,
        labs: args.labs,
        stale: args.stale,
    };
    let mut app = App::new(
        Config {
            primary,
            second,
            tasks_root,
            opts,
            editor,
        },
        Instant::now(),
    );
    relist(&mut app, &engine);

    if args.once {
        let (width, height) = once_size();
        let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
        ui::render(buf.area, &mut buf, &mut app);
        print!("{}", ui::frame_text(&buf));
        return Ok(());
    }
    if !io::stdout().is_terminal() {
        return Err("stdout is not a terminal (--once prints a single frame)".into());
    }
    let terminal = ratatui::try_init().map_err(|e| {
        ratatui::restore();
        format!("cannot take over the terminal: {e}")
    })?;
    let result = live(terminal, app, engine, Duration::from_secs(args.poll));
    ratatui::restore();
    result.map_err(|e| e.to_string())
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn check_dir(root: &Path, project: &str) -> Result<(), String> {
    let dir = root.join(project);
    if dir.is_dir() {
        Ok(())
    } else {
        Err(format!("tasks dir not found: {}", dir.display()))
    }
}

/// The frame size for --once: the tty's when stdin is one, else LINES and
/// COLUMNS, else 24x80.
fn once_size() -> (u16, u16) {
    if io::stdin().is_terminal()
        && let Ok((w, h)) = terminal::size()
        && w > 0
        && h > 0
    {
        return (w, h);
    }
    let var = |name: &str, default: u16| {
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&n: &u16| n > 0)
            .unwrap_or(default)
    };
    (var("COLUMNS", 80), var("LINES", 24))
}

fn relist(app: &mut App, engine: &Engine) {
    let outputs = app
        .columns
        .iter()
        .map(|c| engine.list(&c.project, &app.opts))
        .collect();
    app.set_listings(outputs);
}

fn live(
    mut terminal: DefaultTerminal,
    mut app: App,
    engine: Engine,
    poll: Duration,
) -> io::Result<()> {
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

    let result = run_loop(&mut terminal, &mut app, &engine, poll, &job_tx, &done_rx);

    // A change already queued finishes, also when the loop failed: a script
    // cut off between its commit and its push would leave the tasks repo out
    // of step with its remote.
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
    poll: Duration,
    job_tx: &mpsc::Sender<Job>,
    done_rx: &mpsc::Receiver<(Job, Result<String, String>)>,
) -> io::Result<()> {
    let dirs = app.project_dirs();
    let mut sig = watch::signature(&dirs);
    let mut next_poll = Instant::now() + poll;
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
            next_poll = now + poll;
            changed |= watch::signature(&dirs) != sig;
            app.refresh_peek();
        }
        if changed {
            // Signature first: a change landing during the listing shows up
            // as a difference at the next poll rather than being missed.
            sig = watch::signature(&dirs);
            relist(app, engine);
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
