//! What a project holds, row by row: the listing `taskania list` prints and the
//! dashboard draws. A `LABEL (count)` header per status, then a row per task:
//! two spaces, `<Letter><NNN>`, a space, the slug, then optionally ` (DD-MM)`.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::clock::Date;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    Todo,
    Planning,
    Planned,
    Active,
    Stale,
    Done,
    Canceled,
}

/// Why a manual move is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Into planning or active: those are claims.
    Claim,
    /// Already there.
    Same,
    /// Into stale from anywhere but active.
    NotActive,
}

impl Status {
    pub const ALL: [Status; 7] = [
        Status::Todo,
        Status::Planning,
        Status::Planned,
        Status::Active,
        Status::Stale,
        Status::Done,
        Status::Canceled,
    ];

    /// What `list` shows by default, in its order.
    pub const OPEN: [Status; 5] = [
        Status::Active,
        Status::Planned,
        Status::Planning,
        Status::Stale,
        Status::Todo,
    ];

    /// The directory under a project, which is also the status's name.
    pub fn dir(self) -> &'static str {
        match self {
            Status::Todo => "todo",
            Status::Planning => "planning",
            Status::Planned => "planned",
            Status::Active => "active",
            Status::Stale => "stale",
            Status::Done => "done",
            Status::Canceled => "canceled",
        }
    }

    pub fn from_dir(name: &str) -> Option<Status> {
        Status::ALL.into_iter().find(|s| s.dir() == name)
    }

    /// The manual move rule; `None` when the move is allowed.
    pub fn move_refusal(self, to: Status) -> Option<Refusal> {
        match to {
            Status::Planning | Status::Active => Some(Refusal::Claim),
            _ if self == to => Some(Refusal::Same),
            Status::Stale if self != Status::Active => Some(Refusal::NotActive),
            _ => None,
        }
    }

    pub fn can_move_to(self, to: Status) -> bool {
        self.move_refusal(to).is_none()
    }

    /// Done and canceled: the statuses a reprioritize or rename does not look in.
    pub fn is_closed(self) -> bool {
        matches!(self, Status::Done | Status::Canceled)
    }
}

/// A task's identity across refreshes: survives a move or reprioritize, not
/// a rename.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TaskKey {
    pub id: String,
    pub slug: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub status: Status,
    pub letter: char,
    pub id: String,
    pub slug: String,
}

impl Task {
    pub fn from_filename(status: Status, name: &str) -> Option<Task> {
        let (letter, id, slug) = split_filename(name)?;
        Some(Task {
            status,
            letter,
            id: id.to_string(),
            slug: slug.to_string(),
        })
    }

    pub fn filename(&self) -> String {
        format!("{}{}-{}.md", self.letter, self.id, self.slug)
    }

    /// The `<Letter><NNN>` a row starts with.
    pub fn name(&self) -> String {
        format!("{}{}", self.letter, self.id)
    }

    pub fn key(&self) -> TaskKey {
        TaskKey {
            id: self.id.clone(),
            slug: self.slug.clone(),
        }
    }
}

/// `<Letter><NNN>-<slug>[.md]` into its letter, id and slug.
fn split_filename(name: &str) -> Option<(char, &str, &str)> {
    let name = name.strip_suffix(".md").unwrap_or(name);
    let (head, slug) = name.split_once('-')?;
    let mut chars = head.chars();
    let letter = chars.next().filter(char::is_ascii_uppercase)?;
    let id = chars.as_str();
    let id_ok = id.len() == 3 && id.bytes().all(|b| b.is_ascii_digit());
    (id_ok && !slug.is_empty()).then_some((letter, id, slug))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub text: String,
    pub task: Option<Task>,
}

impl Row {
    fn text(text: impl Into<String>) -> Row {
        Row {
            text: text.into(),
            task: None,
        }
    }
}

