//! Every change to task data. A task is a markdown file under
//! `<root>/<project>/<status>/`, named `<Letter><NNN>-<slug>.md`; a change
//! moves, renames or creates one.
//!
//! A change is checked first, touching nothing and running no git, then
//! applied after the pull. [`Tasks::publish`] commits each change on its own
//! and pushes them together; a verb publishes a batch of one. Each result is
//! what the verb prints on success, or its error line.

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};

use crate::git::Git;
use crate::listing::{Refusal, Status};

use Status::*;

/// A change that passed its check, or the check's refusal.
pub type Change = Result<Apply, String>;

/// A checked change's work on the tree, run after the pull.
pub type Apply = Box<dyn FnOnce() -> Result<Applied, String>>;

/// What a change's verb prints, read once the batch is published.
type Out = Box<dyn FnOnce() -> String>;

pub struct Applied {
    /// None when the change found nothing to do: there is nothing to commit.
    message: Option<String>,
    out: Out,
}

impl Applied {
    fn commit(message: String, out: impl FnOnce() -> String + 'static) -> Applied {
        Applied {
            message: Some(message),
            out: Box::new(out),
        }
    }

    fn nothing(out: String) -> Applied {
        Applied {
            message: None,
            out: Box::new(move || out),
        }
    }
}

/// How far a batch is: git commands finished, of those it will run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
}

#[derive(Clone)]
pub struct Tasks {
    root: PathBuf,
}

impl Tasks {
    pub fn new(root: PathBuf) -> Tasks {
        Tasks { root }
    }

    /// A project's directory, which must exist.
    pub fn project_dir(&self, project: &str) -> Result<PathBuf, String> {
        let dir = self.root.join(project);
        if dir.is_dir() {
            Ok(dir)
        } else {
            Err(format!("error: tasks dir not found: {}", dir.display()))
        }
    }

    /// Publish a batch: one pull, each change applied and committed in turn,
    /// then one pull and push, run only once something was committed. A change
    /// failing its check runs no git; one failing its apply or its commit fails
    /// alone. A failed final pull or push fails every change that committed,
    /// whose commits wait for the next push. `report` hears the progress; one
    /// result per change, in order.
    pub fn publish(
        &self,
        batch: Vec<Change>,
        report: &dyn Fn(Progress),
    ) -> Vec<Result<String, String>> {
        let checked = batch.iter().filter(|c| c.is_ok()).count();
        if checked == 0 {
            // Every change is its check's refusal.
            return batch.into_iter().filter_map(Result::err).map(Err).collect();
        }
        // A pull, an add and a commit per change, a pull and a push.
        let counter = Counter::new(3 + 2 * checked, report);
        let ran = || counter.ran();
        let git = Git::new(&self.root, &ran);
        git.pull();
        let fates: Vec<Fate> = batch
            .into_iter()
            .map(|change| match change {
                Err(e) => Fate::Failed(e),
                Ok(apply) => counter.spend(2, || match apply() {
                    Err(e) => Fate::Failed(e),
                    Ok(Applied { message: None, out }) => Fate::Untouched(out),
                    Ok(Applied {
                        message: Some(message),
                        out,
                    }) => match git.commit(&message) {
                        Ok(()) => Fate::Committed(out),
                        Err(e) => Fate::Failed(e),
                    },
                }),
            })
            .collect();
        let committed = fates.iter().any(|f| matches!(f, Fate::Committed(_)));
        let pushed = counter.spend(2, || if committed { git.push() } else { Ok(()) });
        fates
            .into_iter()
            .map(|fate| match fate {
                Fate::Failed(e) => Err(e),
                Fate::Untouched(out) => Ok(out()),
                Fate::Committed(out) => pushed.clone().map(|()| out()),
            })
            .collect()
    }

    /// A batch of one, as a verb publishes it.
    pub fn publish_one(&self, change: Change) -> Result<String, String> {
        self.publish(vec![change], &|_| {}).remove(0)
    }

    /// todo -> planning; planned or stale -> active. Prints a banner, then the
    /// task.
    pub fn claim(&self, project: &str, file: &str) -> Change {
        let file = task_filename(file);
        let dir = self.project_dir(project)?;
        let project = project.to_string();
        Ok(Box::new(move || {
            let (from, to) = [(Todo, Planning), (Planned, Active), (Stale, Active)]
                .into_iter()
                .find(|(from, _)| dir.join(from.dir()).join(&file).is_file())
                .ok_or_else(|| format!("error: {file} not found in todo/, planned/, or stale/"))?;
            let dst = relocate(&dir, &file, from, to)
                .map_err(|e| format!("error: {e}\nerror: mv failed (race condition?)"))?;
            Ok(Applied::commit(
                format!("Claim: {}", slug(&file)),
                move || {
                    let body = fs::read_to_string(&dst).unwrap_or_default();
                    format!("--- Claimed: {project}/{file} ---\n\n{body}")
                },
            ))
        }))
    }

