//! The command line, parsed by hand: a verb and its arguments, or `--watch`.

use crate::listing::Status;

pub const HELP: &str = "\
Taskania: the task tool. Lists and changes tasks kept as markdown files under $TASKS_ROOT (default ~/repos/tasks), or shows them live.
Usage: taskania VERB [ARGS...] | taskania --watch [--with NAME]
Verbs:
  list           list a project's tasks
  claim          take a task: todo/ -> planning/, planned/ or stale/ -> active/
  planned        park a groomed task: planning/ -> planned/
  unclaim        give a claimed task back: planning/ -> todo/, active/ -> planned/
  done           complete a task: active/ -> done/
  move           move a task by hand
  reprioritize   change a task's priority letter
  rename         change a task's slug
  create         add a reminder task to todo/
  next-id        print a project's next free task id
  commit         commit and push everything changed in the tasks repo
  project        print the cwd's project
Every verb but commit and project acts on the cwd's project, or on --project NAME.
A change pulls the tasks repo first, then commits and pushes it.
taskania VERB --help explains a verb; taskania --watch --help the dashboard.";

pub const WATCH_HELP: &str = "\
Taskania's dashboard: the cwd's project's tasks, live, with a cursor to peek, edit, create, move, reprioritize and rename them.
Usage: taskania --watch [--with NAME]
--with draws a second project's list right-aligned beside it.
Below the open queue come done then canceled since a marker 7 days back.
Keys: up/down move the row cursor, left/right hop to that column, esc hides it.
On a row: enter peeks, e edits, m moves, p sets the priority, r renames. n creates a task.
v opens the view menu: left/right move the done-since marker, enter applies it,
d s toggle row dates and the stale section, esc closes. q or ctrl-c quits.
In a peek: up/down scroll, pgdn/space and pgup page, home/end jump, esc/enter/q close.";

/// A verb's name, the help it prints, and whether it takes `--project`.
struct Spec {
    name: &'static str,
    help: &'static str,
    project: bool,
}

