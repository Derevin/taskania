# taskania

Terminal dashboard for a markdown task queue. Shows one project's tasks live, optionally a second project's beside them, and lets you peek into a task, edit it, create one, move it between statuses and change its priority.

Taskania owns no task logic. It renders the output of `task-list.sh` and calls a task script for every change (`task-move.sh`, `task-create.sh`, `task-reprioritize.sh`, `task-commit.sh`), so those scripts must be on your `PATH`. They live in the `scripts/` directory of [Derevin/dotfiles-public](https://github.com/Derevin/dotfiles-public), together with the conventions of the task repo they manage.

## Status

Published **as is**. No support, no roadmap, no promises. Bug reports, PRs, and feature requests may go unanswered or be closed without comment. Built and maintained for one person's setup; that may diverge from yours at any time.

Forks welcome.

## Build

```
cargo build --release
```

The binary is `target/release/taskania`. Drop it on your `PATH`.

## Usage

```
taskania [--project NAME] [--with NAME] [--recent DAYS] [--dates] [--labs] [--stale] [--poll SECS] [--once]
```

The project defaults to what `find-project.sh` names for the current directory; tasks live under `$TASKS_ROOT` (default `~/repos/tasks`). `--with` adds a second project's list, right-aligned. Below the open queue come the tasks done or canceled in the last `--recent` days (default 7). The view refreshes when a task file changes, checked every `--poll` seconds (default 2). `--once` prints a single frame and exits.

## Keys

| Key | Does |
|-----|------|
| `←` `→` | widen or narrow the done window by a day |
| `d` `l` `s` | toggle row dates, lab tags, the stale section |
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