    /// planning -> planned.
    pub fn planned(&self, project: &str, file: &str) -> Change {
        let file = task_filename(file);
        let dir = self.project_dir(project)?;
        let project = project.to_string();
        Ok(Box::new(move || {
            let src = dir.join(Planning.dir()).join(&file);
            if !src.is_file() {
                return Err(format!("error: not found: {}", src.display()));
            }
            relocate(&dir, &file, Planning, Planned).map_err(io_error)?;
            Ok(Applied::commit(
                format!("Plan: {}", slug(&file)),
                move || format!("--- Planned: {project}/{file} ---\n"),
            ))
        }))
    }

    /// planning -> todo; active -> planned.
    pub fn unclaim(&self, project: &str, file: &str) -> Change {
        let file = task_filename(file);
        let dir = self.project_dir(project)?;
        let project = project.to_string();
        Ok(Box::new(move || {
            let (from, to) = [(Planning, Todo), (Active, Planned)]
                .into_iter()
                .find(|(from, _)| dir.join(from.dir()).join(&file).is_file())
                .ok_or_else(|| format!("error: {file} not found in planning/ or active/"))?;
            relocate(&dir, &file, from, to).map_err(io_error)?;
            Ok(Applied::commit(
                format!("Unclaim: {}", slug(&file)),
                move || format!("--- Unclaimed: {project}/{file} ---\n"),
            ))
        }))
    }

    /// active -> done; `force` takes it from any other status.
    pub fn done(&self, project: &str, file: &str, force: bool) -> Change {
        let file = task_filename(file);
        let dir = self.project_dir(project)?;
        let project = project.to_string();
        Ok(Box::new(move || {
            let from = if force {
                [Active, Planned, Planning, Stale, Todo, Canceled]
                    .into_iter()
                    .find(|s| dir.join(s.dir()).join(&file).is_file())
                    .ok_or_else(|| format!("error: {file} not found in any state"))?
            } else {
                let src = dir.join(Active.dir()).join(&file);
                if !src.is_file() {
                    return Err(format!("error: not found: {}", src.display()));
                }
                Active
            };
            relocate(&dir, &file, from, Done).map_err(io_error)?;
            Ok(Applied::commit(
                format!("Done: {}", slug(&file)),
                move || format!("--- Done: {project}/{file} ---\n"),
            ))
        }))
    }

    /// A manual move, by [`Status::move_refusal`]'s rule.
    pub fn move_task(&self, project: &str, file: &str, to: &str) -> Change {
        let file = task_filename(file);
        let to = Status::from_dir(to).ok_or_else(|| {
            format!("error: unknown status: {to} (todo, planned, stale, done, canceled)")
        })?;
        // A claim target is refused whatever the task's status, before looking
        // for the task or its project.
        if matches!(to, Planning | Active) {
            return Err(format!(
                "error: {}/ is a claim, made by taskania claim",
                to.dir()
            ));
        }
        let dir = self.project_dir(project)?;
        let project = project.to_string();
        Ok(Box::new(move || {
            let found: Vec<Status> = Status::ALL
                .into_iter()
                .filter(|s| dir.join(s.dir()).join(&file).is_file())
                .collect();
            let from = match found[..] {
                [] => return Err(format!("error: {file} not found in {project}")),
                [from] => from,
                _ => {
                    let dirs: Vec<&str> = found.iter().map(|s| s.dir()).collect();
                    return Err(format!(
                        "error: {file} is in more than one status: {}",
                        dirs.join(" ")
                    ));
                }
            };
            let (f, t) = (from.dir(), to.dir());
            match from.move_refusal(to) {
                Some(Refusal::Claim) => unreachable!("claim targets are refused above"),
                Some(Refusal::Same) => return Err(format!("error: {file} is already in {t}/")),
                Some(Refusal::NotActive) => {
                    return Err(format!(
                        "error: only an active task goes stale ({file} is in {f}/)"
                    ));
                }
                None => {}
            }
            relocate(&dir, &file, from, to).map_err(io_error)?;
            Ok(Applied::commit(
                format!("Move: {} {f}->{t}", slug(&file)),
                move || format!("--- Moved: {project}/{file} {f} -> {t} ---\n"),
            ))
        }))
    }