/// What to list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    /// The sections, in order.
    pub statuses: Vec<Status>,
    /// Append done then canceled from the last this many days, each if any,
    /// unless already a section.
    pub recent: Option<u32>,
    /// Tag each row with its file's day-month mtime.
    pub dates: bool,
    /// Print each task's file, indented, under its row.
    pub verbose: bool,
    /// Bare filenames without `.md`, one per line, no headers.
    pub ids: bool,
}

impl Spec {
    /// The open statuses, stale only when asked for.
    pub fn open(stale: bool) -> Vec<Status> {
        Status::OPEN
            .into_iter()
            .filter(|&s| stale || s != Status::Stale)
            .collect()
    }
}

/// The rows of a project's listing, `(none)` when it shows no task. The
/// project directory must exist.
pub fn rows(project_dir: &Path, spec: &Spec) -> Result<Vec<Row>, String> {
    if !project_dir.is_dir() {
        return Err(format!(
            "error: tasks dir not found: {}",
            project_dir.display()
        ));
    }
    let mut out = Vec::new();
    let mut shown = 0;
    for &status in &spec.statuses {
        shown += section(&mut out, project_dir, status, None, spec);
    }
    if let Some(days) = spec.recent
        && !spec.ids
    {
        let window = Duration::from_secs(u64::from(days) * 86_400);
        let tail: Vec<Status> = [Status::Done, Status::Canceled]
            .into_iter()
            .filter(|s| !spec.statuses.contains(s))
            .filter(|&s| !files(project_dir, s, Some(window)).is_empty())
            .collect();
        if !tail.is_empty() {
            out.push(Row::text(""));
            for status in tail {
                shown += section(&mut out, project_dir, status, Some(window), spec);
            }
        }
    }
    if shown == 0 && !spec.ids {
        out.push(Row::text("(none)"));
    }
    Ok(out)
}

/// One status's rows, nothing when it holds no task. Returns the task count.
fn section(
    out: &mut Vec<Row>,
    project_dir: &Path,
    status: Status,
    window: Option<Duration>,
    spec: &Spec,
) -> usize {
    let files = files(project_dir, status, window);
    if files.is_empty() {
        return 0;
    }
    if spec.ids {
        out.extend(files.iter().map(|f| Row::text(stem(&f.name))));
        return files.len();
    }
    out.push(Row::text(format!(
        "{} ({})",
        status.dir().to_ascii_uppercase(),
        files.len()
    )));
    for f in &files {
        // A space after the id sets it off from the slug, whose own hyphens stay.
        let mut text = format!("  {}", stem(&f.name).replacen('-', " ", 1));
        if spec.dates {
            text.push_str(&format!(" ({})", Date::of(f.mtime).dd_mm()));
        }
        out.push(Row {
            text,
            task: Task::from_filename(status, &f.name),
        });
        if spec.verbose {
            out.push(Row::text(""));
            let body = fs::read_to_string(project_dir.join(status.dir()).join(&f.name))
                .unwrap_or_default();
            out.extend(body.lines().map(|l| Row::text(format!("    {l}"))));
            out.push(Row::text(""));
        }
    }
    files.len()
}

fn stem(name: &str) -> &str {
    name.strip_suffix(".md").unwrap_or(name)
}

struct File {
    name: String,
    mtime: SystemTime,
}

