//! Which project a directory belongs to.
//!
//! A `.find-project.conf` in the directory or its nearest ancestor holding one
//! names it outright. Otherwise the git checkout's directory name does, lab
//! suffix stripped, mapped through the projects conf, else lowercased.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const OVERRIDE_FILE: &str = ".find-project.conf";

/// Lab checkouts are named `<repo>-<suffix>`; the suffix is not the project.
/// Each strips from its first occurrence on. `-hwt` counts only before a digit.
const LAB_SUFFIXES: [&str; 6] = ["-hwt", "-dwt", "-cwt", "-hlab-", "-dlab-", "-clab-"];

/// The project `cwd` belongs to. `cwd` is the logical path, symlinks kept, so
/// a symlinked checkout answers to its own name.
pub fn resolve(cwd: &Path, projects_conf: &Path) -> Result<String, String> {
    if let Some(name) = override_name(cwd) {
        return Ok(name);
    }
    let toplevel = toplevel(cwd).ok_or("error: not in a git repo")?;
    let top = logical_top(cwd, &toplevel);
    let base = top
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stripped = strip_lab_suffix(&base);
    Ok(mapped(projects_conf, stripped).unwrap_or_else(|| stripped.to_lowercase()))
}

/// The name in the nearest `.find-project.conf`: its `project = NAME` line,
/// else its first line that is neither blank nor a comment. A file naming
/// nothing ends the search without an answer.
fn override_name(cwd: &Path) -> Option<String> {
    let file = cwd
        .ancestors()
        .take_while(|d| d.parent().is_some())
        .map(|d| d.join(OVERRIDE_FILE))
        .find(|f| f.is_file())?;
    let text = fs::read_to_string(file).ok()?;
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    let keyed = lines.iter().find_map(|l| {
        let (key, value) = l.split_once('=')?;
        (key.trim() == "project").then(|| value.rsplit('=').next().unwrap_or(value))
    });
    let name = keyed
        .or_else(|| lines.iter().copied().find(|l| !l.trim().is_empty()))?
        .trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn toplevel(cwd: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let top = String::from_utf8(out.stdout).ok()?;
    let top = top.trim_end_matches('\n');
    (out.status.success() && !top.is_empty()).then(|| PathBuf::from(top))
}

/// The nearest ancestor of `cwd` (itself included) that resolves to the
/// toplevel, else the toplevel.
fn logical_top(cwd: &Path, toplevel: &Path) -> PathBuf {
    let real_top = fs::canonicalize(toplevel).unwrap_or_else(|_| toplevel.to_path_buf());
    cwd.ancestors()
        .take_while(|d| d.parent().is_some())
        .find(|d| fs::canonicalize(d).is_ok_and(|r| r == real_top))
        .unwrap_or(toplevel)
        .to_path_buf()
}

pub fn strip_lab_suffix(name: &str) -> &str {
    let mut name = name;
    for suffix in LAB_SUFFIXES {
        let digit_only = suffix == "-hwt";
        let cut = name.match_indices(suffix).map(|(i, _)| i).find(|&i| {
            !digit_only
                || name[i + suffix.len()..]
                    .bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_digit())
        });
        if let Some(i) = cut {
            name = &name[..i];
        }
    }
    name
}