    /// Rewrite an open task's letter. `task` is its filename or bare id.
    pub fn reprioritize(&self, project: &str, task: &str, letter: &str) -> Change {
        let letter = letter.to_uppercase();
        if !is_letter(&letter) {
            return Err(
                "error: priority must be a single letter A-Z (see ~/repos/tasks/CLAUDE.md)".into(),
            );
        }
        let dir = self.project_dir(project)?;
        let project = project.to_string();
        let task = task.to_string();
        Ok(Box::new(move || {
            let (status, name) = resolve_open_task(&dir, &task)?;
            let old = name[..1].to_string();
            if old == letter {
                return Ok(Applied::nothing(format!(
                    "{project}/{name} already at priority {letter}\n"
                )));
            }
            let new_name = format!("{letter}{}", &name[1..]);
            let here = dir.join(status.dir());
            if here.join(&new_name).exists() {
                return Err(format!(
                    "error: {new_name} already exists in {}/ (duplicate ID?)",
                    status.dir()
                ));
            }
            fs::rename(here.join(&name), here.join(&new_name)).map_err(io_error)?;
            Ok(Applied::commit(
                format!("Reprioritize: {} {old}->{letter}", slug(&name)),
                move || {
                    format!(
                        "--- {project}/{name} -> {new_name} ({}) ---\n",
                        status.dir()
                    )
                },
            ))
        }))
    }

    /// Rewrite an open task's slug. `task` is its filename or bare id.
    pub fn rename(&self, project: &str, task: &str, title: &str) -> Change {
        let new_slug = slugify(title);
        if new_slug.is_empty() {
            return Err("error: title has no alphanumeric characters to slug".into());
        }
        let dir = self.project_dir(project)?;
        let project = project.to_string();
        let task = task.to_string();
        Ok(Box::new(move || {
            let (status, name) = resolve_open_task(&dir, &task)?;
            let old_slug = slug(&name);
            if old_slug == new_slug {
                return Ok(Applied::nothing(format!(
                    "{project}/{name} already has slug {new_slug}\n"
                )));
            }
            let new_name = format!("{}-{new_slug}.md", &name[..4]);
            let here = dir.join(status.dir());
            let target = here.join(&new_name);
            match target.symlink_metadata() {
                Ok(_) => {
                    return Err(format!(
                        "error: {new_name} already exists in {}/ (duplicate ID?)",
                        status.dir()
                    ));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_error(e)),
            }
            fs::rename(here.join(&name), target).map_err(io_error)?;
            Ok(Applied::commit(
                format!("Rename: {old_slug}->{new_slug}"),
                move || {
                    format!(
                        "--- {project}/{name} -> {new_name} ({}) ---\n",
                        status.dir()
                    )
                },
            ))
        }))
    }

    /// An empty reminder task in todo/, named by `title`. Prints its project and filename.
    /// `make` creates the project when it has none yet.
    pub fn create(&self, project: &str, make: bool, letter: &str, title: &str) -> Change {
        let letter = letter.to_uppercase();
        if !is_letter(&letter) {
            return Err("error: priority must be a single letter A-Z".into());
        }
        if title.trim().is_empty() {
            return Err("error: title required".into());
        }
        let slug = slugify(title);
        if slug.is_empty() {
            return Err("error: title has no alphanumeric characters to slug".into());
        }
        let dir = if make {
            self.root.join(project)
        } else {
            self.project_dir(project)?
        };
        let project = project.to_string();
        Ok(Box::new(move || {
            for s in Status::ALL {
                fs::create_dir_all(dir.join(s.dir())).map_err(io_error)?;
            }
            // Read after the pull and after any change ahead in the batch.
            let file = format!("{letter}{}-{slug}.md", next_id(&dir));
            fs::write(dir.join(Todo.dir()).join(&file), "").map_err(io_error)?;
            Ok(Applied::commit(format!("Add: {slug}"), move || {
                format!("--- Created: {project}/{file} ---\n")
            }))
        }))
    }

    /// The project's next free id, three digits.
    pub fn next_id(&self, project: &str) -> Result<String, String> {
        let dir = self.project_dir(project)?;
        Git::new(&self.root, &|| {}).pull();
        Ok(format!("{}\n", next_id(&dir)))
    }