const SPECS: [Spec; 12] = [
    Spec {
        name: "list",
        project: true,
        help: "\
List a project's tasks, a LABEL (n) section per status: active planned planning stale todo.
Usage: taskania list [--project NAME] [--status STATUS...] [--all] [--verbose] [--no-header] [--ids] [--no-stale] [--dates] [--recent DAYS]
Statuses: todo planning planned active stale done canceled.
--status lists only those; --all lists every status, done and canceled too.
--recent DAYS appends done then canceled from the last DAYS days, each if any.
--no-stale omits the stale section. --dates tags each row with its file's day-month mtime.
--verbose prints each task's file under its row. --no-header drops the Tasks: line.
--ids prints bare filenames without .md, for choosers.",
    },
    Spec {
        name: "claim",
        project: true,
        help: "\
Claim a task: todo/ -> planning/, planned/ or stale/ -> active/. Prints the task.
Usage: taskania claim [--project NAME] FILE
FILE is the task's filename, with or without .md.",
    },
    Spec {
        name: "planned",
        project: true,
        help: "\
Park a groomed task: planning/ -> planned/.
Usage: taskania planned [--project NAME] FILE",
    },
    Spec {
        name: "unclaim",
        project: true,
        help: "\
Give a claimed task back: planning/ -> todo/, active/ -> planned/.
Usage: taskania unclaim [--project NAME] FILE",
    },
    Spec {
        name: "done",
        project: true,
        help: "\
Complete a task: active/ -> done/. --force takes it from any other status.
Usage: taskania done [--project NAME] [--force] FILE",
    },
    Spec {
        name: "move",
        project: true,
        help: "\
Move a task by hand: into todo/, planned/, done/ or canceled/ from any other status, into stale/ only from active/.
Usage: taskania move [--project NAME] FILE STATUS
planning/ and active/ are claims: see taskania claim.",
    },
    Spec {
        name: "reprioritize",
        project: true,
        help: "\
Change an open task's priority letter; its id and status stay.
Usage: taskania reprioritize [--project NAME] FILE|ID LETTER
ID is the task's number alone, e.g. 42.",
    },
    Spec {
        name: "rename",
        project: true,
        help: "\
Set an open task's filename slug from TITLE; its id, letter and status stay.
Usage: taskania rename [--project NAME] FILE|ID TITLE
ID is the task's number alone, e.g. 42. Quote a multi-word title.",
    },
    Spec {
        name: "create",
        project: true,
        help: "\
Create a reminder task: an empty file in todo/, named by the title. Prints its project and filename.
Usage: taskania create [--project NAME] [-p LETTER] TITLE...
-p sets the priority letter (default N). The cwd's project is made if it has no tasks yet;
a --project one must exist.",
    },
    Spec {
        name: "next-id",
        project: true,
        help: "\
Print a project's next free task id, three digits.
Usage: taskania next-id [--project NAME]",
    },
    Spec {
        name: "commit",
        project: false,
        help: "\
Commit everything changed in the tasks repo, and push it.
Usage: taskania commit MESSAGE",
    },
    Spec {
        name: "project",
        project: false,
        help: "\
Print the cwd's project.
Usage: taskania project
A .find-project.conf in the cwd or its nearest ancestor names it: a project = NAME line, else
its first line. Otherwise the git checkout's directory name, lab suffix stripped, mapped through
$TASKANIA_PROJECTS_CONF (default $XDG_CONFIG_HOME/taskania/projects.conf, ~/.config when unset:
name = project lines), else lowercased.",
    },
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListArgs {
    pub statuses: Vec<Status>,
    pub all: bool,
    pub verbose: bool,
    pub no_header: bool,
    pub ids: bool,
    pub no_stale: bool,
    pub dates: bool,
    pub recent: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verb {
    List(ListArgs),
    Claim(String),
    Planned(String),
    Unclaim(String),
    Done { file: String, force: bool },
    Move { file: String, to: String },
    Reprioritize { task: String, letter: String },
    Rename { task: String, title: String },
    Create { letter: String, title: String },
    NextId,
    Commit(String),
    Project,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parsed {
    Help(&'static str),
    Watch { with: Option<String> },
    Run { project: Option<String>, verb: Verb },
}

pub fn parse<I: IntoIterator<Item = String>>(argv: I) -> Result<Parsed, String> {
    let argv: Vec<String> = argv.into_iter().collect();
    match argv.first() {
        None => Ok(Parsed::Help(HELP)),
        Some(first) => match SPECS.iter().find(|s| s.name == first) {
            Some(spec) => parse_verb(spec, &argv[1..]),
            None => parse_watch(&argv),
        },
    }
}

fn parse_watch(argv: &[String]) -> Result<Parsed, String> {
    let watch = argv.iter().any(|a| a == "--watch");
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(Parsed::Help(if watch { WATCH_HELP } else { HELP }));
    }
    let mut with = None;
    let mut it = argv.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--watch" => {}
            "--with" => with = Some(value(&mut it, arg)?),
            a if a.starts_with('-') => return Err(format!("unexpected argument: {a}")),
            a => return Err(format!("unknown verb: {a} (taskania --help lists them)")),
        }
    }
    if !watch {
        return Err("--with goes with --watch".into());
    }
    Ok(Parsed::Watch { with })
}

fn parse_verb(spec: &Spec, args: &[String]) -> Result<Parsed, String> {
    let name = spec.name;
    let options = args.iter().take_while(|a| *a != "--");
    if options.clone().any(|a| a == "--help" || a == "-h") {
        return Ok(Parsed::Help(spec.help));
    }
    let mut project = None;
    let mut list = ListArgs::default();
    let (mut force, mut letter) = (false, None);
    let mut positional = Vec::new();
    let mut it = args.iter().peekable();
    while let Some(arg) = it.next() {
        if arg == "--" {
            positional.extend(it.by_ref().cloned());
            break;
        }
        if !arg.starts_with('-') || arg == "-" {
            positional.push(arg.clone());
            continue;
        }
        match (name, arg.as_str()) {
            (_, "--project") if spec.project => project = Some(value(&mut it, arg)?),
            ("list", "--status") => {
                while let Some(s) = it.peek().and_then(|s| Status::from_dir(s)) {
                    list.statuses.push(s);
                    it.next();
                }
            }
            ("list", "--all") => list.all = true,
            ("list", "--verbose") => list.verbose = true,
            ("list", "--no-header") => list.no_header = true,
            ("list", "--ids") => list.ids = true,
            ("list", "--no-stale") => list.no_stale = true,
            ("list", "--dates") => list.dates = true,
            ("list", "--recent") => {
                let days = value(&mut it, arg)?;
                list.recent = Some(whole(&days).ok_or("--recent takes a whole-number day count")?);
            }
            ("done", "--force") => force = true,
            ("create", "-p") => letter = Some(value(&mut it, arg)?),
            ("create", p) if p.starts_with("-p") => letter = Some(p[2..].to_string()),
            _ => return Err(format!("{name}: unexpected argument: {arg}")),
        }
    }

    let mut pos = positional.into_iter();
    let mut need = |what: &str| {
        pos.next()
            .ok_or_else(|| format!("{name} needs {what} (taskania {name} --help)"))
    };
    let verb = match name {
        "list" => Verb::List(list),
        "claim" => Verb::Claim(need("FILE")?),
        "planned" => Verb::Planned(need("FILE")?),
        "unclaim" => Verb::Unclaim(need("FILE")?),
        "done" => Verb::Done {
            file: need("FILE")?,
            force,
        },
        "move" => Verb::Move {
            file: need("FILE")?,
            to: need("STATUS")?,
        },
        "reprioritize" => Verb::Reprioritize {
            task: need("FILE or ID")?,
            letter: need("LETTER")?,
        },
        "rename" => Verb::Rename {
            task: need("FILE or ID")?,
            title: need("TITLE")?,
        },
        "create" => {
            let title: Vec<String> = pos.by_ref().collect();
            Verb::Create {
                letter: letter.unwrap_or_else(|| "N".into()),
                title: title.join(" "),
            }
        }
        "next-id" => Verb::NextId,
        "commit" => Verb::Commit(need("MESSAGE")?),
        "project" => Verb::Project,
        _ => unreachable!("every spec has a verb"),
    };
    if let Some(extra) = pos.next() {
        return Err(format!("{name}: unexpected argument: {extra}"));
    }
    Ok(Parsed::Run { project, verb })
}

fn value<'a>(it: &mut impl Iterator<Item = &'a String>, flag: &str) -> Result<String, String> {
    it.next()
        .cloned()
        .ok_or_else(|| format!("{flag} needs a value"))
}

