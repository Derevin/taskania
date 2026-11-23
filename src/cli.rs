//! The verbs: each makes one change or read and returns what it prints.

use crate::Ctx;
use crate::args::{ListArgs, Verb};
use crate::listing::{self, Spec, Status};
use crate::tasks::Tasks;

pub fn run(ctx: &Ctx, project: Option<String>, verb: Verb) -> Result<String, String> {
    let tasks = ctx.tasks();
    let explicit = project.is_some();
    let project = || project.clone().map_or_else(|| ctx.cwd_project(), Ok);
    let change = match verb {
        Verb::List(a) => return list(&tasks, &project()?, &a),
        Verb::NextId => return tasks.next_id(&project()?),
        Verb::Project => return Ok(format!("{}\n", ctx.cwd_project()?)),
        Verb::Claim(file) => tasks.claim(&project()?, &file),
        Verb::Planned(file) => tasks.planned(&project()?, &file),
        Verb::Unclaim(file) => tasks.unclaim(&project()?, &file),
        Verb::Done { file, force } => tasks.done(&project()?, &file, force),
        Verb::Move { file, to } => tasks.move_task(&project()?, &file, &to),
        Verb::Reprioritize { task, letter } => tasks.reprioritize(&project()?, &task, &letter),
        Verb::Rename { task, title } => tasks.rename(&project()?, &task, &title),
        // A project named by hand must exist: a typo must not start a new one.
        Verb::Create { letter, title } => tasks.create(&project()?, !explicit, &letter, &title),
        Verb::Commit(message) => tasks.commit(&message),
    };
    tasks.publish_one(change)
}

fn list(tasks: &Tasks, project: &str, a: &ListArgs) -> Result<String, String> {
    use Status::*;
    let dir = tasks.project_dir(project)?;
    let mut statuses = if !a.statuses.is_empty() {
        a.statuses.clone()
    } else if a.all {
        vec![Canceled, Done, Active, Planned, Planning, Stale, Todo]
    } else {
        Spec::open(true)
    };
    if a.no_stale {
        statuses.retain(|&s| s != Stale);
    }
    let spec = Spec {
        statuses,
        recent: a.recent,
        dates: a.dates,
        verbose: a.verbose,
        ids: a.ids,
    };
    let mut out = String::new();
    if !a.no_header && !a.ids {
        out.push_str(&format!("Tasks: {project}\n\n"));
    }
    for row in listing::rows(&dir, &spec)? {
        out.push_str(&row.text);
        out.push('\n');
    }
    Ok(out)
}
