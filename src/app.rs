//! Taskania's state and what each key does to it: a state machine fed key
//! events, listings and job results. It reads task files but changes nothing
//! itself; every change comes back as an [`Effect`] for the loop to carry out.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::clock::Date;
use crate::engine::{Call, ListOpts};
use crate::listing::{self, Row, Status, Task, TaskKey};
use crate::watch::{self, Stamp};

/// How long the cursor outlives the last key.
pub const CURSOR_TIMEOUT: Duration = Duration::from_secs(60);
const INFO_FOR: Duration = Duration::from_secs(5);
const ERROR_FOR: Duration = Duration::from_secs(30);

pub struct Config {
    pub primary: String,
    pub second: Option<String>,
    pub tasks_root: PathBuf,
    pub opts: ListOpts,
    pub editor: Option<String>,
}

pub struct Column {
    pub project: String,
    pub rows: Vec<Row>,
    /// First row shown while the cursor is in this column.
    pub offset: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub col: usize,
    pub row: usize,
    pub key: TaskKey,
}

pub enum Mode {
    Normal,
    Peek(Peek),
    Move { task: Task, worker: Option<String> },
    Prio { task: Task, worker: Option<String> },
    Form(Form),
}

pub struct Peek {
    pub col: usize,
    pub key: TaskKey,
    pub path: PathBuf,
    /// None once the file is gone and could not be found again.
    pub text: Option<String>,
    pub stamp: Option<Stamp>,
    pub scroll: usize,
}

pub struct Form {
    pub col: usize,
    pub letter: char,
    pub title: Vec<char>,
    pub pos: usize,
    pub in_prio: bool,
}

pub struct Message {
    pub text: String,
    pub error: bool,
    until: Instant,
}

/// A script call queued for the worker thread, with what the app needs back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub call: Call,
    pub col: usize,
    /// The task acted on, dimmed until the job is done.
    pub key: Option<TaskKey>,
    /// The bar's line when it succeeds.
    pub done: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    Quit,
    Relist,
    Run(Job),
    Edit(Edit),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub editor: String,
    pub path: PathBuf,
    pub col: usize,
    pub name: String,
    pub slug: String,
    /// The content before the editor ran; None when it could not be read.
    pub before: Option<Vec<u8>>,
}

pub struct App {
    pub tasks_root: PathBuf,
    pub opts: ListOpts,
    pub columns: Vec<Column>,
    pub cursor: Option<Cursor>,
    pub mode: Mode,
    pub today: Date,
    pub editor: Option<String>,
    /// Jobs queued or running.
    pub in_flight: usize,
    pub pending: Vec<(usize, TaskKey)>,
    pub message: Option<Message>,
    /// Lines the peek shows at once, for paging; kept by the renderer.
    pub peek_rows: usize,
    /// Where the cursor lands after the next listing (a created task).
    focus: Option<(usize, TaskKey)>,
    last_key: Instant,
}

impl App {
    pub fn new(cfg: Config, now: Instant) -> App {
        let mut columns = vec![Column {
            project: cfg.primary,
            rows: vec![],
            offset: 0,
        }];
        if let Some(second) = cfg.second {
            columns.push(Column {
                project: second,
                rows: vec![],
                offset: 0,
            });
        }
        App {
            tasks_root: cfg.tasks_root,
            opts: cfg.opts,
            columns,
            cursor: None,
            mode: Mode::Normal,
            today: Date::today(),
            editor: cfg.editor,
            in_flight: 0,
            pending: vec![],
            message: None,
            peek_rows: 20,
            focus: None,
            last_key: now,
        }
    }

    pub fn project_dirs(&self) -> Vec<PathBuf> {
        self.columns
            .iter()
            .map(|c| self.tasks_root.join(&c.project))
            .collect()
    }

    pub fn cursor_task(&self) -> Option<(usize, &Task)> {
        let c = self.cursor.as_ref()?;
        let task = self.columns.get(c.col)?.rows.get(c.row)?.task.as_ref()?;
        Some((c.col, task))
    }

    pub fn is_pending(&self, col: usize, key: &TaskKey) -> bool {
        self.pending.iter().any(|(c, k)| *c == col && k == key)
    }

    fn task_path(&self, col: usize, task: &Task) -> PathBuf {
        self.tasks_root
            .join(&self.columns[col].project)
            .join(task.status.dir())
            .join(task.filename())
    }

    // --- listings ----------------------------------------------------------

    /// New listings, one per column in order. The cursor follows its task;
    /// when the task left the view it stays at its row, on whatever task row
    /// is nearest.
    pub fn set_listings(&mut self, outputs: Vec<String>) {
        for (col, out) in outputs.into_iter().enumerate() {
            if let Some(column) = self.columns.get_mut(col) {
                column.rows = listing::parse(&out);
            }
        }
        if let Some((col, key)) = self.focus.take()
            && let Some(row) = self.find(col, &key)
        {
            self.cursor = Some(Cursor { col, row, key });
        }
        if let Some(c) = self.cursor.clone() {
            self.cursor = match self.find(c.col, &c.key) {
                Some(row) => Some(Cursor { row, ..c }),
                None => self
                    .nearest(c.col, c.row)
                    .map(|row| self.cursor_at(c.col, row)),
            };
        }
        // A menu belongs to the task it was opened on.
        let menu_task = match &self.mode {
            Mode::Move { task, .. } | Mode::Prio { task, .. } => Some(task.key()),
            _ => None,
        };
        if let Some(key) = menu_task
            && self.cursor.as_ref().map(|c| &c.key) != Some(&key)
        {
            self.mode = Mode::Normal;
        }
    }

    fn find(&self, col: usize, key: &TaskKey) -> Option<usize> {
        let rows = &self.columns.get(col)?.rows;
        rows.iter().position(|r| {
            r.task
                .as_ref()
                .is_some_and(|t| t.id == key.id && t.slug == key.slug)
        })
    }