/// A status's `*.md` files, those touched within `window` when given. Done
/// and canceled sort by priority letter, then newest first: a finished task's
/// id says nothing about when it finished. The rest sort by name.
fn files(project_dir: &Path, status: Status, window: Option<Duration>) -> Vec<File> {
    let Ok(entries) = fs::read_dir(project_dir.join(status.dir())) else {
        return vec![];
    };
    let now = SystemTime::now();
    let mut files: Vec<File> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let meta = fs::metadata(e.path()).ok()?;
            (name.ends_with(".md") && meta.is_file()).then(|| File {
                name,
                mtime: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            })
        })
        .filter(|f| match (window, now.duration_since(f.mtime)) {
            (None, _) | (Some(_), Err(_)) => true,
            (Some(w), Ok(age)) => age < w,
        })
        .collect();
    if status.is_closed() {
        files.sort_by(|a, b| {
            let letter = |f: &File| f.name.chars().next();
            letter(a)
                .cmp(&letter(b))
                .then(b.mtime.cmp(&a.mtime))
                .then(a.name.cmp(&b.name))
        });
    } else {
        files.sort_by(|a, b| a.name.cmp(&b.name));
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::fs::File as FsFile;

    fn put(root: &Path, status: &str, name: &str, body: &str, days_ago: u64) {
        let dir = root.join(status);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(name), body).unwrap();
        let t = SystemTime::now() - Duration::from_secs(days_ago * 86_400);
        FsFile::options()
            .write(true)
            .open(dir.join(name))
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    /// A task in every open status, done and canceled ones of different ages.
    fn proj() -> TempDir {
        let tmp = TempDir::new();
        let p = tmp.path();
        put(p, "todo", "H003-fix-sensor-bug.md", "# Sensor\n", 0);
        put(p, "todo", "U012-update-readme.md", "# Readme\n", 0);
        put(p, "planned", "N007-refactor-api.md", "# Api\n", 0);
        put(
            p,
            "active",
            "H001-being-done.md",
            "# Being\n\nWorker: hlab-9\n",
            0,
        );
        put(p, "stale", "H002-paused.md", "# Paused\n", 0);
        put(p, "done", "N020-recent-done.md", "# Shipped\n", 2);
        put(p, "done", "N021-old-done.md", "# Old\n", 30);
        put(p, "canceled", "N022-recent-cancel.md", "# Dropped\n", 1);
        tmp
    }

    fn spec(statuses: &[Status]) -> Spec {
        Spec {
            statuses: statuses.to_vec(),
            recent: None,
            dates: false,
            verbose: false,
            ids: false,
        }
    }

    fn text(dir: &Path, spec: &Spec) -> String {
        rows(dir, spec)
            .unwrap()
            .iter()
            .map(|r| r.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_default_view_is_the_open_queue_stale_above_todo() {
        let t = proj();
        assert_eq!(
            text(t.path(), &spec(&Spec::open(true))),
            "ACTIVE (1)\n  H001 being-done\nPLANNED (1)\n  N007 refactor-api\n\
             STALE (1)\n  H002 paused\nTODO (2)\n  H003 fix-sensor-bug\n  U012 update-readme"
        );
        assert_eq!(
            text(t.path(), &spec(&Spec::open(false))),
            "ACTIVE (1)\n  H001 being-done\nPLANNED (1)\n  N007 refactor-api\n\
             TODO (2)\n  H003 fix-sensor-bug\n  U012 update-readme"
        );
    }

    #[test]
    fn a_row_names_its_task_and_nothing_else_does() {
        let t = proj();
        let rows = rows(t.path(), &spec(&[Status::Todo])).unwrap();
        assert_eq!(rows[0].task, None);
        let task = rows[1].task.as_ref().unwrap();
        assert_eq!(
            (task.status, task.filename()),
            (Status::Todo, "H003-fix-sensor-bug.md".to_string())
        );
    }

    #[test]
    fn ids_are_bare_filenames_by_priority_without_headers() {
        let t = proj();
        let ids = Spec {
            ids: true,
            recent: Some(7),
            ..spec(&[Status::Todo, Status::Planned])
        };
        assert_eq!(
            text(t.path(), &ids),
            "H003-fix-sensor-bug\nU012-update-readme\nN007-refactor-api"
        );
        let empty = TempDir::new();
        assert_eq!(rows(empty.path(), &ids).unwrap(), vec![]);
    }

    #[test]
    fn dates_tag_each_row_with_its_mtime() {
        let t = proj();
        let dated = Spec {
            dates: true,
            ..spec(&[Status::Active])
        };
        let mtime = fs::metadata(t.path().join("active/H001-being-done.md"))
            .and_then(|m| m.modified())
            .unwrap();
        let day = Date::of(mtime).dd_mm();
        assert_eq!(
            text(t.path(), &dated),
            format!("ACTIVE (1)\n  H001 being-done ({day})")
        );
    }

    #[test]
    fn verbose_prints_the_file_indented_under_its_row() {
        let t = proj();
        let verbose = Spec {
            verbose: true,
            ..spec(&[Status::Active])
        };
        assert_eq!(
            text(t.path(), &verbose),
            "ACTIVE (1)\n  H001 being-done\n\n    # Being\n    \n    Worker: hlab-9\n"
        );
    }

    #[test]
    fn recent_appends_done_then_canceled_within_the_window() {
        let t = proj();
        let recent = Spec {
            recent: Some(7),
            ..spec(&Spec::open(true))
        };
        let got = text(t.path(), &recent);
        assert!(
            got.ends_with(
                "  U012 update-readme\n\nDONE (1)\n  N020 recent-done\nCANCELED (1)\n  N022 recent-cancel"
            ),
            "{got}"
        );
        let wide = Spec {
            recent: Some(100),
            ..spec(&[Status::Todo])
        };
        assert!(text(t.path(), &wide).contains("DONE (2)\n  N020 recent-done\n  N021 old-done"));
    }

    #[test]
    fn recent_skips_an_empty_window_and_a_status_already_listed() {
        let t = proj();
        put(t.path(), "canceled", "N023-old-cancel.md", "", 40);
        fs::remove_file(t.path().join("canceled/N022-recent-cancel.md")).unwrap();
        let recent = Spec {
            recent: Some(7),
            ..spec(&[Status::Todo])
        };
        assert!(!text(t.path(), &recent).contains("CANCELED"));
        let all = Spec {
            recent: Some(7),
            ..spec(&[Status::Done])
        };
        assert_eq!(
            text(t.path(), &all),
            "DONE (2)\n  N020 recent-done\n  N021 old-done",
            "one unwindowed section, no tail"
        );
    }

    #[test]
    fn done_sorts_by_letter_then_newest_first() {
        let t = TempDir::new();
        put(t.path(), "done", "A010-alpha.md", "", 10);
        put(t.path(), "done", "H061-newer.md", "", 1);
        put(t.path(), "done", "H060-older.md", "", 5);
        assert_eq!(
            text(t.path(), &spec(&[Status::Done])),
            "DONE (3)\n  A010 alpha\n  H061 newer\n  H060 older"
        );
    }

    #[test]
    fn nothing_to_show_is_none_and_a_missing_project_an_error() {
        let t = TempDir::new();
        fs::create_dir_all(t.path().join("todo")).unwrap();
        assert_eq!(text(t.path(), &spec(&Spec::open(true))), "(none)");
        let gone = t.path().join("nope");
        assert_eq!(
            rows(&gone, &spec(&[Status::Todo])),
            Err(format!("error: tasks dir not found: {}", gone.display()))
        );
    }

    #[test]
    fn other_files_list_as_text() {
        let t = TempDir::new();
        put(t.path(), "todo", "notes.md", "", 0);
        put(t.path(), "todo", "N001-a.txt", "", 0);
        let rows = rows(t.path(), &spec(&[Status::Todo])).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[1].text.as_str(), &rows[1].task), ("  notes", &None));
    }

    #[test]
    fn moves_follow_the_manual_move_rule() {
        use Status::*;
        for from in Status::ALL {
            for to in Status::ALL {
                let legal = from != to
                    && match to {
                        Todo | Planned | Done | Canceled => true,
                        Stale => from == Active,
                        Planning | Active => false,
                    };
                assert_eq!(from.can_move_to(to), legal, "{from:?} -> {to:?}");
            }
        }
        assert_eq!(Active.move_refusal(Active), Some(Refusal::Claim));
        assert_eq!(Todo.move_refusal(Todo), Some(Refusal::Same));
        assert_eq!(Todo.move_refusal(Stale), Some(Refusal::NotActive));
    }
}
