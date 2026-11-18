//! The binary from outside: every verb against a real tasks repo with its own
//! origin (each change pulls and pushes), run from a checkout naming its
//! project through `.find-project.conf`. PATH holds only the base system and
//! HOME is a temp dir, so nothing reaches an installed script or the real
//! tasks repo.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

const BIN: &str = env!("CARGO_BIN_EXE_taskania");

const STATUSES: [&str; 7] = [
    "todo", "planning", "planned", "active", "stale", "done", "canceled",
];

struct Sandbox {
    root: PathBuf,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

impl Sandbox {
    /// A tasks repo holding projects `widget` and `proj`, and a checkout of
    /// `widget` to run in.
    fn new() -> Sandbox {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("taskania-cli-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for d in ["home", "elsewhere"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        let s = Sandbox { root };
        git(&s.root, &["init", "-q", "--bare", "origin.git"]);
        git(&s.root, &["clone", "-q", "origin.git", "tasks"]);
        let tasks = s.tasks();
        git(&tasks, &["config", "user.email", "t@t"]);
        git(&tasks, &["config", "user.name", "T"]);
        for p in ["widget", "proj"] {
            for st in STATUSES {
                fs::create_dir_all(tasks.join(p).join(st)).unwrap();
            }
        }
        fs::write(tasks.join("README.md"), "tasks\n").unwrap();
        git(&tasks, &["add", "-A"]);
        git(&tasks, &["commit", "-qm", "init"]);
        git(&tasks, &["branch", "-M", "main"]);
        git(&tasks, &["push", "-qu", "origin", "main"]);
        s.checkout("widget", Some("project = widget\n"));
        s
    }

    fn tasks(&self) -> PathBuf {
        self.root.join("tasks")
    }

    /// A git checkout under the sandbox, its project named by an override
    /// file when given.
    fn checkout(&self, name: &str, conf: Option<&str>) -> PathBuf {
        let dir = self.root.join(name);
        fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        if let Some(c) = conf {
            fs::write(dir.join(".find-project.conf"), c).unwrap();
        }
        dir
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_in(&self.root.join("widget"), args)
    }

    fn run_in(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.root.join("home"))
            .env("TASKS_ROOT", self.tasks())
            .env("TASKANIA_PROJECTS_CONF", self.root.join("projects.conf"))
            .env("PWD", cwd)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .current_dir(cwd)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    /// Run, expecting success; returns stdout.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
        text(&out.stdout)
    }

