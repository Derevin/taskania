//! Command-line flags, parsed by hand.

pub const HELP: &str = "\
Taskania: live task dashboard over the task scripts, with a cursor to peek, edit, create, move and reprioritize tasks.
Usage: taskania [--project NAME] [--with NAME] [--recent DAYS] [--dates] [--labs] [--stale] [--poll SECS] [--once]
--project names the primary project (default: find-project.sh's answer for the cwd).
--with draws a second project's list right-aligned beside it.
Below the open queue come done then canceled since a marker DAYS back (--recent, default 7).
--dates, --labs and --stale start with row dates, [lab] tags and the stale section shown.
--poll is the seconds between checks for changed task files (default 2).
--once prints one frame and exits, leaving the terminal alone.
Keys: left/right move the done-since marker, d l s toggle dates, lab tags, stale, q quits.
up/down move the row cursor, tab hops columns, esc hides it. On a row: enter peeks,
e edits, m moves, p sets the priority. n creates a task.
Needs the task scripts (task-list.sh and kin) on PATH.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    pub project: Option<String>,
    pub with: Option<String>,
    pub recent: u32,
    pub dates: bool,
    pub labs: bool,
    pub stale: bool,
    pub poll: u64,
    pub once: bool,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            project: None,
            with: None,
            recent: 7,
            dates: false,
            labs: false,
            stale: false,
            poll: 2,
            once: false,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    Help,
    Run(Args),
}

pub fn parse<I: IntoIterator<Item = String>>(argv: I) -> Result<Parsed, String> {
    let mut args = Args::default();
    let mut it = argv.into_iter();
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--help" | "-h" => return Ok(Parsed::Help),
            "--project" => args.project = Some(value()?),
            "--with" => args.with = Some(value()?),
            "--recent" => {
                args.recent = whole(&value()?).ok_or("--recent takes a whole-number day count")?
            }
            "--poll" => {
                args.poll = whole(&value()?)
                    .filter(|&p| p > 0)
                    .ok_or("--poll takes a whole number of seconds, at least 1")?
                    .into()
            }
            "--dates" => args.dates = true,
            "--labs" => args.labs = true,
            "--stale" => args.stale = true,
            "--once" => args.once = true,
            _ => return Err(format!("unexpected argument: {flag}")),
        }
    }
    Ok(Parsed::Run(args))
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

    #[test]
    fn no_flags_means_the_defaults() {
        assert_eq!(run(&[]), Ok(Parsed::Run(Args::default())));
        let Ok(Parsed::Run(a)) = run(&[]) else {
            unreachable!()
        };
        assert_eq!(
            (a.recent, a.poll, a.dates, a.labs, a.stale, a.once),
            (7, 2, false, false, false, false)
        );
    }

    #[test]
    fn every_flag_lands() {
        let got = run(&[
            "--project",
            "proj",
            "--with",
            "widget",
            "--recent",
            "3",
            "--dates",
            "--labs",
            "--stale",
            "--poll",
            "5",
            "--once",
        ]);
        let want = Args {
            project: Some("proj".into()),
            with: Some("widget".into()),
            recent: 3,
            dates: true,
            labs: true,
            stale: true,
            poll: 5,
            once: true,
        };
        assert_eq!(got, Ok(Parsed::Run(want)));
    }

    #[test]
    fn help_wins_wherever_it_stands() {
        assert_eq!(run(&["--help"]), Ok(Parsed::Help));
        assert_eq!(run(&["--once", "--help"]), Ok(Parsed::Help));
        assert!(HELP.lines().nth(1).unwrap().starts_with("Usage: taskania"));
    }

    #[test]
    fn bad_input_is_an_error() {
        assert_eq!(run(&["--with"]), Err("--with needs a value".into()));
        assert_eq!(run(&["--project"]), Err("--project needs a value".into()));
        assert!(run(&["--recent", "x"]).is_err());
        assert!(run(&["--recent", "-1"]).is_err());
        assert!(run(&["--poll", "0"]).is_err());
        assert!(run(&["--settle", "6"]).is_err());
        assert!(run(&["--", "--all"]).is_err());
    }
}