    fn task_rows(&self, col: usize) -> Vec<usize> {
        let Some(column) = self.columns.get(col) else {
            return vec![];
        };
        column
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.task.is_some())
            .map(|(i, _)| i)
            .collect()
    }

    /// The first task row at or below `row`, else the last one above it.
    fn nearest(&self, col: usize, row: usize) -> Option<usize> {
        let rows = self.task_rows(col);
        rows.iter()
            .copied()
            .find(|&i| i >= row)
            .or_else(|| rows.last().copied())
    }

    fn cursor_at(&self, col: usize, row: usize) -> Cursor {
        let key = self.columns[col].rows[row]
            .task
            .as_ref()
            .map(Task::key)
            .expect("a task row");
        Cursor { col, row, key }
    }

    /// Where the cursor goes once the task at `row` is moved away: the row
    /// that slides into its place (the next one in its section), else the one
    /// above it in the section, else the nearest anywhere.
    fn successor(&self, col: usize, row: usize) -> Option<usize> {
        let rows = &self.columns[col].rows;
        let status = rows[row].task.as_ref()?.status;
        let free = |i: &usize| {
            rows[*i]
                .task
                .as_ref()
                .is_some_and(|t| !self.is_pending(col, &t.key()))
        };
        let same = |i: &usize| rows[*i].task.as_ref().is_some_and(|t| t.status == status);
        let below = row + 1..rows.len();
        below
            .clone()
            .filter(free)
            .find(same)
            .or_else(|| (0..row).rev().filter(free).find(same))
            .or_else(|| below.clone().find(free))
            .or_else(|| (0..row).rev().find(free))
    }

    // --- keys --------------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent, now: Instant) -> Vec<Effect> {
        if key.kind == KeyEventKind::Release {
            return vec![];
        }
        self.last_key = now;
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return if key.code == KeyCode::Char('c') {
                vec![Effect::Quit]
            } else {
                vec![]
            };
        }
        // Bare keys only: tmux owns Alt.
        if key.modifiers.contains(KeyModifiers::ALT) {
            return vec![];
        }
        match self.mode {
            Mode::Normal => self.normal_key(key.code, now),
            Mode::Peek(_) => self.peek_key(key.code),
            Mode::Move { .. } => self.move_key(key.code),
            Mode::Prio { .. } => self.prio_key(key.code, now),
            Mode::Form(_) => self.form_key(key.code),
        }
    }

    fn normal_key(&mut self, code: KeyCode, now: Instant) -> Vec<Effect> {
        match code {
            KeyCode::Char('q') => vec![Effect::Quit],
            KeyCode::Left => {
                self.opts.recent += 1;
                vec![Effect::Relist]
            }
            KeyCode::Right if self.opts.recent > 1 => {
                self.opts.recent -= 1;
                vec![Effect::Relist]
            }
            KeyCode::Char('d') => {
                self.opts.dates = !self.opts.dates;
                vec![Effect::Relist]
            }
            KeyCode::Char('l' | 'L') => {
                self.opts.labs = !self.opts.labs;
                vec![Effect::Relist]
            }
            KeyCode::Char('s') => {
                self.opts.stale = !self.opts.stale;
                vec![Effect::Relist]
            }
            KeyCode::Down => self.step(true),
            KeyCode::Up => self.step(false),
            KeyCode::Tab | KeyCode::BackTab => self.hop(),
            KeyCode::Esc => {
                self.hide();
                self.message = None;
                vec![]
            }
            KeyCode::Enter => self.open_peek(),
            KeyCode::Char('e') => self.open_editor(now),
            KeyCode::Char('m') => self.open_move(),
            KeyCode::Char('p') => self.open_prio(now),
            KeyCode::Char('n') => self.open_form(),
            _ => vec![],
        }
    }

    fn hide(&mut self) {
        self.cursor = None;
        for c in &mut self.columns {
            c.offset = 0;
        }
    }

    /// Up/Down. The first press reveals the cursor on the primary's first task.
    fn step(&mut self, down: bool) -> Vec<Effect> {
        let Some(c) = self.cursor.clone() else {
            self.reveal(0);
            return vec![];
        };
        let rows = self.task_rows(c.col);
        let next = if down {
            rows.iter().copied().find(|&i| i > c.row)
        } else {
            rows.iter().copied().rev().find(|&i| i < c.row)
        };
        if let Some(row) = next {
            self.cursor = Some(self.cursor_at(c.col, row));
        }
        vec![]
    }

    /// Show the cursor on a column's first task, else on the other column's.
    fn reveal(&mut self, col: usize) {
        let order = [col, 1 - col.min(1)];
        for col in order {
            if let Some(&row) = self.task_rows(col).first() {
                self.cursor = Some(self.cursor_at(col, row));
                return;
            }
        }
    }

    /// Tab: to the other column, on the task row nearest the same screen row.
    fn hop(&mut self) -> Vec<Effect> {
        if self.columns.len() < 2 {
            return vec![];
        }
        let Some(c) = self.cursor.clone() else {
            self.reveal(1);
            return vec![];
        };
        let other = 1 - c.col;
        let screen_row = c.row.saturating_sub(self.columns[c.col].offset);
        if let Some(row) = self.nearest(other, screen_row) {
            self.columns[c.col].offset = 0;
            self.cursor = Some(self.cursor_at(other, row));
        }
        vec![]
    }

    fn open_peek(&mut self) -> Vec<Effect> {
        let Some((col, task)) = self.cursor_task() else {
            return vec![];
        };
        let (key, path) = (task.key(), self.task_path(col, task));
        self.mode = Mode::Peek(Peek {
            col,
            key,
            text: fs::read_to_string(&path).ok(),
            stamp: watch::stamp(&path),
            path,
            scroll: 0,
        });
        vec![]
    }

    fn open_editor(&mut self, now: Instant) -> Vec<Effect> {
        let Some((col, task)) = self.cursor_task() else {
            return vec![];
        };
        let (name, slug, path) = (task.name(), task.slug.clone(), self.task_path(col, task));
        let Some(editor) = self.editor.clone() else {
            self.error(
                "no editor: set $VISUAL or $EDITOR, e.g. export EDITOR=micro",
                now,
            );
            return vec![];
        };
        let before = fs::read(&path).ok();
        vec![Effect::Edit(Edit {
            editor,
            path,
            col,
            name,
            slug,
            before,
        })]
    }

    fn open_move(&mut self) -> Vec<Effect> {
        let Some((col, task)) = self.cursor_task() else {
            return vec![];
        };
        let worker = read_worker(&self.task_path(col, task));
        self.mode = Mode::Move {
            task: task.clone(),
            worker,
        };
        vec![]
    }

    fn open_prio(&mut self, now: Instant) -> Vec<Effect> {
        let Some((col, task)) = self.cursor_task() else {
            return vec![];
        };
        if task.status.is_closed() {
            let text = format!(
                "{} is {}: only an open task takes a new priority",
                task.name(),
                task.status.dir()
            );
            self.info(text, now);
            return vec![];
        }
        let worker = read_worker(&self.task_path(col, task));
        self.mode = Mode::Prio {
            task: task.clone(),
            worker,
        };
        vec![]
    }

    fn open_form(&mut self) -> Vec<Effect> {
        let col = self.cursor.as_ref().map_or(0, |c| c.col);
        self.mode = Mode::Form(Form {
            col,
            letter: 'N',
            title: vec![],
            pos: 0,
            in_prio: false,
        });
        vec![]
    }

    fn peek_key(&mut self, code: KeyCode) -> Vec<Effect> {
        let page = self.peek_rows.saturating_sub(1).max(1);
        let Mode::Peek(p) = &mut self.mode else {
            return vec![];
        };
        match code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.mode = Mode::Normal,
            KeyCode::Down => p.scroll += 1,
            KeyCode::Up => p.scroll = p.scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => p.scroll += page,
            KeyCode::PageUp => p.scroll = p.scroll.saturating_sub(page),
            KeyCode::Home => p.scroll = 0,
            KeyCode::End => p.scroll = usize::MAX,
            _ => {}
        }
        vec![]
    }

    fn move_key(&mut self, code: KeyCode) -> Vec<Effect> {
        let to = match code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                return vec![];
            }
            KeyCode::Char(c) => match MOVES.iter().find(|(k, _)| *k == c) {
                Some(&(_, s)) => s,
                None => return vec![],
            },
            _ => return vec![],
        };
        let Mode::Move { task, .. } = &self.mode else {
            return vec![];
        };
        // A target the script would refuse is inert.
        if !task.status.can_move_to(to) {
            return vec![];
        }
        let task = task.clone();
        self.mode = Mode::Normal;
        let Some(c) = self.cursor.clone() else {
            return vec![];
        };
        let call = Call::Move {
            project: self.columns[c.col].project.clone(),
            file: task.filename(),
            to,
        };
        let done = format!("moved {} to {}", task.name(), to.dir());
        // Queued first: the successor skips pending rows, this one included.
        let effects = self.queue(Job {
            call,
            col: c.col,
            key: Some(task.key()),
            done,
        });
        if let Some(row) = self.successor(c.col, c.row) {
            self.cursor = Some(self.cursor_at(c.col, row));
        }
        effects
    }

    fn prio_key(&mut self, code: KeyCode, now: Instant) -> Vec<Effect> {
        let letter = match code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                return vec![];
            }
            KeyCode::Char(c) if c.is_ascii_alphabetic() => c.to_ascii_uppercase(),
            _ => return vec![],
        };
        let Mode::Prio { task, .. } = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return vec![];
        };
        let Some(col) = self.cursor.as_ref().map(|c| c.col) else {
            return vec![];
        };
        if letter == task.letter {
            self.info(format!("{} is already at {letter}", task.name()), now);
            return vec![];
        }
        let call = Call::Reprioritize {
            project: self.columns[col].project.clone(),
            file: task.filename(),
            letter,
        };
        let done = format!("{} is now {letter}{}", task.name(), task.id);
        self.queue(Job {
            call,
            col,
            key: Some(task.key()),
            done,
        })
    }

    fn form_key(&mut self, code: KeyCode) -> Vec<Effect> {
        let columns = self.columns.len();
        let Mode::Form(f) = &mut self.mode else {
            return vec![];
        };
        match code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let title: String = f.title.iter().collect::<String>().trim().to_string();
                if title.is_empty() {
                    return vec![];
                }
                let (col, letter) = (f.col, f.letter);
                self.mode = Mode::Normal;
                let project = self.columns[col].project.clone();
                let call = Call::Create {
                    project,
                    letter,
                    title,
                };
                return self.queue(Job {
                    call,
                    col,
                    key: None,
                    done: String::new(),
                });
            }
            KeyCode::Tab | KeyCode::BackTab if columns > 1 => f.col = 1 - f.col,
            KeyCode::Char(c) if f.in_prio && c.is_ascii_alphabetic() => {
                f.letter = c.to_ascii_uppercase();
            }
            KeyCode::Right if f.in_prio => {
                f.in_prio = false;
                f.pos = 0;
            }
            _ if f.in_prio => {}
            KeyCode::Char(c) => {
                f.title.insert(f.pos, c);
                f.pos += 1;
            }
            KeyCode::Backspace if f.pos > 0 => {
                f.pos -= 1;
                f.title.remove(f.pos);
            }
            KeyCode::Delete if f.pos < f.title.len() => {
                f.title.remove(f.pos);
            }
            KeyCode::Home => f.pos = 0,
            KeyCode::End => f.pos = f.title.len(),
            KeyCode::Left if f.pos == 0 => f.in_prio = true,
            KeyCode::Left => f.pos -= 1,
            KeyCode::Right if f.pos < f.title.len() => f.pos += 1,
            _ => {}
        }
        vec![]
    }

    fn queue(&mut self, job: Job) -> Vec<Effect> {
        self.in_flight += 1;
        if let Some(key) = &job.key {
            self.pending.push((job.col, key.clone()));
        }
        vec![Effect::Run(job)]
    }

    // --- results -----------------------------------------------------------

    pub fn on_job_done(
        &mut self,
        job: Job,
        result: Result<String, String>,
        now: Instant,
    ) -> Vec<Effect> {
        self.in_flight = self.in_flight.saturating_sub(1);
        if let Some(key) = &job.key
            && let Some(i) = self
                .pending
                .iter()
                .position(|(c, k)| *c == job.col && k == key)
        {
            self.pending.remove(i);
        }
        match result {
            Ok(out) => {
                let mut done = job.done;
                if let Call::Create { .. } = job.call {
                    let file = out
                        .lines()
                        .map(str::trim)
                        .rfind(|l| !l.is_empty())
                        .unwrap_or("");
                    if let Some(key) = TaskKey::from_filename(file) {
                        self.focus = Some((job.col, key));
                    }
                    done = format!("created {}", file.strip_suffix(".md").unwrap_or(file));
                }
                self.info(done, now);
            }
            Err(e) => self.error(e, now),
        }
        vec![Effect::Relist]
    }

    /// Back from the editor: commit a changed file, unless the task moved
    /// while it was open.
    pub fn after_edit(
        &mut self,
        edit: Edit,
        status: io::Result<ExitStatus>,
        now: Instant,
    ) -> Vec<Effect> {
        let Some(name) = edit
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
        else {
            return vec![];
        };
        let project_dir = self.tasks_root.join(&self.columns[edit.col].project);
        let holders: Vec<&str> = Status::ALL
            .iter()
            .map(|s| s.dir())
            .filter(|d| project_dir.join(d).join(&name).is_file())
            .collect();
        if !edit.path.is_file() {
            self.error(
                format!("{} moved while open: edit not committed", edit.name),
                now,
            );
            return vec![];
        }
        if holders.len() > 1 {
            let dirs = holders
                .iter()
                .map(|d| format!("{d}/"))
                .collect::<Vec<_>>()
                .join(" and ");
            self.error(
                format!("{} is now in {dirs}: edit not committed", edit.name),
                now,
            );
            return vec![];
        }
        if fs::read(&edit.path).ok() != edit.before {
            let call = Call::Commit {
                message: format!("Edit: {}", edit.slug),
            };
            let done = format!("committed edit of {}", edit.name);
            return self.queue(Job {
                call,
                col: edit.col,
                key: None,
                done,
            });
        }
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => self.error(format!("{} exited with {s}", edit.editor), now),
            Err(e) => self.error(format!("cannot run {}: {e}", edit.editor), now),
        }
        vec![]
    }

    // --- time --------------------------------------------------------------

    /// The cursor hides after a minute without a key, restoring the idle
    /// frame; a peek or a half-typed form stays until dismissed.
    pub fn tick(&mut self, now: Instant) {
        let row_bound = matches!(
            self.mode,
            Mode::Normal | Mode::Move { .. } | Mode::Prio { .. }
        );
        if self.cursor.is_some() && row_bound && now.duration_since(self.last_key) >= CURSOR_TIMEOUT
        {
            self.hide();
            self.mode = Mode::Normal;
        }
        if self.message.as_ref().is_some_and(|m| now >= m.until) {
            self.message = None;
        }
    }

    /// Re-read the peeked file when it changed on disk, following the task
    /// when a move or a reprioritize renamed it.
    pub fn refresh_peek(&mut self) {
        let project_dir = match &self.mode {
            Mode::Peek(p) => self.tasks_root.join(&self.columns[p.col].project),
            _ => return,
        };
        let Mode::Peek(p) = &mut self.mode else {
            return;
        };
        if !p.path.is_file()
            && let Some(path) = locate(&project_dir, &p.key)
        {
            p.path = path;
        }
        let stamp = watch::stamp(&p.path);
        if stamp != p.stamp {
            p.stamp = stamp;
            p.text = fs::read_to_string(&p.path).ok();
        }
    }

    fn info(&mut self, text: impl Into<String>, now: Instant) {
        // An unread error outranks news of a later success.
        if self
            .message
            .as_ref()
            .is_some_and(|m| m.error && now < m.until)
        {
            return;
        }
        self.message = Some(Message {
            text: text.into(),
            error: false,
            until: now + INFO_FOR,
        });
    }

    fn error(&mut self, text: impl Into<String>, now: Instant) {
        self.message = Some(Message {
            text: text.into(),
            error: true,
            until: now + ERROR_FOR,
        });
    }
}