    /// Run, expecting a refusal that says why and commits nothing; returns
    /// the error.
    fn refused(&self, args: &[&str]) -> String {
        let before = self.head();
        let out = self.run(args);
        let err = text(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{args:?} succeeded");
        assert!(!err.trim().is_empty(), "{args:?} says why");
        assert_eq!(self.head(), before, "{args:?} commits nothing");
        err.trim_end().to_string()
    }

    /// A task file in `widget`, committed and pushed.
    fn seed(&self, status: &str, file: &str, body: &str) {
        self.seed_in("widget", status, file, body);
    }

    fn seed_in(&self, project: &str, status: &str, file: &str, body: &str) {
        let tasks = self.tasks();
        fs::write(tasks.join(project).join(status).join(file), body).unwrap();
        git(&tasks, &["add", "-A"]);
        git(&tasks, &["commit", "-qm", &format!("seed {file}")]);
        git(&tasks, &["push", "-q"]);
    }

    fn at(&self, status: &str, file: &str) -> bool {
        self.tasks()
            .join("widget")
            .join(status)
            .join(file)
            .is_file()
    }

    fn body(&self, status: &str, file: &str) -> String {
        fs::read_to_string(self.tasks().join("widget").join(status).join(file)).unwrap()
    }

    fn head(&self) -> String {
        git(&self.tasks(), &["rev-parse", "HEAD"])
    }

    fn subject(&self) -> String {
        git(&self.tasks(), &["log", "-1", "--format=%s"])
    }

    fn pushed(&self) -> bool {
        self.head() == git(&self.tasks(), &["rev-parse", "origin/main"])
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn age(path: &Path, days: u64) {
    let t = SystemTime::now() - Duration::from_secs(days * 86_400);
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

const VERBS: [&str; 11] = [
    "list",
    "claim",
    "planned",
    "unclaim",
    "done",
    "move",
    "reprioritize",
    "create",
    "next-id",
    "commit",
    "project",
];

// --- help ---------------------------------------------------------------------

#[test]
fn bare_and_help_print_the_help_and_open_nothing() {
    let s = Sandbox::new();
    for args in [&[][..], &["--help"], &["-h"]] {
        let out = s.run_in(&s.root.join("elsewhere"), args);
        assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
        let help = text(&out.stdout);
        assert!(help.starts_with("Taskania"), "{help}");
        assert!(help.lines().any(|l| l.starts_with("Usage: taskania")));
        for verb in VERBS {
            assert!(help.contains(&format!("\n  {verb} ")), "{verb} listed");
        }
    }
}

#[test]
fn every_verb_explains_itself_without_a_project() {
    let s = Sandbox::new();
    for verb in VERBS {
        let out = s.run_in(&s.root.join("elsewhere"), &[verb, "--help"]);
        assert!(out.status.success(), "{verb}");
        let help = text(&out.stdout);
        let usage = format!("Usage: taskania {verb}");
        assert!(help.lines().nth(1).unwrap().starts_with(&usage), "{help}");
    }
    let out = s.run(&["--watch", "--help"]);
    assert!(out.status.success());
    assert!(text(&out.stdout).contains("Usage: taskania --watch"));
}

// --- the dashboard ------------------------------------------------------------

#[test]
fn the_dashboard_takes_only_with() {
    let s = Sandbox::new();
    for flag in [
        "--project",
        "--recent",
        "--poll",
        "--dates",
        "--labs",
        "--stale",
        "--once",
    ] {
        let out = s.run(&["--watch", flag, "1"]);
        assert_eq!(out.status.code(), Some(1), "{flag}");
        assert_eq!(
            text(&out.stderr),
            format!("taskania: unexpected argument: {flag}\n")
        );
    }
}

#[test]
fn the_dashboard_checks_its_projects_then_needs_a_terminal() {
    let s = Sandbox::new();
    let err = |args: &[&str], cwd: &Path| {
        let out = s.run_in(cwd, args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        text(&out.stderr)
    };
    let widget = s.root.join("widget");
    assert!(err(&["--watch", "--with", "nope"], &widget).contains("tasks dir not found"));
    assert!(err(&["--watch"], &s.root.join("elsewhere")).contains("not in a git repo"));
    let gone = s.checkout("gone", Some("project = gone\n"));
    assert!(err(&["--watch"], &gone).contains("tasks dir not found"));
    assert!(err(&["--watch", "--with", "proj"], &widget).contains("not a terminal"));
}

#[test]
fn unknown_verbs_and_flags_are_errors() {
    let s = Sandbox::new();
    for (args, why) in [
        (&["bogus"][..], "taskania: unknown verb: bogus"),
        (&["--bogus"], "taskania: unexpected argument: --bogus"),
        (
            &["list", "widget"],
            "taskania: list: unexpected argument: widget",
        ),
        (
            &["list", "--no-workers"],
            "taskania: list: unexpected argument: --no-workers",
        ),
        (&["claim"], "taskania: claim needs FILE"),
        (
            &["commit", "--project", "widget", "m"],
            "taskania: commit: unexpected argument: --project",
        ),
        (
            &["project", "--project", "widget"],
            "taskania: project: unexpected argument: --project",
        ),
    ] {
        let out = s.run(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(
            text(&out.stderr).starts_with(why),
            "{args:?}: {}",
            text(&out.stderr)
        );
    }
}

// --- project ------------------------------------------------------------------

#[test]
fn project_names_the_cwds_project() {
    let s = Sandbox::new();
    assert_eq!(s.ok(&["project"]), "widget\n");
    fs::create_dir_all(s.root.join("widget/deep/er")).unwrap();
    let out = s.run_in(&s.root.join("widget/deep/er"), &["project"]);
    assert_eq!(text(&out.stdout), "widget\n");

    let lab = s.checkout("Gadget-hlab-054-some-slug", None);
    assert_eq!(text(&s.run_in(&lab, &["project"]).stdout), "gadget\n");
    fs::write(s.root.join("projects.conf"), "# map\nGadget = widget\n").unwrap();
    assert_eq!(text(&s.run_in(&lab, &["project"]).stdout), "widget\n");

    let out = s.run_in(&s.root.join("elsewhere"), &["project"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stderr), "error: not in a git repo\n");
}

#[test]
fn the_projects_conf_defaults_to_the_tasks_root() {
    let s = Sandbox::new();
    let lab = s.checkout("Gadget-dlab-007", None);
    fs::write(s.tasks().join("projects.conf"), "Gadget = proj\n").unwrap();
    let out = Command::new(BIN)
        .arg("project")
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", s.root.join("home"))
        .env("TASKS_ROOT", s.tasks())
        .env_remove("TASKANIA_PROJECTS_CONF")
        .env_remove("PWD")
        .current_dir(&lab)
        .output()
        .unwrap();
    assert_eq!(text(&out.stdout), "proj\n", "{}", text(&out.stderr));
}

// --- list ---------------------------------------------------------------------

/// A task in every open status, done and canceled ones of different ages.
fn listed(s: &Sandbox) {
    let w = s.tasks().join("widget");
    for (status, file, body, days) in [
        ("todo", "H003-fix-sensor-bug.md", "# Sensor\n", 0),
        ("todo", "U012-update-readme.md", "# Readme\n", 0),
        ("planned", "N007-refactor-api.md", "# Api\n", 0),
        (
            "active",
            "H001-being-done.md",
            "# Being\n\nWorker: hlab-9\n",
            0,
        ),
        ("stale", "H002-paused.md", "# Paused\n\nWorker: hlab-5\n", 0),
        ("done", "N020-recent-done.md", "# Shipped\n", 2),
        ("done", "N021-old-done.md", "# Old\n", 30),
        ("canceled", "N022-recent-cancel.md", "# Dropped\n", 1),
    ] {
        fs::write(w.join(status).join(file), body).unwrap();
        age(&w.join(status).join(file), days);
    }
}

#[test]
fn list_shows_the_open_queue_under_a_header_without_workers() {
    let s = Sandbox::new();
    listed(&s);
    assert_eq!(
        s.ok(&["list"]),
        "Tasks: widget\n\nACTIVE (1)\n  H001 being-done\nPLANNED (1)\n  N007 refactor-api\n\
         STALE (1)\n  H002 paused\nTODO (2)\n  H003 fix-sensor-bug\n  U012 update-readme\n"
    );
    assert_eq!(
        s.ok(&[
            "list",
            "--no-header",
            "--no-stale",
            "--status",
            "active",
            "stale"
        ]),
        "ACTIVE (1)\n  H001 being-done\n"
    );
    assert_eq!(
        s.ok(&["list", "--status", "todo", "planned", "--ids"]),
        "H003-fix-sensor-bug\nU012-update-readme\nN007-refactor-api\n"
    );
    assert_eq!(
        s.ok(&["list", "--project", "proj"]),
        "Tasks: proj\n\n(none)\n"
    );
    assert_eq!(s.ok(&["list", "--project", "proj", "--ids"]), "");
}

#[test]
fn list_takes_the_view_flags() {
    let s = Sandbox::new();
    listed(&s);
    let recent = s.ok(&["list", "--no-header", "--recent", "7"]);
    assert!(
        recent.ends_with("  U012 update-readme\n\nDONE (1)\n  N020 recent-done\nCANCELED (1)\n  N022 recent-cancel\n"),
        "{recent}"
    );
    let all = s.ok(&["list", "--no-header", "--all"]);
    let labels: Vec<&str> = all.lines().filter(|l| !l.starts_with(' ')).collect();
    assert_eq!(
        labels,
        [
            "CANCELED (1)",
            "DONE (2)",
            "ACTIVE (1)",
            "PLANNED (1)",
            "STALE (1)",
            "TODO (2)"
        ]
    );
    let dated = s.ok(&["list", "--no-header", "--status", "planned", "--dates"]);
    assert!(
        dated.starts_with("PLANNED (1)\n  N007 refactor-api ("),
        "{dated}"
    );
    assert_eq!(
        s.ok(&["list", "--no-header", "--status", "planned", "--verbose"]),
        "PLANNED (1)\n  N007 refactor-api\n\n    # Api\n\n"
    );
    let out = s.run(&["list", "--project", "nope"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).starts_with("error: tasks dir not found: "));
}

// --- claim, planned, unclaim, done --------------------------------------------

#[test]
fn claim_takes_todo_to_planning_and_planned_or_stale_to_active() {
    let s = Sandbox::new();
    s.seed("todo", "N001-raw.md", "# Raw\n\nbody\n");
    let out = s.ok(&["claim", "N001-raw"]);
    assert_eq!(out, "--- Claimed: N001-raw.md ---\n\n# Raw\n\nbody\n");
    assert!(s.at("planning", "N001-raw.md") && !s.at("todo", "N001-raw.md"));
    assert_eq!(
        s.body("planning", "N001-raw.md"),
        "# Raw\n\nbody\n",
        "no worker stamped"
    );
    assert_eq!(s.subject(), "Claim: raw");
    assert!(s.pushed());

    s.seed("planned", "N002-groomed.md", "");
    s.ok(&["claim", "N002-groomed.md"]);
    assert!(s.at("active", "N002-groomed.md"));

    s.seed("stale", "N003-paused.md", "# P\nWorker: hlab-5\n");
    s.ok(&["claim", "N003-paused"]);
    assert_eq!(
        s.body("active", "N003-paused.md"),
        "# P\nWorker: hlab-5\n",
        "an old worker line is body text, left alone"
    );

    assert_eq!(
        s.refused(&["claim", "N099-no-such-task"]),
        "error: N099-no-such-task.md not found in todo/, planned/, or stale/"
    );
    s.seed("done", "N004-finished.md", "");
    s.refused(&["claim", "N004-finished"]);
}

#[test]
fn planned_parks_a_planning_task() {
    let s = Sandbox::new();
    s.seed("planning", "N001-grilled.md", "# G\nWorker: hlab-1\n");
    assert_eq!(s.ok(&["planned", "N001-grilled"]), "");
    assert_eq!(
        s.body("planned", "N001-grilled.md"),
        "# G\nWorker: hlab-1\n"
    );
    assert_eq!(s.subject(), "Plan: grilled");
    assert!(s.pushed());
    let path = s.tasks().join("widget/planning/N001-grilled.md");
    assert_eq!(
        s.refused(&["planned", "N001-grilled.md"]),
        format!("error: not found: {}", path.display())
    );
}

#[test]
fn unclaim_returns_a_claim_where_it_came_from() {
    let s = Sandbox::new();
    s.seed("planning", "N001-mid-grill.md", "");
    assert_eq!(
        s.ok(&["unclaim", "N001-mid-grill"]),
        "--- Unclaimed: N001-mid-grill.md ---\n"
    );
    assert!(s.at("todo", "N001-mid-grill.md"));
    assert_eq!(s.subject(), "Unclaim: mid-grill");

    s.seed("active", "N002-building.md", "");
    s.ok(&["unclaim", "N002-building.md"]);
    assert!(s.at("planned", "N002-building.md"));
    assert!(s.pushed());

    assert_eq!(
        s.refused(&["unclaim", "N002-building"]),
        "error: N002-building.md not found in planning/ or active/"
    );
}

#[test]
fn done_completes_an_active_task_or_any_with_force() {
    let s = Sandbox::new();
    s.seed("active", "N001-built.md", "");
    assert_eq!(s.ok(&["done", "N001-built"]), "");
    assert!(s.at("done", "N001-built.md"));
    assert_eq!(s.subject(), "Done: built");
    assert!(s.pushed());

    s.seed("planned", "N002-skipped.md", "");
    let path = s.tasks().join("widget/active/N002-skipped.md");
    assert_eq!(
        s.refused(&["done", "N002-skipped"]),
        format!("error: not found: {}", path.display())
    );
    assert!(s.at("planned", "N002-skipped.md"));

    for (n, status) in ["active", "planned", "planning", "stale", "todo", "canceled"]
        .into_iter()
        .enumerate()
    {
        let file = format!("N1{n:02}-forced.md");
        s.seed(status, &file, "");
        s.ok(&["done", "--force", &file]);
        assert!(s.at("done", &file), "--force from {status}");
    }
    assert_eq!(
        s.refused(&["done", "--force", "N001-built"]),
        "error: N001-built.md not found in any state"
    );
}

// --- move ---------------------------------------------------------------------

#[test]
fn move_lands_every_legal_target() {
    let s = Sandbox::new();
    let legal = |from: &str| -> &[&str] {
        match from {
            "todo" => &["planned", "done", "canceled"],
            "planning" | "stale" => &["todo", "planned", "done", "canceled"],
            "planned" => &["todo", "done", "canceled"],
            "active" => &["todo", "planned", "stale", "done", "canceled"],
            "done" => &["todo", "planned", "canceled"],
            "canceled" => &["todo", "planned", "done"],
            _ => unreachable!(),
        }
    };
    let mut n = 0;
    for from in STATUSES {
        for to in legal(from) {
            n += 1;
            let file = format!("N{n:03}-task-{n}.md");
            s.seed(from, &file, "# T\nWorker: hlab-5\n");
            let out = s.ok(&["move", file.strip_suffix(".md").unwrap(), to]);
            assert_eq!(out, format!("--- Moved: {file} {from} -> {to} ---\n"));
            assert!(s.at(to, &file) && !s.at(from, &file), "{from} -> {to}");
            assert_eq!(s.subject(), format!("Move: task-{n} {from}->{to}"));
            assert_eq!(s.body(to, &file), "# T\nWorker: hlab-5\n", "{from} -> {to}");
        }
    }
    assert!(s.pushed());
}

#[test]
fn move_refuses_what_the_rule_forbids_and_changes_nothing() {
    let s = Sandbox::new();
    for (n, from) in ["todo", "planning", "planned", "done", "canceled"]
        .into_iter()
        .enumerate()
    {
        let file = format!("N{n:03}-x.md");
        s.seed(from, &file, "");
        assert_eq!(
            s.refused(&["move", &file, "stale"]),
            format!("error: only an active task goes stale ({file} is in {from}/)")
        );
        assert!(s.at(from, &file));
    }
    s.seed("todo", "N010-t.md", "");
    s.seed("active", "N011-a.md", "");
    for (args, err) in [
        (
            ["N010-t", "planning"],
            "error: planning/ is a claim, made by taskania claim",
        ),
        (
            ["N011-a", "active"],
            "error: active/ is a claim, made by taskania claim",
        ),
        (
            ["N099-none", "planning"],
            "error: planning/ is a claim, made by taskania claim",
        ),
        (["N010-t", "todo"], "error: N010-t.md is already in todo/"),
        (
            ["N010-t", "bogus"],
            "error: unknown status: bogus (todo, planned, stale, done, canceled)",
        ),
        (
            ["N099-none", "todo"],
            "error: N099-none.md not found in widget",
        ),
    ] {
        assert_eq!(s.refused(&["move", args[0], args[1]]), err);
    }
    assert!(s.at("todo", "N010-t.md") && s.at("active", "N011-a.md"));

    fs::copy(
        s.tasks().join("widget/todo/N010-t.md"),
        s.tasks().join("widget/done/N010-t.md"),
    )
    .unwrap();
    git(&s.tasks(), &["add", "-A"]);
    git(&s.tasks(), &["commit", "-qm", "dup"]);
    assert_eq!(
        s.refused(&["move", "N010-t", "planned"]),
        "error: N010-t.md is in more than one status: todo done"
    );
}

#[test]
fn project_flag_acts_from_anywhere_and_must_name_a_project() {
    let s = Sandbox::new();
    s.seed_in("proj", "todo", "N001-far.md", "");
    let out = s.run_in(
        &s.root.join("elsewhere"),
        &["move", "--project", "proj", "N001-far", "done"],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(s.tasks().join("proj/done/N001-far.md").is_file());

    s.seed("todo", "N002-here.md", "");
    let err = s.refused(&["move", "N002-here", "done", "--project", "nope"]);
    let dir = s.tasks().join("nope");
    assert_eq!(
        err,
        format!("error: tasks dir not found: {}", dir.display())
    );
    assert!(s.at("todo", "N002-here.md"));
}

// --- reprioritize -------------------------------------------------------------

#[test]
fn reprioritize_rewrites_the_letter_by_filename_or_id() {
    let s = Sandbox::new();
    s.seed("planned", "N007-api.md", "");
    assert_eq!(
        s.ok(&["reprioritize", "N007-api", "h"]),
        "--- N007-api.md -> H007-api.md (planned) ---\n"
    );
    assert!(s.at("planned", "H007-api.md"));
    assert_eq!(s.subject(), "Reprioritize: api N->H");
    assert!(s.pushed());

    s.ok(&["reprioritize", "7", "U"]);
    assert!(s.at("planned", "U007-api.md"));
    s.ok(&["reprioritize", "path/to/U007-api.md", "A"]);
    assert!(s.at("planned", "A007-api.md"));

    let before = s.head();
    assert_eq!(
        s.ok(&["reprioritize", "007", "A"]),
        "A007-api.md already at priority A\n"
    );
    assert_eq!(s.head(), before);
}

#[test]
fn reprioritize_refuses_bad_letters_and_ambiguous_or_closed_tasks() {
    let s = Sandbox::new();
    s.seed("todo", "N001-a.md", "");
    s.seed("active", "H001-b.md", "");
    s.seed("done", "N002-c.md", "");
    s.seed("todo", "N003-d.md", "");
    s.seed("todo", "H003-d.md", "");
    let letter = "error: priority must be a single letter A-Z (see ~/repos/tasks/CLAUDE.md)";
    for bad in ["HH", "3", ""] {
        assert_eq!(s.refused(&["reprioritize", "N001-a", bad]), letter);
    }
    assert_eq!(
        s.refused(&["reprioritize", "1", "U"]),
        "error: 1 matches 2 tasks: N001-a.md H001-b.md"
    );
    assert_eq!(
        s.refused(&["reprioritize", "N002-c", "U"]),
        "error: N002-c not found in todo/, planning/, planned/, active/, or stale/"
    );
    assert_eq!(
        s.refused(&["reprioritize", "N003-d", "H"]),
        "error: H003-d.md already exists in todo/ (duplicate ID?)"
    );
    s.seed("todo", "notes.md", "");
    assert_eq!(
        s.refused(&["reprioritize", "notes", "H"]),
        "error: not a <letter><NNN>-slug.md task file: notes.md"
    );
}

// --- create, next-id ----------------------------------------------------------

#[test]
fn create_adds_an_empty_task_named_by_the_title() {
    let s = Sandbox::new();
    assert_eq!(
        s.ok(&["create", "Fix the thing"]),
        "N001-fix-the-thing.md\n"
    );
    assert_eq!(s.body("todo", "N001-fix-the-thing.md"), "");
    assert_eq!(s.subject(), "Add: fix-the-thing");
    assert!(s.pushed());
    s.ok(&["create", "-p", "H", "Urgent thing"]);
    assert!(s.at("todo", "H002-urgent-thing.md"));
    s.ok(&["create", "-p", "u", "Minor thing"]);
    assert!(s.at("todo", "U003-minor-thing.md"));
    s.ok(&["create", "Fix: the (API)!!"]);
    assert!(s.at("todo", "N004-fix-the-api.md"));
    s.ok(&["create", "these", "are", "loose", "words"]);
    assert!(s.at("todo", "N005-these-are-loose-words.md"));
}

#[test]
fn a_refused_create_writes_nothing_and_takes_no_id() {
    let s = Sandbox::new();
    for (args, err) in [
        (&["create", ""][..], "error: title required"),
        (&["create", "   "], "error: title required"),
        (&["create"], "error: title required"),
        (
            &["create", "-p", "HH", "two-letter prio"],
            "error: priority must be a single letter A-Z",
        ),
        (
            &["create", "-p", "3", "digit prio"],
            "error: priority must be a single letter A-Z",
        ),
        (
            &["create", "!!!"],
            "error: title has no alphanumeric characters to slug",
        ),
    ] {
        assert_eq!(s.refused(args), err);
    }
    assert_eq!(
        fs::read_dir(s.tasks().join("widget/todo")).unwrap().count(),
        0
    );
    s.ok(&["create", "After the failures"]);
    assert!(s.at("todo", "N001-after-the-failures.md"));
}

#[test]
fn create_makes_the_cwds_project_but_not_a_named_one() {
    let s = Sandbox::new();
    let err = s.refused(&["create", "--project", "nope", "typo"]);
    assert!(err.starts_with("error: tasks dir not found"), "{err}");
    assert!(!s.tasks().join("nope").exists());

    let fresh = s.checkout("fresh", None);
    let out = s.run_in(&fresh, &["create", "first"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(s.tasks().join("fresh/todo/N001-first.md").is_file());
    assert!(s.tasks().join("fresh/stale").is_dir());

    let out = s.run_in(
        &s.root.join("elsewhere"),
        &["create", "--project", "proj", "from afar"],
    );
    assert_eq!(text(&out.stdout), "N001-from-afar.md\n");
    assert!(s.tasks().join("proj/todo/N001-from-afar.md").is_file());
}

#[test]
fn next_id_is_one_past_the_highest_in_any_status() {
    let s = Sandbox::new();
    assert_eq!(s.ok(&["next-id"]), "001\n");
    s.seed("done", "H041-old.md", "");
    s.seed("todo", "N007-a.md", "");
    s.seed("todo", "notes.md", "");
    assert_eq!(s.ok(&["next-id"]), "042\n");
    assert_eq!(s.ok(&["next-id", "--project", "proj"]), "001\n");
}

// --- commit -------------------------------------------------------------------

#[test]
fn commit_commits_everything_under_the_root_and_pushes() {
    let s = Sandbox::new();
    s.seed("todo", "N001-x.md", "");
    fs::write(s.tasks().join("widget/todo/N001-x.md"), "edited\n").unwrap();
    fs::write(s.tasks().join("proj/todo/N002-y.md"), "new\n").unwrap();
    let out = s.run_in(&s.root.join("elsewhere"), &["commit", "Edit: x"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "");
    assert_eq!(s.subject(), "Edit: x");
    assert!(s.pushed());
    assert_eq!(git(&s.tasks(), &["status", "--porcelain"]), "");

    let err = s.refused(&["commit", "nothing"]);
    assert!(err.contains("nothing to commit"), "{err}");
}

#[test]
fn a_change_pulls_what_another_clone_pushed() {
    let s = Sandbox::new();
    let other = s.root.join("other");
    git(
        &s.root,
        &["clone", "-q", "-b", "main", "origin.git", "other"],
    );
    git(&other, &["config", "user.email", "o@o"]);
    git(&other, &["config", "user.name", "O"]);
    // Git keeps no empty directory: the clone has no status dirs.
    fs::create_dir_all(other.join("widget/todo")).unwrap();
    fs::write(other.join("widget/todo/N001-remote.md"), "").unwrap();
    git(&other, &["add", "-A"]);
    git(&other, &["commit", "-qm", "remote"]);
    git(&other, &["push", "-q"]);

    s.ok(&["move", "N001-remote", "done"]);
    assert!(s.at("done", "N001-remote.md"));
    assert!(s.pushed());
}
