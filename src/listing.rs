//! The rows of `task-list.sh`'s human output, and the task each one names.
//!
//! That format is a contract: a `LABEL (count)` header sets the status of the
//! rows below it, and a task row is two spaces, `<Letter><NNN>`, a space, the
//! slug, then optionally ` [worker]` and ` (DD-MM)`. Anything else is text.

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

    /// The directory under a project, which is also the name the scripts take.
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

    fn from_label(label: &str) -> Option<Status> {
        Status::ALL.into_iter().find(|s| {
            s.dir().eq_ignore_ascii_case(label) && label.bytes().all(|b| b.is_ascii_uppercase())
        })
    }

    /// Whether `task-move.sh` takes a task from here to `to`: into todo,
    /// planned, done or canceled from anywhere else, into stale only from active,
    /// never into planning or active (those are claims).
    pub fn can_move_to(self, to: Status) -> bool {
        self != to
            && match to {
                Status::Todo | Status::Planned | Status::Done | Status::Canceled => true,
                Status::Stale => self == Status::Active,
                Status::Planning | Status::Active => false,
            }
    }

    /// Done and canceled: the statuses `task-reprioritize.sh` does not look in.
    pub fn is_closed(self) -> bool {
        matches!(self, Status::Done | Status::Canceled)
    }
}

/// A task's identity across refreshes: the id and slug survive a
/// reprioritize, which rewrites only the letter, and a move, which only
/// changes the directory.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TaskKey {
    pub id: String,
    pub slug: String,
}

impl TaskKey {
    /// From a task filename, `<Letter><NNN>-<slug>[.md]`.
    pub fn from_filename(name: &str) -> Option<TaskKey> {
        let name = name.strip_suffix(".md").unwrap_or(name);
        let (head, slug) = name.split_once('-')?;
        let (_, id) = split_name(head)?;
        (!slug.is_empty()).then(|| TaskKey {
            id: id.to_string(),
            slug: slug.to_string(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub status: Status,
    pub letter: char,
    pub id: String,
    pub slug: String,
}

impl Task {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub text: String,
    pub task: Option<Task>,
}

/// Split `task-list.sh` output into rows: trailing newlines dropped, every
/// other line kept, blank ones included.
pub fn parse(output: &str) -> Vec<Row> {
    let mut status = None;
    output
        .trim_end_matches('\n')
        .split('\n')
        .map(|line| {
            if let Some(s) = header(line) {
                status = Some(s);
            }
            let task = status.and_then(|s| task_row(line, s));
            Row {
                text: line.to_string(),
                task,
            }
        })
        .collect()
}

/// `TODO (3)` and its kin.
fn header(line: &str) -> Option<Status> {
    let (label, rest) = line.split_once(" (")?;
    let count = rest.strip_suffix(')')?;
    if count.is_empty() || !count.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Status::from_label(label)
}

fn task_row(line: &str, status: Status) -> Option<Task> {
    let rest = line.strip_prefix("  ")?;
    let (head, rest) = rest.split_once(' ')?;
    let (letter, id) = split_name(head)?;
    let slug = rest.split(' ').next().unwrap_or("");
    if slug.is_empty() {
        return None;
    }
    Some(Task {
        status,
        letter,
        id: id.to_string(),
        slug: slug.to_string(),
    })
}

/// `N001` into its letter and three-digit id.
fn split_name(head: &str) -> Option<(char, &str)> {
    let mut chars = head.chars();
    let letter = chars.next().filter(char::is_ascii_uppercase)?;
    let id = chars.as_str();
    (id.len() == 3 && id.bytes().all(|b| b.is_ascii_digit())).then_some((letter, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = "\
ACTIVE (1)
  H010 busy [hlab-7] (16-11)
TODO (2)
  N001 fix-the-thing
  U012 update-readme (03-11)

DONE (1)
  N050 shipped
";

    #[test]
    fn rows_keep_every_line_but_the_trailing_newline() {
        let rows = parse(LISTING);
        assert_eq!(rows.len(), 8);
        assert_eq!(rows[5].text, "");
        assert_eq!(parse("(none)\n").len(), 1);
    }

    #[test]
    fn a_header_sets_the_status_of_the_rows_below() {
        let rows = parse(LISTING);
        let tasks: Vec<_> = rows.iter().filter_map(|r| r.task.as_ref()).collect();
        let got: Vec<_> = tasks.iter().map(|t| (t.status, t.filename())).collect();
        assert_eq!(
            got,
            vec![
                (Status::Active, "H010-busy.md".to_string()),
                (Status::Todo, "N001-fix-the-thing.md".to_string()),
                (Status::Todo, "U012-update-readme.md".to_string()),
                (Status::Done, "N050-shipped.md".to_string()),
            ]
        );
    }

    #[test]
    fn worker_and_date_tags_are_not_part_of_the_slug() {
        let rows = parse(LISTING);
        let busy = rows[1].task.as_ref().unwrap();
        assert_eq!(busy.slug, "busy");
        assert_eq!(busy.name(), "H010");
    }

    #[test]
    fn other_lines_are_not_task_rows() {
        for line in [
            "(none)",
            "",
            "error: tasks dir not found: /x",
            "TODO (2)",
            "   N001 indented-too-far",
            "  N01 short-id",
            "  n001 lowercase",
            "  N001",
        ] {
            let rows = parse(&format!("TODO (1)\n{line}\n(end)"));
            assert_eq!(rows[1].task, None, "{line:?}");
        }
    }

    #[test]
    fn rows_before_any_header_are_not_tasks() {
        assert_eq!(parse("  N001 orphan")[0].task, None);
    }

    #[test]
    fn a_header_needs_a_known_uppercase_label_and_a_count() {
        assert_eq!(header("TODO (3)"), Some(Status::Todo));
        assert_eq!(header("CANCELED (12)"), Some(Status::Canceled));
        assert_eq!(header("todo (3)"), None);
        assert_eq!(header("TODO (x)"), None);
        assert_eq!(header("TODO ()"), None);
        assert_eq!(header("BOGUS (1)"), None);
    }

    #[test]
    fn moves_follow_the_manual_move_rule() {
        use Status::*;
        // Every pair, as task-move.sh decides it: todo, planned, done and
        // canceled from any other status, stale only from active.
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
    }

    #[test]
    fn a_key_comes_from_a_filename() {
        let key = TaskKey::from_filename("N050-fix-the-sensor.md").unwrap();
        assert_eq!(
            (key.id.as_str(), key.slug.as_str()),
            ("050", "fix-the-sensor")
        );
        assert_eq!(
            TaskKey::from_filename("N050-x"),
            Some(TaskKey {
                id: "050".into(),
                slug: "x".into()
            })
        );
        assert_eq!(TaskKey::from_filename("notes.md"), None);
        assert_eq!(TaskKey::from_filename("N050-.md"), None);
    }
}