    /// Commit whatever changed under the root.
    pub fn commit(&self, message: &str) -> Change {
        if !self.root.is_dir() {
            return Err(format!(
                "error: tasks root not found: {}",
                self.root.display()
            ));
        }
        let message = message.to_string();
        Ok(Box::new(move || Ok(Applied::commit(message, String::new))))
    }
}

/// Where a change in a batch got to.
enum Fate {
    Failed(String),
    /// Applied with nothing to commit.
    Untouched(Out),
    Committed(Out),
}

/// A batch's git commands, finished against expected, told to `report` at
/// the start, after each command, and whenever the total shrinks.
struct Counter<'a> {
    done: Cell<usize>,
    total: Cell<usize>,
    report: &'a dyn Fn(Progress),
}

impl<'a> Counter<'a> {
    fn new(total: usize, report: &'a dyn Fn(Progress)) -> Counter<'a> {
        let c = Counter {
            done: Cell::new(0),
            total: Cell::new(total),
            report,
        };
        c.tell();
        c
    }

    fn ran(&self) {
        self.done.set(self.done.get() + 1);
        self.tell();
    }

    /// Run `f`, expected to run `budget` git commands, and stop expecting
    /// those it did not, so the count still reaches the total.
    fn spend<T>(&self, budget: usize, f: impl FnOnce() -> T) -> T {
        let before = self.done.get();
        let out = f();
        let unspent = budget.saturating_sub(self.done.get() - before);
        if unspent > 0 {
            self.total.set(self.total.get() - unspent);
            self.tell();
        }
        out
    }

    fn tell(&self) {
        (self.report)(Progress {
            done: self.done.get(),
            total: self.total.get(),
        });
    }
}

/// Move `file` from one status to another, making the target's directory.
fn relocate(dir: &Path, file: &str, from: Status, to: Status) -> std::io::Result<PathBuf> {
    let dst_dir = dir.join(to.dir());
    fs::create_dir_all(&dst_dir)?;
    let dst = dst_dir.join(file);
    fs::rename(dir.join(from.dir()).join(file), &dst)?;
    Ok(dst)
}

fn io_error(e: std::io::Error) -> String {
    format!("error: {e}")
}

/// The file a task is named by: `N042-some-slug` and `N042-some-slug.md` alike.
pub fn task_filename(arg: &str) -> String {
    format!("{}.md", arg.strip_suffix(".md").unwrap_or(arg))
}

/// The filename without its `<Letter><NNN>-` prefix and `.md`.
fn slug(file: &str) -> &str {
    let name = file.strip_suffix(".md").unwrap_or(file);
    if is_task_name(name) { &name[5..] } else { name }
}

/// Starts `<Letter><NNN>-`.
fn is_task_name(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() >= 5
        && b[0].is_ascii_uppercase()
        && b[1..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
}

fn is_letter(s: &str) -> bool {
    s.len() == 1 && s.as_bytes()[0].is_ascii_uppercase()
}

/// `42` as `042`: up to three digits name a task by id alone.
fn bare_id(arg: &str) -> Option<String> {
    let ok = (1..=3).contains(&arg.len()) && arg.bytes().all(|b| b.is_ascii_digit());
    ok.then(|| format!("{:03}", arg.parse::<u32>().unwrap_or(0)))
}

fn resolve_open_task(dir: &Path, task: &str) -> Result<(Status, String), String> {
    let arg = Path::new(task)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (id, file) = (bare_id(&arg), task_filename(&arg));
    let wanted = |name: &str| match &id {
        Some(id) => name.ends_with(".md") && is_task_name(name) && name[1..4] == *id,
        None => name == file,
    };
    let mut matches = Vec::new();
    for status in [Todo, Planning, Planned, Active, Stale] {
        let mut names: Vec<String> = fs::read_dir(dir.join(status.dir()))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_file())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| wanted(n))
            .collect();
        names.sort();
        matches.extend(names.into_iter().map(|n| (status, n)));
    }
    let (status, name) = match &matches[..] {
        [] => {
            return Err(format!(
                "error: {task} not found in todo/, planning/, planned/, active/, or stale/"
            ));
        }
        [one] => one.clone(),
        _ => {
            let names: Vec<&str> = matches.iter().map(|(_, n)| n.as_str()).collect();
            return Err(format!(
                "error: {task} matches {} tasks: {}",
                matches.len(),
                names.join(" ")
            ));
        }
    };
    if !is_task_name(&name) {
        return Err(format!(
            "error: not a <letter><NNN>-slug.md task file: {name}"
        ));
    }
    Ok((status, name))
}

