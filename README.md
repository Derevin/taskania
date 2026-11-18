# taskania

Task tool for a markdown task queue kept in git. Its verbs list and change tasks; its dashboard shows one project's tasks live, optionally a second project's beside them, and lets you peek into a task, edit it, create one, move it between statuses and change its priority.

Tasks are files under `$TASKS_ROOT` (default `~/repos/tasks`), a git repo with a remote: `<project>/<status>/<Letter><NNN>-<slug>.md`, where the status is one of `todo planning planned active stale done canceled`, the letter a priority and `NNN` the task's id. Every change pulls the repo first, then commits and pushes it. Taskania needs `git` and nothing else.

## Status

Published **as is**. No support, no roadmap, no promises. Bug reports, PRs, and feature requests may go unanswered or be closed without comment. Built and maintained for one person's setup; that may diverge from yours at any time.

Forks welcome.

## Build

```
cargo build --release
```

The binary is `target/release/taskania`. Drop it on your `PATH`.

## Verbs

```
taskania list [--status STATUS...] [--all] [--verbose] [--no-header] [--ids] [--no-stale] [--dates] [--recent DAYS]
taskania claim FILE               todo -> planning; planned or stale -> active
taskania planned FILE             planning -> planned
taskania unclaim FILE             planning -> todo; active -> planned
taskania done [--force] FILE      active -> done; --force from any status
taskania move FILE STATUS         by hand: into todo, planned, done or canceled; into stale only from active
taskania reprioritize FILE|ID LETTER
taskania create [-p LETTER] TITLE...
taskania next-id
taskania commit MESSAGE           commit and push everything changed in the tasks repo
taskania project                  print the current directory's project
```

`FILE` is a task's filename, with or without `.md`. Every verb but `commit` and `project` acts on the current directory's project, or on the one named by `--project NAME`. `taskania VERB --help` explains each.

The current directory's project is named by a `.find-project.conf` in it or its nearest ancestor (a `project = NAME` line, or a bare name). Otherwise it is the git checkout's directory name with any lab suffix (`-hlab-…`, `-dlab-…`, `-clab-…`, `-hwt<N>…`, `-dwt…`, `-cwt…`) stripped, looked up in `$TASKANIA_PROJECTS_CONF` (default `$TASKS_ROOT/projects.conf`, lines `name = project`), else lowercased.

## Dashboard

```
taskania --watch [--with NAME]
```

Shows the current directory's project; `--with` adds a second project's list, right-aligned. Below the open queue come the tasks done or canceled in the last 7 days. The view refreshes when a task file changes, checked every 2 seconds.

| Key | Does |
|-----|------|
| `←` `→` | widen or narrow the done window by a day |
| `d` `s` | toggle row dates, the stale section |
| `↑` `↓` | move the row cursor; the first press shows it |
| `Tab` | hop to the other column |
| `Esc` | hide the cursor (it also hides after a minute without a key) |
| `Enter` | peek: the task file, scrollable |
| `e` | edit the task file in `$VISUAL` or `$EDITOR`, then commit it |
| `m` | move: `t` todo, `p` planned, `s` stale, `d` done, `c` canceled |
| `p` | set the priority letter, A to Z |
| `n` | create a task: a priority letter and a title |
| `q` | quit |

Changes run in the background, one at a time, in order; the bottom bar says `syncing…` until they land.

## License

MIT — see [LICENSE](LICENSE).