/// `name`'s project in the conf: its `name = project` line, `#` comments
/// skipped. A missing conf maps nothing.
fn mapped(conf: &Path, name: &str) -> Option<String> {
    let text = fs::read_to_string(conf).ok()?;
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| {
            let rest = l.strip_prefix(name)?.trim_start_matches(' ');
            rest.strip_prefix('=')?;
            let value = l.rsplit('=').next()?.trim();
            Some(value.to_string())
        })
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::os::unix::fs::symlink;

    fn git_init(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        let ok = Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }

    fn conf(tmp: &TempDir, text: &str) -> PathBuf {
        let p = tmp.path().join("projects.conf");
        fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn a_checkout_is_named_by_its_directory_lowercased() {
        let tmp = TempDir::new();
        let repo = tmp.path().join("MyRepo");
        git_init(&repo);
        fs::create_dir_all(repo.join("src/deep")).unwrap();
        let none = tmp.path().join("absent.conf");
        assert_eq!(resolve(&repo, &none), Ok("myrepo".into()));
        assert_eq!(resolve(&repo.join("src/deep"), &none), Ok("myrepo".into()));
    }

    #[test]
    fn lab_suffixes_are_stripped() {
        for (name, want) in [
            ("dotfiles-hlab-054-taskania-two-modes", "dotfiles"),
            ("dotfiles-dlab-tmp3", "dotfiles"),
            ("dotfiles-clab-012-x", "dotfiles"),
            ("dotfiles-hwt4", "dotfiles"),
            ("dotfiles-hwtx", "dotfiles-hwtx"),
            ("dotfiles-dwt", "dotfiles"),
            ("dotfiles-cwt2-x", "dotfiles"),
            ("my-repo", "my-repo"),
        ] {
            assert_eq!(strip_lab_suffix(name), want, "{name}");
        }
    }

    #[test]
    fn the_projects_conf_maps_a_stripped_name() {
        let tmp = TempDir::new();
        let repo = tmp.path().join("Widget-hlab-007-thing");
        git_init(&repo);
        let c = conf(
            &tmp,
            "# comment\n#Widget = commented\nWidgetx = wrong\nWidget = gadget\nWidget = later\n",
        );
        assert_eq!(resolve(&repo, &c), Ok("gadget".into()));
        let c = conf(&tmp, "Widget=gadget\n");
        assert_eq!(resolve(&repo, &c), Ok("gadget".into()));
        let c = conf(&tmp, "other = x\n");
        assert_eq!(resolve(&repo, &c), Ok("widget".into()));
    }

    #[test]
    fn an_override_file_wins_and_walks_up() {
        let tmp = TempDir::new();
        let repo = tmp.path().join("repo");
        git_init(&repo);
        fs::create_dir_all(repo.join("a/b")).unwrap();
        let none = tmp.path().join("absent.conf");
        fs::write(repo.join(OVERRIDE_FILE), "# c\nfirst\nproject = named\n").unwrap();
        assert_eq!(resolve(&repo.join("a/b"), &none), Ok("named".into()));
        fs::write(repo.join(OVERRIDE_FILE), "\n# c\n  bare  \nmore\n").unwrap();
        assert_eq!(resolve(&repo.join("a"), &none), Ok("bare".into()));
        fs::write(repo.join("a/").join(OVERRIDE_FILE), "# nothing\n\n").unwrap();
        assert_eq!(
            resolve(&repo.join("a/b"), &none),
            Ok("repo".into()),
            "a file naming nothing falls through to the checkout, not to the next file up"
        );
    }

    #[test]
    fn an_override_needs_no_git() {
        let tmp = TempDir::new();
        let dir = tmp.path().join("plain");
        fs::create_dir_all(&dir).unwrap();
        let none = tmp.path().join("absent.conf");
        assert_eq!(resolve(&dir, &none), Err("error: not in a git repo".into()));
        fs::write(dir.join(OVERRIDE_FILE), "project = widget\n").unwrap();
        assert_eq!(resolve(&dir, &none), Ok("widget".into()));
    }

    #[test]
    fn a_symlinked_checkout_keeps_its_own_name() {
        let tmp = TempDir::new();
        let repo = tmp.path().join("real");
        git_init(&repo);
        fs::create_dir_all(repo.join("sub")).unwrap();
        let link = tmp.path().join("linked-hwt3");
        symlink(&repo, &link).unwrap();
        let c = conf(&tmp, "linked = via-link\nreal = via-real\n");
        assert_eq!(resolve(&link.join("sub"), &c), Ok("via-link".into()));
        assert_eq!(resolve(&repo, &c), Ok("via-real".into()));
    }
}