fn whole(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(argv: &[&str]) -> Result<Parsed, String> {
        parse(argv.iter().map(|s| s.to_string()))
    }

    fn verb(argv: &[&str]) -> (Option<String>, Verb) {
        match run(argv) {
            Ok(Parsed::Run { project, verb }) => (project, verb),
            other => panic!("{argv:?}: {other:?}"),
        }
    }

    #[test]
    fn bare_and_help_print_the_top_help() {
        for argv in [&[][..], &["--help"], &["-h"]] {
            assert_eq!(run(argv), Ok(Parsed::Help(HELP)));
        }
        assert!(HELP.lines().nth(1).unwrap().starts_with("Usage: taskania"));
    }

    #[test]
    fn every_verb_has_help_with_a_usage_line() {
        for spec in &SPECS {
            for flag in ["--help", "-h"] {
                assert_eq!(run(&[spec.name, flag]), Ok(Parsed::Help(spec.help)));
            }
            let usage = format!("Usage: taskania {}", spec.name);
            assert!(
                spec.help.lines().nth(1).unwrap().starts_with(&usage),
                "{}",
                spec.name
            );
            assert!(
                HELP.contains(&format!("\n  {} ", spec.name)),
                "{}",
                spec.name
            );
        }
        assert_eq!(
            run(&["move", "x", "--help", "y"]),
            Ok(Parsed::Help(SPECS[5].help))
        );
    }

    #[test]
    fn watch_takes_only_with() {
        assert_eq!(run(&["--watch"]), Ok(Parsed::Watch { with: None }));
        assert_eq!(
            run(&["--with", "dotfiles", "--watch"]),
            Ok(Parsed::Watch {
                with: Some("dotfiles".into())
            })
        );
        assert_eq!(run(&["--watch", "--help"]), Ok(Parsed::Help(WATCH_HELP)));
        assert_eq!(run(&["-h", "--watch"]), Ok(Parsed::Help(WATCH_HELP)));
        for gone in [
            "--project",
            "--recent",
            "--poll",
            "--dates",
            "--labs",
            "--stale",
            "--once",
        ] {
            assert_eq!(
                run(&["--watch", gone]),
                Err(format!("unexpected argument: {gone}"))
            );
        }
        assert_eq!(
            run(&["--watch", "--with"]),
            Err("--with needs a value".into())
        );
        assert_eq!(
            run(&["--with", "x"]),
            Err("--with goes with --watch".into())
        );
        assert!(
            run(&["bogus"])
                .unwrap_err()
                .starts_with("unknown verb: bogus")
        );
    }

    #[test]
    fn verbs_take_their_positionals_and_project_anywhere() {
        assert_eq!(
            verb(&["claim", "N001-x", "--project", "widget"]),
            (Some("widget".into()), Verb::Claim("N001-x".into()))
        );
        assert_eq!(
            verb(&["done", "--force", "N001-x.md"]),
            (
                None,
                Verb::Done {
                    file: "N001-x.md".into(),
                    force: true
                }
            )
        );
        assert_eq!(
            verb(&["move", "N001-x", "done"]).1,
            Verb::Move {
                file: "N001-x".into(),
                to: "done".into()
            }
        );
        assert_eq!(
            verb(&["reprioritize", "42", "h"]).1,
            Verb::Reprioritize {
                task: "42".into(),
                letter: "h".into()
            }
        );
        assert_eq!(
            verb(&["rename", "42", "New title"]).1,
            Verb::Rename {
                task: "42".into(),
                title: "New title".into()
            }
        );
        assert_eq!(verb(&["next-id", "--project", "p"]).1, Verb::NextId);
        assert_eq!(
            verb(&["commit", "Edit: x"]).1,
            Verb::Commit("Edit: x".into())
        );
        assert_eq!(verb(&["project"]).1, Verb::Project);
    }

    #[test]
    fn create_joins_its_title_words() {
        let create = |letter: &str, title: &str| Verb::Create {
            letter: letter.into(),
            title: title.into(),
        };
        assert_eq!(
            verb(&["create", "fix", "the", "thing"]).1,
            create("N", "fix the thing")
        );
        assert_eq!(verb(&["create", "-p", "h", "x y"]).1, create("h", "x y"));
        assert_eq!(verb(&["create", "-pU", "x"]).1, create("U", "x"));
        assert_eq!(
            verb(&["create", "--", "-5", "degrees"]).1,
            create("N", "-5 degrees")
        );
        assert_eq!(verb(&["create"]).1, create("N", ""));
    }

    #[test]
    fn list_takes_its_flags() {
        let (project, v) = verb(&[
            "list",
            "--status",
            "todo",
            "done",
            "--recent",
            "3",
            "--project",
            "p",
            "--all",
            "--verbose",
            "--no-header",
            "--ids",
            "--no-stale",
            "--dates",
        ]);
        let want = ListArgs {
            statuses: vec![Status::Todo, Status::Done],
            all: true,
            verbose: true,
            no_header: true,
            ids: true,
            no_stale: true,
            dates: true,
            recent: Some(3),
        };
        assert_eq!((project, v), (Some("p".into()), Verb::List(want)));
        assert_eq!(
            run(&["list", "--status", "bogus"]),
            Err("list: unexpected argument: bogus".into())
        );
        assert!(run(&["list", "--recent", "x"]).is_err());
        assert!(run(&["list", "--recent", "-1"]).is_err());
        assert_eq!(
            run(&["list", "--no-workers"]),
            Err("list: unexpected argument: --no-workers".into())
        );
        assert_eq!(
            run(&["list", "widget"]),
            Err("list: unexpected argument: widget".into())
        );
    }

    #[test]
    fn bad_input_is_an_error() {
        assert_eq!(
            run(&["claim"]),
            Err("claim needs FILE (taskania claim --help)".into())
        );
        assert_eq!(
            run(&["move", "N001-x"]),
            Err("move needs STATUS (taskania move --help)".into())
        );
        assert_eq!(
            run(&["claim", "a", "b"]),
            Err("claim: unexpected argument: b".into())
        );
        assert_eq!(
            run(&["claim", "--project"]),
            Err("--project needs a value".into())
        );
        assert_eq!(
            run(&["claim", "--force", "a"]),
            Err("claim: unexpected argument: --force".into())
        );
        for name in ["commit", "project"] {
            assert_eq!(
                run(&[name, "--project", "p", "m"]),
                Err(format!("{name}: unexpected argument: --project"))
            );
        }
    }
}
