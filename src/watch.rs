//! Change detection: a signature of the watched projects' task files, compared
//! every poll. Native and git-free, so it never contends for the tasks repo.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// What a file looks like from outside: enough to tell it changed.
pub type Stamp = (SystemTime, u64);

/// Path, mtime and size of every `<project>/*/*.md`, sorted.
pub fn signature(projects: &[PathBuf]) -> Vec<(PathBuf, Stamp)> {
    let mut sig = Vec::new();
    for project in projects {
        let Ok(dirs) = fs::read_dir(project) else {
            continue;
        };
        for dir in dirs.flatten() {
            let Ok(files) = fs::read_dir(dir.path()) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().is_some_and(|e| e == "md")
                    && let Some(stamp) = stamp(&path)
                {
                    sig.push((path, stamp));
                }
            }
        }
    }
    sig.sort();
    sig
}

pub fn stamp(path: &Path) -> Option<Stamp> {
    let m = fs::metadata(path).ok()?;
    m.is_file()
        .then(|| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn a_change_to_any_task_file_changes_the_signature() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(proj.join("todo")).unwrap();
        fs::create_dir_all(proj.join("done")).unwrap();
        fs::write(proj.join("todo/N001-a.md"), "x").unwrap();
        fs::write(proj.join("todo/notes.txt"), "x").unwrap();
        let dirs = vec![proj.clone()];
        let before = signature(&dirs);
        assert_eq!(before.len(), 1);

        fs::rename(proj.join("todo/N001-a.md"), proj.join("done/N001-a.md")).unwrap();
        let moved = signature(&dirs);
        assert_ne!(moved, before);

        fs::write(proj.join("done/N001-a.md"), "xy").unwrap();
        assert_ne!(signature(&dirs), moved);
        assert_eq!(signature(&dirs), signature(&dirs));
    }

    #[test]
    fn a_file_outside_the_watched_projects_does_not() {
        let tmp = TempDir::new();
        fs::create_dir_all(tmp.path().join("proj/todo")).unwrap();
        fs::create_dir_all(tmp.path().join("other/todo")).unwrap();
        let dirs = vec![tmp.path().join("proj")];
        let before = signature(&dirs);
        fs::write(tmp.path().join("other/todo/N001-a.md"), "x").unwrap();
        assert_eq!(signature(&dirs), before);
    }
}