#[cfg(test)]
impl Message {
    pub fn test(text: &str, error: bool) -> Message {
        Message {
            text: text.into(),
            error,
            until: Instant::now() + ERROR_FOR,
        }
    }
}

/// The move menu's keys, in the order the bar shows them.
pub const MOVES: [(char, Status); 5] = [
    ('t', Status::Todo),
    ('p', Status::Planned),
    ('s', Status::Stale),
    ('d', Status::Done),
    ('c', Status::Canceled),
];

/// The task file's `Worker:` stamp, the lab holding it.
fn read_worker(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("Worker: "))
        .map(|w| w.trim().to_string())
}

/// A task's file wherever it now sits in the project, by id and slug.
fn locate(project_dir: &Path, key: &TaskKey) -> Option<PathBuf> {
    Status::ALL.iter().find_map(|s| {
        fs::read_dir(project_dir.join(s.dir()))
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .and_then(TaskKey::from_filename)
                    .as_ref()
                    == Some(key)
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{PROJ, TempDir, WIDGET, app, key};
    use std::os::unix::process::ExitStatusExt;

    fn chars(a: &mut App, s: &str) {
        for c in s.chars() {
            key(a, KeyCode::Char(c));
        }
    }

    fn on(a: &App) -> Option<String> {
        a.cursor_task()
            .map(|(col, t)| format!("{col}:{}", t.name()))
    }

    fn jobs(effects: Vec<Effect>) -> Vec<Call> {
        effects
            .into_iter()
            .filter_map(|e| {
                if let Effect::Run(j) = e {
                    Some(j.call)
                } else {
                    None
                }
            })
            .collect()
    }

    /// A tasks root on disk holding the files the PROJ listing names, and the
    /// first of WIDGET's.
    fn on_disk(a: &mut App) -> TempDir {
        let tmp = TempDir::new();
        for (project, dir, file, body) in [
            ("proj", "active", "H010-busy.md", "# Busy\nWorker: hlab-7\n\nbody\n"),
            ("proj", "todo", "N001-alpha.md", "# Alpha\n\nfirst line\n"),
            ("proj", "todo", "N002-beta.md", ""),
            ("proj", "todo", "N003-gamma.md", "# Gamma\n"),
            ("proj", "done", "N050-shipped.md", "# Shipped\n"),
            ("widget", "todo", "N007-left-pad.md", "# Pad\nWorker: hlab-9\n"),
        ] {
            let d = tmp.path().join(project).join(dir);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join(file), body).unwrap();
        }
        a.tasks_root = tmp.path().to_path_buf();
        tmp
    }

    #[test]
    fn the_cursor_starts_hidden_and_the_first_press_reveals_it() {
        let mut a = app(false);
        assert_eq!(on(&a), None);
        key(&mut a, KeyCode::Up);
        assert_eq!(on(&a), Some("0:H010".into()));
        key(&mut a, KeyCode::Esc);
        key(&mut a, KeyCode::Down);
        assert_eq!(on(&a), Some("0:H010".into()));
    }

    #[test]
    fn the_cursor_lands_only_on_task_rows_tail_included() {
        let mut a = app(false);
        let mut seen = vec![];
        for _ in 0..8 {
            key(&mut a, KeyCode::Down);
            seen.push(on(&a).unwrap());
        }
        seen.dedup();
        assert_eq!(seen, ["0:H010", "0:N001", "0:N002", "0:N003", "0:N050"]);
        for _ in 0..8 {
            key(&mut a, KeyCode::Up);
        }
        assert_eq!(on(&a), Some("0:H010".into()));
    }

    #[test]
    fn esc_hides_the_cursor() {
        let mut a = app(false);
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Esc);
        assert_eq!(a.cursor, None);
    }

    #[test]
    fn the_cursor_hides_after_a_minute_without_a_key() {
        let mut a = app(false);
        let t0 = Instant::now();
        a.on_key(KeyEvent::from(KeyCode::Down), t0);
        a.tick(t0 + Duration::from_secs(59));
        assert!(a.cursor.is_some());
        a.on_key(KeyEvent::from(KeyCode::Down), t0 + Duration::from_secs(59));
        a.tick(t0 + Duration::from_secs(100));
        assert!(a.cursor.is_some(), "a key restarts the minute");
        a.tick(t0 + Duration::from_secs(119));
        assert_eq!(a.cursor, None);
    }

    #[test]
    fn tab_hops_columns_and_reveals_in_the_second() {
        let mut a = app(true);
        key(&mut a, KeyCode::Tab);
        assert_eq!(on(&a), Some("1:N007".into()));
        key(&mut a, KeyCode::Tab);
        assert_eq!(
            on(&a),
            Some("0:H010".into()),
            "row 1 of the second is beside row 1 of the first"
        );
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Tab);
        assert_eq!(on(&a), Some("1:N008".into()));
        let mut solo = app(false);
        key(&mut solo, KeyCode::Tab);
        assert_eq!(solo.cursor, None);
    }

    #[test]
    fn row_keys_never_act_without_a_cursor() {
        let mut a = app(false);
        for code in [
            KeyCode::Enter,
            KeyCode::Char('e'),
            KeyCode::Char('m'),
            KeyCode::Char('p'),
        ] {
            assert_eq!(key(&mut a, code), vec![]);
            assert!(matches!(a.mode, Mode::Normal));
        }
    }

    #[test]
    fn the_bash_watch_keys_still_work() {
        let mut a = app(false);
        assert_eq!(key(&mut a, KeyCode::Left), vec![Effect::Relist]);
        assert_eq!(a.opts.recent, 8);
        key(&mut a, KeyCode::Right);
        key(&mut a, KeyCode::Right);
        assert_eq!(a.opts.recent, 6);
        a.opts.recent = 1;
        assert_eq!(key(&mut a, KeyCode::Right), vec![]);
        assert_eq!(a.opts.recent, 1);
        key(&mut a, KeyCode::Char('d'));
        key(&mut a, KeyCode::Char('L'));
        key(&mut a, KeyCode::Char('s'));
        assert!(a.opts.dates && a.opts.labs && a.opts.stale);
        key(&mut a, KeyCode::Char('l'));
        assert!(!a.opts.labs);
        assert_eq!(key(&mut a, KeyCode::Char('q')), vec![Effect::Quit]);
    }

    #[test]
    fn alt_keys_are_tmuxs_and_ctrl_c_quits() {
        let mut a = app(false);
        let alt_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT);
        assert_eq!(a.on_key(alt_q, Instant::now()), vec![]);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(a.on_key(ctrl_c, Instant::now()), vec![Effect::Quit]);
    }

    #[test]
    fn the_cursor_follows_its_task_across_a_reprioritize() {
        let mut a = app(false);
        for _ in 0..3 {
            key(&mut a, KeyCode::Down);
        }
        assert_eq!(on(&a), Some("0:N002".into()));
        let resorted = "ACTIVE (1)\n  H010 busy\nTODO (3)\n  A002 beta\n  N001 alpha\n  N003 gamma";
        a.set_listings(vec![resorted.into()]);
        assert_eq!(on(&a), Some("0:A002".into()));
        assert_eq!(a.cursor.as_ref().unwrap().row, 3);
    }

    #[test]
    fn a_cursor_whose_task_left_the_view_stays_on_its_row() {
        let mut a = app(false);
        for _ in 0..3 {
            key(&mut a, KeyCode::Down);
        }
        assert_eq!(on(&a), Some("0:N002".into()));
        a.set_listings(vec![PROJ.replace("  N002 beta\n", "")]);
        assert_eq!(on(&a), Some("0:N003".into()));
        a.set_listings(vec!["(none)".into()]);
        assert_eq!(a.cursor, None);
    }

    #[test]
    fn a_move_offers_only_what_the_script_takes() {
        let mut a = app(false);
        let _tmp = on_disk(&mut a);
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Down); // N001, todo
        key(&mut a, KeyCode::Char('m'));
        assert!(matches!(a.mode, Mode::Move { .. }));
        assert_eq!(
            key(&mut a, KeyCode::Char('s')),
            vec![],
            "stale is only for active tasks"
        );
        assert_eq!(key(&mut a, KeyCode::Char('t')), vec![], "already in todo");
        assert_eq!(key(&mut a, KeyCode::Char('x')), vec![]);
        assert!(matches!(a.mode, Mode::Move { .. }));
        key(&mut a, KeyCode::Esc);
        assert!(matches!(a.mode, Mode::Normal));
        assert_eq!(on(&a), Some("0:N001".into()));
    }

    #[test]
    fn a_move_runs_task_move_and_slides_the_cursor_to_the_next_row() {
        let mut a = app(false);
        let _tmp = on_disk(&mut a);
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Down); // N001
        key(&mut a, KeyCode::Char('m'));
        let calls = jobs(key(&mut a, KeyCode::Char('d')));
        assert_eq!(
            calls,
            vec![Call::Move {
                project: "proj".into(),
                file: "N001-alpha.md".into(),
                to: Status::Done
            }]
        );
        assert!(matches!(a.mode, Mode::Normal));
        assert_eq!(
            on(&a),
            Some("0:N002".into()),
            "triage is repeated keystrokes"
        );
        assert_eq!(a.in_flight, 1);
        assert!(a.is_pending(
            0,
            &TaskKey {
                id: "001".into(),
                slug: "alpha".into()
            }
        ));

        // The same keystrokes again act on the next task.
        key(&mut a, KeyCode::Char('m'));
        let calls = jobs(key(&mut a, KeyCode::Char('c')));
        assert_eq!(
            calls,
            vec![Call::Move {
                project: "proj".into(),
                file: "N002-beta.md".into(),
                to: Status::Canceled
            }]
        );
        assert_eq!(on(&a), Some("0:N003".into()));

        // Its section moved away, the next row below takes the cursor.
        key(&mut a, KeyCode::Char('m'));
        key(&mut a, KeyCode::Char('d'));
        assert_eq!(on(&a), Some("0:N050".into()));
    }

    #[test]
    fn moving_the_last_of_a_section_lands_on_the_one_above() {
        let mut a = app(false);
        let _tmp = on_disk(&mut a);
        for _ in 0..4 {
            key(&mut a, KeyCode::Down); // N003
        }
        key(&mut a, KeyCode::Char('m'));
        key(&mut a, KeyCode::Char('d'));
        assert_eq!(on(&a), Some("0:N002".into()));
    }

    #[test]
    fn after_the_move_lands_the_cursor_stays_on_the_row_that_slid_in() {
        let mut a = app(false);
        let _tmp = on_disk(&mut a);
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Down); // N001
        let row = a.cursor.as_ref().unwrap().row;
        key(&mut a, KeyCode::Char('m'));
        let effects = key(&mut a, KeyCode::Char('d'));
        let Some(Effect::Run(job)) = effects.into_iter().next() else {
            panic!("no job")
        };
        assert_eq!(
            a.on_job_done(job, Ok(String::new()), Instant::now()),
            vec![Effect::Relist]
        );
        assert_eq!(a.in_flight, 0);
        assert!(a.pending.is_empty());
        a.set_listings(vec![
            PROJ.replace("  N001 alpha\n", "")
                .replace("  N050 shipped", "  N001 alpha\n  N050 shipped"),
        ]);
        assert_eq!(on(&a), Some("0:N002".into()));
        assert_eq!(a.cursor.as_ref().unwrap().row, row);
        assert_eq!(
            a.message.as_ref().map(|m| m.text.as_str()),
            Some("moved N001 to done")
        );
    }

    #[test]
    fn an_active_task_can_go_stale_and_the_menu_names_its_worker() {
        let mut a = app(false);
        let _tmp = on_disk(&mut a);
        key(&mut a, KeyCode::Down); // H010, active, Worker: hlab-7
        key(&mut a, KeyCode::Char('m'));
        let Mode::Move { worker, .. } = &a.mode else {
            panic!("no menu")
        };
        assert_eq!(worker.as_deref(), Some("hlab-7"));
        let calls = jobs(key(&mut a, KeyCode::Char('s')));
        assert_eq!(
            calls,
            vec![Call::Move {
                project: "proj".into(),
                file: "H010-busy.md".into(),
                to: Status::Stale
            }]
        );
    }

    #[test]
    fn a_menu_closes_when_its_task_leaves_the_cursor() {
        let mut a = app(false);
        let _tmp = on_disk(&mut a);
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Down); // N001
        key(&mut a, KeyCode::Char('m'));
        a.set_listings(vec![PROJ.replace("  N001 alpha\n", "")]);
        assert!(matches!(a.mode, Mode::Normal));
        assert_eq!(
            key(&mut a, KeyCode::Char('d')),
            vec![Effect::Relist],
            "d is the dates toggle again"
        );
    }

    #[test]
    fn a_failed_job_shows_the_scripts_last_line() {
        let mut a = app(false);
        let _tmp = on_disk(&mut a);
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Char('m'));
        let Some(Effect::Run(job)) = key(&mut a, KeyCode::Char('d')).into_iter().next() else {
            panic!()
        };
        let now = Instant::now();
        a.on_job_done(job.clone(), Err("error: push rejected".into()), now);
        let m = a.message.as_ref().unwrap();
        assert!(m.error);
        assert_eq!(m.text, "error: push rejected");
        // A later success does not bury it.
        a.in_flight = 1;
        a.on_job_done(job, Ok(String::new()), now);
        assert_eq!(a.message.as_ref().unwrap().text, "error: push rejected");
        a.tick(now + ERROR_FOR);
        assert!(a.message.is_none());
    }

    #[test]
    fn priority_takes_any_letter_and_refuses_closed_tasks() {
        let mut a = app(false);
        let _tmp = on_disk(&mut a);
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Down); // N001
        key(&mut a, KeyCode::Char('p'));
        assert!(matches!(a.mode, Mode::Prio { .. }));
        assert_eq!(key(&mut a, KeyCode::Char('1')), vec![]);
        let calls = jobs(key(&mut a, KeyCode::Char('h')));
        assert_eq!(
            calls,
            vec![Call::Reprioritize {
                project: "proj".into(),
                file: "N001-alpha.md".into(),
                letter: 'H'
            }]
        );
        assert!(matches!(a.mode, Mode::Normal));

        key(&mut a, KeyCode::Char('p'));
        key(&mut a, KeyCode::Esc);
        assert!(matches!(a.mode, Mode::Normal));

        key(&mut a, KeyCode::Char('p'));
        assert_eq!(jobs(key(&mut a, KeyCode::Char('n'))), vec![], "already N");

        for _ in 0..5 {
            key(&mut a, KeyCode::Down); // N050, done
        }
        key(&mut a, KeyCode::Char('p'));
        assert!(matches!(a.mode, Mode::Normal));
        assert!(a.message.as_ref().unwrap().text.contains("N050 is done"));
    }

    #[test]
    fn the_second_columns_task_is_acted_on_in_its_project() {
        let mut a = app(true);
        let _tmp = on_disk(&mut a);
        key(&mut a, KeyCode::Tab); // N007, widget
        key(&mut a, KeyCode::Char('p'));
        let Mode::Prio { worker, .. } = &a.mode else {
            panic!("no priority prompt")
        };
        assert_eq!(worker.as_deref(), Some("hlab-9"));
        assert_eq!(
            jobs(key(&mut a, KeyCode::Char('h'))),
            vec![Call::Reprioritize {
                project: "widget".into(),
                file: "N007-left-pad.md".into(),
                letter: 'H'
            }]
        );
        key(&mut a, KeyCode::Char('m'));
        let Mode::Move { worker, .. } = &a.mode else {
            panic!("no move menu")
        };
        assert_eq!(worker.as_deref(), Some("hlab-9"));
        assert_eq!(
            jobs(key(&mut a, KeyCode::Char('d'))),
            vec![Call::Move {
                project: "widget".into(),
                file: "N007-left-pad.md".into(),
                to: Status::Done
            }]
        );
    }

    #[test]
    fn the_timeout_closes_a_menu_but_not_a_form_or_a_peek() {
        let t0 = Instant::now();
        let later = t0 + CURSOR_TIMEOUT;
        for (open, survives) in [('m', false), ('p', false), ('n', true)] {
            let mut a = app(false);
            let _tmp = on_disk(&mut a);
            a.on_key(KeyEvent::from(KeyCode::Down), t0);
            a.on_key(KeyEvent::from(KeyCode::Down), t0); // N001
            a.on_key(KeyEvent::from(KeyCode::Char(open)), t0);
            a.tick(later);
            assert_eq!(!matches!(a.mode, Mode::Normal), survives, "{open}");
        }
        let mut a = app(false);
        let _tmp = on_disk(&mut a);
        a.on_key(KeyEvent::from(KeyCode::Down), t0);
        a.on_key(KeyEvent::from(KeyCode::Enter), t0);
        a.tick(later);
        assert!(matches!(a.mode, Mode::Peek(_)));
    }

    #[test]
    fn the_form_creates_in_the_cursors_column() {
        let mut a = app(true);
        key(&mut a, KeyCode::Char('n'));
        let Mode::Form(f) = &a.mode else {
            panic!("no form")
        };
        assert_eq!((f.col, f.letter, f.in_prio), (0, 'N', false));
        chars(&mut a, "fix the sensor");
        let calls = jobs(key(&mut a, KeyCode::Enter));
        assert_eq!(
            calls,
            vec![Call::Create {
                project: "proj".into(),
                letter: 'N',
                title: "fix the sensor".into()
            }]
        );

        key(&mut a, KeyCode::Tab); // cursor into the second column
        key(&mut a, KeyCode::Char('n'));
        chars(&mut a, "x");
        let calls = jobs(key(&mut a, KeyCode::Enter));
        assert_eq!(
            calls,
            vec![Call::Create {
                project: "widget".into(),
                letter: 'N',
                title: "x".into()
            }]
        );
    }

    #[test]
    fn the_form_edits_its_title_and_letter() {
        let mut a = app(true);
        key(&mut a, KeyCode::Char('n'));
        chars(&mut a, "ac");
        key(&mut a, KeyCode::Left);
        chars(&mut a, "b"); // abc
        key(&mut a, KeyCode::Home);
        key(&mut a, KeyCode::Delete); // bc
        key(&mut a, KeyCode::End);
        key(&mut a, KeyCode::Backspace); // b
        chars(&mut a, "ee"); // bee
        key(&mut a, KeyCode::Home);
        key(&mut a, KeyCode::Left); // into prio
        chars(&mut a, "x1"); // X; the digit does nothing there
        key(&mut a, KeyCode::Right); // back to the title's start
        chars(&mut a, "a "); // a bee
        key(&mut a, KeyCode::Tab); // to widget
        let Mode::Form(f) = &a.mode else { panic!() };
        assert_eq!(
            (f.title.iter().collect::<String>(), f.letter, f.pos, f.col),
            ("a bee".into(), 'X', 2, 1)
        );
        let calls = jobs(key(&mut a, KeyCode::Enter));
        assert_eq!(
            calls,
            vec![Call::Create {
                project: "widget".into(),
                letter: 'X',
                title: "a bee".into()
            }]
        );
    }

    #[test]
    fn a_blank_title_submits_nothing_and_esc_cancels() {
        let mut a = app(false);
        key(&mut a, KeyCode::Char('n'));
        chars(&mut a, "  ");
        assert_eq!(key(&mut a, KeyCode::Enter), vec![]);
        assert!(matches!(a.mode, Mode::Form(_)));
        key(&mut a, KeyCode::Tab); // no second project: nothing to flip to
        let Mode::Form(f) = &a.mode else { panic!() };
        assert_eq!(f.col, 0);
        key(&mut a, KeyCode::Esc);
        assert!(matches!(a.mode, Mode::Normal));
    }

    #[test]
    fn letters_in_the_form_are_text_not_commands() {
        let mut a = app(false);
        key(&mut a, KeyCode::Char('n'));
        assert_eq!(key(&mut a, KeyCode::Char('q')), vec![]);
        let Mode::Form(f) = &a.mode else { panic!() };
        assert_eq!(f.title, vec!['q']);
    }

    #[test]
    fn a_created_task_gets_the_cursor() {
        let mut a = app(true);
        key(&mut a, KeyCode::Char('n'));
        key(&mut a, KeyCode::Tab);
        chars(&mut a, "new one");
        let Some(Effect::Run(job)) = key(&mut a, KeyCode::Enter).into_iter().next() else {
            panic!()
        };
        a.on_job_done(
            job,
            Ok("[main 1a2b3c] Add: new-one\nN009-new-one.md\n".into()),
            Instant::now(),
        );
        assert_eq!(a.message.as_ref().unwrap().text, "created N009-new-one");
        a.set_listings(vec![PROJ.into(), format!("{WIDGET}\n  N009 new-one")]);
        assert_eq!(on(&a), Some("1:N009".into()));
    }

    #[test]
    fn peek_opens_scrolls_and_closes() {
        let mut a = app(false);
        let _tmp = on_disk(&mut a);
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Enter);
        let Mode::Peek(p) = &a.mode else {
            panic!("no peek")
        };
        assert_eq!(p.text.as_deref(), Some("# Busy\nWorker: hlab-7\n\nbody\n"));
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Up);
        let Mode::Peek(p) = &a.mode else { panic!() };
        assert_eq!(p.scroll, 1);
        assert_eq!(key(&mut a, KeyCode::Char('m')), vec![]);
        assert!(
            matches!(a.mode, Mode::Peek(_)),
            "row keys are off while peeking"
        );
        for close in [KeyCode::Esc, KeyCode::Enter, KeyCode::Char('q')] {
            key(&mut a, close);
            assert!(matches!(a.mode, Mode::Normal));
            key(&mut a, KeyCode::Enter);
            assert!(matches!(a.mode, Mode::Peek(_)));
        }
    }

    #[test]
    fn peek_rereads_a_changed_file_and_follows_a_moved_one() {
        let mut a = app(false);
        let tmp = on_disk(&mut a);
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Down); // N001
        key(&mut a, KeyCode::Enter);
        let todo = tmp.path().join("proj/todo/N001-alpha.md");
        fs::write(&todo, "# Alpha, a good deal longer now\n").unwrap();
        a.refresh_peek();
        let Mode::Peek(p) = &a.mode else { panic!() };
        assert_eq!(p.text.as_deref(), Some("# Alpha, a good deal longer now\n"));

        fs::rename(&todo, tmp.path().join("proj/done/H001-alpha.md")).unwrap();
        a.refresh_peek();
        let Mode::Peek(p) = &a.mode else { panic!() };
        assert_eq!(p.path, tmp.path().join("proj/done/H001-alpha.md"));

        fs::remove_file(&p.path).unwrap();
        a.refresh_peek();
        let Mode::Peek(p) = &a.mode else { panic!() };
        assert_eq!(p.text, None);
    }

    #[test]
    fn editing_needs_an_editor() {
        let mut a = app(false);
        a.editor = None;
        key(&mut a, KeyCode::Down);
        assert_eq!(key(&mut a, KeyCode::Char('e')), vec![]);
        let m = a.message.as_ref().unwrap();
        assert!(
            m.error && m.text.contains("export EDITOR=micro"),
            "{}",
            m.text
        );
    }

    fn edit_of(a: &mut App) -> Edit {
        match key(a, KeyCode::Char('e')).into_iter().next() {
            Some(Effect::Edit(e)) => e,
            other => panic!("no edit: {other:?}"),
        }
    }

    #[test]
    fn an_edit_that_changed_the_file_is_committed() {
        let mut a = app(false);
        let tmp = on_disk(&mut a);
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Down); // N001
        let edit = edit_of(&mut a);
        assert_eq!(
            (edit.editor.as_str(), edit.path.clone()),
            ("micro", tmp.path().join("proj/todo/N001-alpha.md"))
        );
        fs::write(&edit.path, "# Alpha\n\nedited\n").unwrap();
        let calls = jobs(a.after_edit(edit, Ok(ExitStatus::from_raw(0)), Instant::now()));
        assert_eq!(
            calls,
            vec![Call::Commit {
                message: "Edit: alpha".into()
            }]
        );
    }

    #[test]
    fn an_unchanged_edit_commits_nothing() {
        let mut a = app(false);
        let _tmp = on_disk(&mut a);
        key(&mut a, KeyCode::Down);
        let edit = edit_of(&mut a);
        assert_eq!(
            a.after_edit(edit, Ok(ExitStatus::from_raw(0)), Instant::now()),
            vec![]
        );
        assert!(a.message.is_none());
    }

    #[test]
    fn a_failed_editor_says_so() {
        let mut a = app(false);
        let _tmp = on_disk(&mut a);
        key(&mut a, KeyCode::Down);
        let edit = edit_of(&mut a);
        a.after_edit(edit.clone(), Ok(ExitStatus::from_raw(1 << 8)), Instant::now());
        assert!(a.message.as_ref().unwrap().text.contains("micro exited"));
        let gone = io::Error::from(io::ErrorKind::NotFound);
        a.after_edit(edit, Err(gone), Instant::now());
        assert!(a.message.as_ref().unwrap().text.contains("cannot run micro"));
    }

    #[test]
    fn an_edit_of_a_task_moved_while_open_commits_nothing() {
        let mut a = app(false);
        let tmp = on_disk(&mut a);
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Down); // N001
        let edit = edit_of(&mut a);
        let done = tmp.path().join("proj/done/N001-alpha.md");
        fs::rename(&edit.path, &done).unwrap();
        assert_eq!(
            a.after_edit(edit.clone(), Ok(ExitStatus::from_raw(0)), Instant::now()),
            vec![]
        );
        assert!(
            a.message
                .as_ref()
                .unwrap()
                .text
                .contains("moved while open")
        );

        // The editor wrote its buffer back to the old path: now in two places.
        fs::write(&edit.path, "# Alpha\n\nedited\n").unwrap();
        assert_eq!(
            a.after_edit(edit, Ok(ExitStatus::from_raw(0)), Instant::now()),
            vec![]
        );
        assert!(
            a.message.as_ref().unwrap().text.contains("todo/ and done/"),
            "{}",
            a.message.as_ref().unwrap().text
        );
    }
}
