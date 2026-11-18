//! Every change to task data. A task is a markdown file under
//! `<root>/<project>/<status>/`, named `<Letter><NNN>-<slug>.md`; a change
//! moves, renames or creates one, then commits and pushes the tasks repo.
//!
//! Each returns what the verb prints on success, or its error line.

use std::fs;
use std::path::{Path, PathBuf};

use crate::git::Git;
use crate::listing::{Refusal, Status};

use Status::*;

#[derive(Clone)]
pub struct Tasks {
    root: PathBuf,
}

impl Tasks {
    pub fn new(root: PathBuf) -> Tasks {
        Tasks { root }
    }

    fn git(&self) -> Git {
        Git::new(&self.root)
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

    /// todo -> planning; planned or stale -> active. Prints a banner, then the
    /// task.
    pub fn claim(&self, project: &str, file: &str) -> Result<String, String> {
        let file = task_filename(file);
        let dir = self.project_dir(project)?;
        let git = self.git();
        git.pull();
        let (from, to) = [(Todo, Planning), (Planned, Active), (Stale, Active)]
            .into_iter()
            .find(|(from, _)| dir.join(from.dir()).join(&file).is_file())
            .ok_or_else(|| format!("error: {file} not found in todo/, planned/, or stale/"))?;
        let dst = relocate(&dir, &file, from, to)
            .map_err(|e| format!("error: {e}\nerror: mv failed (race condition?)"))?;
        git.publish(&format!("Claim: {}", slug(&file)))?;
        let body = fs::read_to_string(&dst).unwrap_or_default();
        Ok(format!("--- Claimed: {file} ---\n\n{body}"))
    }

    /// planning -> planned.
    pub fn planned(&self, project: &str, file: &str) -> Result<String, String> {
        let file = task_filename(file);
        let dir = self.project_dir(project)?;
        let git = self.git();
        git.pull();
        let src = dir.join(Planning.dir()).join(&file);
        if !src.is_file() {
            return Err(format!("error: not found: {}", src.display()));
        }
        relocate(&dir, &file, Planning, Planned).map_err(io_error)?;
        git.publish(&format!("Plan: {}", slug(&file)))?;
        Ok(String::new())
    }

    /// planning -> todo; active -> planned.
    pub fn unclaim(&self, project: &str, file: &str) -> Result<String, String> {
        let file = task_filename(file);
        let dir = self.project_dir(project)?;
        let git = self.git();
        git.pull();
        let (from, to) = [(Planning, Todo), (Active, Planned)]
            .into_iter()
            .find(|(from, _)| dir.join(from.dir()).join(&file).is_file())
            .ok_or_else(|| format!("error: {file} not found in planning/ or active/"))?;
        relocate(&dir, &file, from, to).map_err(io_error)?;
        git.publish(&format!("Unclaim: {}", slug(&file)))?;
        Ok(format!("--- Unclaimed: {file} ---\n"))
    }

    /// active -> done; `force` takes it from any other status.
    pub fn done(&self, project: &str, file: &str, force: bool) -> Result<String, String> {
        let file = task_filename(file);
        let dir = self.project_dir(project)?;
        let git = self.git();
        git.pull();
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
        git.publish(&format!("Done: {}", slug(&file)))?;
        Ok(String::new())
    }

    /// A manual move, by [`Status::move_refusal`]'s rule.
    pub fn move_task(&self, project: &str, file: &str, to: &str) -> Result<String, String> {
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
        let git = self.git();
        git.pull();
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
        git.publish(&format!("Move: {} {f}->{t}", slug(&file)))?;
        Ok(format!("--- Moved: {file} {f} -> {t} ---\n"))
    }

    /// Rewrite an open task's letter. `task` is its filename or bare id.
    pub fn reprioritize(&self, project: &str, task: &str, letter: &str) -> Result<String, String> {
        let letter = letter.to_uppercase();
        if !is_letter(&letter) {
            return Err(
                "error: priority must be a single letter A-Z (see ~/repos/tasks/CLAUDE.md)".into(),
            );
        }
        let dir = self.project_dir(project)?;
        let git = self.git();
        git.pull();
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
        let old = &name[..1];
        if old == letter {
            return Ok(format!("{name} already at priority {letter}\n"));
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
        git.publish(&format!("Reprioritize: {} {old}->{letter}", slug(&name)))?;
        Ok(format!("--- {name} -> {new_name} ({}) ---\n", status.dir()))
    }

    /// An empty reminder task in todo/, named by `title`. Prints its filename.
    /// `make` creates the project when it has none yet.
    pub fn create(
        &self,
        project: &str,
        make: bool,
        letter: &str,
        title: &str,
    ) -> Result<String, String> {
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
        let git = self.git();
        git.pull();
        for s in Status::ALL {
            fs::create_dir_all(dir.join(s.dir())).map_err(io_error)?;
        }
        let file = format!("{letter}{}-{slug}.md", next_id(&dir));
        fs::write(dir.join(Todo.dir()).join(&file), "").map_err(io_error)?;
        git.publish(&format!("Add: {slug}"))?;
        Ok(format!("{file}\n"))
    }

    /// The project's next free id, three digits.
    pub fn next_id(&self, project: &str) -> Result<String, String> {
        let dir = self.project_dir(project)?;
        self.git().pull();
        Ok(format!("{}\n", next_id(&dir)))
    }

    /// Commit whatever changed under the root, and push it.
    pub fn commit(&self, message: &str) -> Result<String, String> {
        if !self.root.is_dir() {
            return Err(format!(
                "error: tasks root not found: {}",
                self.root.display()
            ));
        }
        let git = self.git();
        git.pull();
        git.publish(message)?;
        Ok(String::new())
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