/// Lowercased, every run of anything but `a-z0-9` one hyphen, none at the ends.
pub fn slugify(title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars().map(|c| c.to_ascii_lowercase()) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_string()
}

/// One more than the highest id under any status, three digits.
fn next_id(dir: &Path) -> String {
    let max = Status::ALL
        .iter()
        .filter_map(|s| fs::read_dir(dir.join(s.dir())).ok())
        .flatten()
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".md") && is_task_name(n))
        .filter_map(|n| n[1..4].parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    format!("{:03}", max + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TasksRepo;
    use std::cell::RefCell;

    /// Publish `batch`, returning its results and every `done/total` it
    /// reported.
    fn publish(t: &Tasks, batch: Vec<Change>) -> (Vec<Result<String, String>>, Vec<String>) {
        let seen = RefCell::new(vec![]);
        let results = t.publish(batch, &|p| {
            seen.borrow_mut().push(format!("{}/{}", p.done, p.total));
        });
        (results, seen.into_inner())
    }

    fn counted(to: usize) -> Vec<String> {
        (0..=to).map(|n| format!("{n}/{to}")).collect()
    }

    #[test]
    fn a_batch_commits_each_change_and_pushes_once() {
        let r = TasksRepo::new(&["proj"]);
        r.seed("proj", "todo", "N001-x.md", "");
        r.count_pushes();
        let t = Tasks::new(r.root());
        let batch = vec![
            t.create("proj", false, "N", "one"),
            t.create("proj", false, "H", "two"),
        ];
        let (results, seen) = publish(&t, batch);
        assert_eq!(
            results,
            vec![
                Ok("--- Created: proj/N002-one.md ---\n".into()),
                Ok("--- Created: proj/H003-two.md ---\n".into()),
            ]
        );
        assert_eq!(r.subjects(2), ["Add: two", "Add: one"]);
        assert!(r.pushed());
        assert_eq!(r.pushes(), 1);
        // A pull, an add and a commit for each, a pull and a push.
        assert_eq!(seen, counted(7));
    }

    #[test]
    fn a_batch_of_one_counts_five_git_commands() {
        let r = TasksRepo::new(&["proj"]);
        let t = Tasks::new(r.root());
        let (results, seen) = publish(&t, vec![t.create("proj", false, "N", "one")]);
        assert_eq!(
            results,
            vec![Ok("--- Created: proj/N001-one.md ---\n".into())]
        );
        assert_eq!(seen, counted(5));
    }

    #[test]
    fn a_failing_change_fails_alone() {
        let r = TasksRepo::new(&["proj"]);
        r.count_pushes();
        let t = Tasks::new(r.root());
        let batch = vec![
            t.create("proj", false, "N", "one"),
            t.move_task("proj", "N404-gone", "done"),
            t.commit("Edit: nothing"),
            t.reprioritize("proj", "1", "7"),
            t.create("proj", false, "N", "two"),
        ];
        let (mut results, seen) = publish(&t, batch);
        let nothing = results[2].clone().unwrap_err();
        assert!(
            nothing.ends_with("nothing to commit, working tree clean"),
            "{nothing}"
        );
        results[2] = Err(String::new());
        assert_eq!(
            results,
            vec![
                Ok("--- Created: proj/N001-one.md ---\n".into()),
                Err("error: N404-gone.md not found in proj".into()),
                Err(String::new()),
                Err(
                    "error: priority must be a single letter A-Z (see ~/repos/tasks/CLAUDE.md)"
                        .into()
                ),
                Ok("--- Created: proj/N002-two.md ---\n".into()),
            ]
        );
        assert_eq!(r.subjects(2), ["Add: two", "Add: one"]);
        assert!(r.pushed());
        assert_eq!(r.pushes(), 1);
        // The refused reprioritize is never counted; the failed move gives
        // back its add and commit, so the count still reaches the total.
        assert_eq!(
            seen,
            [
                "0/11", "1/11", "2/11", "3/11", "3/9", "4/9", "5/9", "6/9", "7/9", "8/9", "9/9"
            ]
        );
    }

    #[test]
    fn a_failed_push_fails_every_change_that_committed() {
        let r = TasksRepo::new(&["proj"]);
        r.seed("proj", "todo", "N001-x.md", "");
        r.refuse_pushes();
        let t = Tasks::new(r.root());
        let batch = vec![
            t.create("proj", false, "N", "one"),
            t.reprioritize("proj", "1", "N"),
            t.create("proj", false, "N", "!!"),
            t.create("proj", false, "N", "two"),
        ];
        let (results, seen) = publish(&t, batch);
        let refused = results[0].clone().unwrap_err();
        assert!(refused.contains("pre-receive hook declined"), "{refused}");
        assert_eq!(
            results,
            vec![
                Err(refused.clone()),
                Ok("proj/N001-x.md already at priority N\n".into()),
                Err("error: title has no alphanumeric characters to slug".into()),
                Err(refused),
            ]
        );
        // The commits wait for the next push.
        assert_eq!(r.subjects(2), ["Add: two", "Add: one"]);
        assert!(!r.pushed());
        // The no-op gives back its add and commit; the refused create never counts.
        assert_eq!(
            seen,
            [
                "0/9", "1/9", "2/9", "3/9", "3/7", "4/7", "5/7", "6/7", "7/7"
            ]
        );
    }

    #[test]
    fn an_unreachable_origin_still_counts_to_the_total() {
        let r = TasksRepo::new(&["proj"]);
        let gone = r.root().join("gone.git");
        crate::testutil::git(
            &r.root(),
            &["remote", "set-url", "origin", &gone.to_string_lossy()],
        );
        let t = Tasks::new(r.root());
        let (results, seen) = publish(&t, vec![t.create("proj", false, "N", "one")]);
        assert!(results[0].is_err(), "{results:?}");
        assert_eq!(r.subjects(1), ["Add: one"]);
        // The failed first pull counts; the push after the failed final pull
        // is given back.
        assert_eq!(seen, ["0/5", "1/5", "2/5", "3/5", "4/5", "4/4"]);
    }

    #[test]
    fn a_failed_check_runs_no_git() {
        let r = TasksRepo::new(&["proj"]);
        let head = r.subjects(1);
        let t = Tasks::new(r.root());
        let batch = vec![
            t.create("proj", false, "1", "one"),
            t.move_task("proj", "N001-x", "active"),
            t.claim("nope", "N001-x"),
        ];
        let (results, seen) = publish(&t, batch);
        assert_eq!(
            results,
            vec![
                Err("error: priority must be a single letter A-Z".into()),
                Err("error: active/ is a claim, made by taskania claim".into()),
                Err(format!(
                    "error: tasks dir not found: {}",
                    r.root().join("nope").display()
                )),
            ]
        );
        assert!(seen.is_empty(), "{seen:?}");
        assert_eq!(r.subjects(1), head);
    }

    #[test]
    fn a_change_to_nothing_pulls_but_does_not_push() {
        let r = TasksRepo::new(&["proj"]);
        r.seed("proj", "todo", "N001-x.md", "");
        r.count_pushes();
        let t = Tasks::new(r.root());
        let (results, seen) = publish(&t, vec![t.reprioritize("proj", "1", "n")]);
        assert_eq!(
            results,
            vec![Ok("proj/N001-x.md already at priority N\n".into())]
        );
        assert_eq!(seen, ["0/5", "1/5", "1/3", "1/1"]);
        assert_eq!(r.subjects(1), ["seed N001-x.md"]);
        assert_eq!(r.pushes(), 0);
    }

    #[test]
    fn a_title_slugs_to_lowercase_words_joined_by_hyphens() {
        assert_eq!(slugify("Fix the thing"), "fix-the-thing");
        assert_eq!(slugify("Fix: the (API)!!"), "fix-the-api");
        assert_eq!(slugify("  --a__b--  "), "a-b");
        assert_eq!(slugify("über 99%"), "ber-99");
        assert_eq!(slugify("!!!"), "");
    }

    #[test]
    fn a_task_is_named_with_or_without_md() {
        assert_eq!(task_filename("N042-x"), "N042-x.md");
        assert_eq!(task_filename("N042-x.md"), "N042-x.md");
        assert_eq!(slug("N042-some-slug.md"), "some-slug");
        assert_eq!(slug("notes.md"), "notes");
    }

    #[test]
    fn a_bare_id_is_up_to_three_digits() {
        assert_eq!(bare_id("42").as_deref(), Some("042"));
        assert_eq!(bare_id("007").as_deref(), Some("007"));
        assert_eq!(bare_id("1234"), None);
        assert_eq!(bare_id("N042"), None);
    }
}
