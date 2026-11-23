//! Drawing. The list area: each column clipped to its width, a list taller than
//! the body cut to `rows-1` lines and `  +N (+M) more` (open tasks cut, then
//! done and canceled), a second column as wide as its longest line (at most
//! half the pane) flush with the right edge. On top of it: the cursor, the bar,
//! the move, priority and view prompts, the task form and the peek overlay.

use ratatui::buffer::{Buffer, CellWidth};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Widget};

use crate::app::{App, Form, MOVES, Mode};
use crate::listing::{Row, Task};

const GUTTER: usize = 2;
const IDLE_HINTS: [&str; 2] = ["(↑↓←→ select, n new, v view, q)", "(↑↓←→ n v q)"];
const CURSOR_HINTS: [&str; 2] = [
    "(⏎ peek, e edit, m move, p prio, r rename, n new, v view, esc)",
    "(⏎ e m p r n v esc)",
];

/// Draw the pane. Returns where the text cursor belongs, if it shows at all.
pub fn render(area: Rect, buf: &mut Buffer, app: &mut App) -> Option<Position> {
    if matches!(app.mode, Mode::Peek(_)) {
        render_peek(area, buf, app);
        return None;
    }
    let form = matches!(app.mode, Mode::Form(_));
    let foot_height = if form { 3 } else { 1 }.min(area.height);
    let body = Rect {
        height: area.height - foot_height,
        ..area
    };
    let foot = Rect {
        y: area.y + body.height,
        height: foot_height,
        ..area
    };
    render_body(body, buf, app);
    if form {
        return render_form(foot, buf, app);
    }
    if foot.height > 0 {
        buf.set_line(foot.x, foot.y, &bar(app, foot.width as usize), foot.width);
    }
    None
}

/// Column widths for a pane `cols` wide whose second list's longest line is
/// `longest`: the second takes what it needs up to half the pane less the
/// gutter, the primary the rest.
pub fn split(cols: usize, longest: usize) -> (usize, usize) {
    let right = longest.min(cols.saturating_sub(GUTTER) / 2).max(1);
    let left = cols.saturating_sub(right + GUTTER).max(1);
    (left, right)
}

/// Which rows of a column fit `height` lines: the first shown, how many, and
/// how many rows the `+N more` line stands for (0: none). Without a cursor the
/// column shows its top. With one it scrolls, as little as keeps the cursor
/// in view; `offset` carries the scroll between frames.
pub fn window(
    total: usize,
    height: usize,
    cursor: Option<usize>,
    offset: &mut usize,
) -> (usize, usize, usize) {
    if height == 0 {
        return (0, 0, 0);
    }
    let Some(c) = cursor else {
        *offset = 0;
        return if total > height {
            (0, height - 1, total - height + 1)
        } else {
            (0, total, 0)
        };
    };
    if total <= height {
        *offset = 0;
        return (0, total, 0);
    }
    if height == 1 {
        *offset = c;
        return (c, 1, 0);
    }
    let last_start = total - height;
    // Rows shown from `s`: all the rest at the bottom, else one fewer than the
    // height, the last line being `+N more`.
    let shown = |s: usize| {
        if s >= last_start {
            total - s
        } else {
            height - 1
        }
    };
    let mut s = (*offset).min(last_start).min(c);
    while c >= s + shown(s) {
        s += 1;
    }
    *offset = s;
    (s, shown(s), total - s - shown(s))
}

fn render_body(area: Rect, buf: &mut Buffer, app: &mut App) {
    let cols = area.width as usize;
    let spans: Vec<(u16, usize)> = if app.columns.len() > 1 {
        let longest = app.columns[1]
            .rows
            .iter()
            .map(|r| text_width(&r.text))
            .max()
            .unwrap_or(0);
        let (left, right) = split(cols, longest);
        vec![(area.x, left), (area.x + (left + GUTTER) as u16, right)]
    } else {
        vec![(area.x, cols)]
    };
    let cursor = app.cursor.as_ref().map(|c| (c.col, c.row));
    for (col, (x, width)) in spans.into_iter().enumerate() {
        let here = cursor.filter(|&(c, _)| c == col).map(|(_, r)| r);
        let (start, shown, more) = {
            let column = &mut app.columns[col];
            window(
                column.rows.len(),
                area.height as usize,
                here,
                &mut column.offset,
            )
        };
        let column = &app.columns[col];
        for (i, row) in column.rows[start..start + shown].iter().enumerate() {
            let y = area.y + i as u16;
            let pending = row
                .task
                .as_ref()
                .is_some_and(|t| app.is_pending(col, &t.key()));
            let style = if here == Some(start + i) {
                let style = Style::new().add_modifier(Modifier::REVERSED);
                buf.set_style(Rect::new(x, y, width as u16, 1).intersection(area), style);
                style
            } else if pending {
                Style::new().add_modifier(Modifier::DIM)
            } else {
                Style::new()
            };
            buf.set_stringn(x, y, &row.text, width, style);
        }
        if more > 0 {
            buf.set_stringn(
                x,
                area.y + shown as u16,
                more_line(&column.rows[start + shown..]),
                width,
                Style::new(),
            );
        }
    }
}

/// The count of the tasks among the `cut` rows: `  +N more`, or
/// `  +N (+M) more` when M of them are done or canceled.
fn more_line(cut: &[Row]) -> String {
    let tasks = cut.iter().filter_map(|r| r.task.as_ref());
    let closed = tasks.clone().filter(|t| t.status.is_closed()).count();
    let open = tasks.count() - closed;
    if closed == 0 {
        format!("  +{open} more")
    } else {
        format!("  +{open} (+{closed}) more")
    }
}

fn bar(app: &App, width: usize) -> Line<'static> {
    match &app.mode {
        Mode::Move(task) => move_menu(task),
        Mode::Prio(task) => {
            let mut spans = vec![Span::raw(format!("priority for {}: ", task.name()))];
            spans.push(Span::styled(
                "a-z",
                Style::new().add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::raw("  esc"));
            Line::from(spans)
        }
        Mode::View { recent } => view_menu(app, *recent, width),
        _ => status_bar(app, width),
    }
}

/// The view menu: the pending done window, then the toggles and their state.
/// A narrow pane loses `esc`, then `⏎`; past that the bar clips its right end.
fn view_menu(app: &App, recent: u32, width: usize) -> Line<'static> {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let state = |on: bool| if on { "on" } else { "off" };
    let menu = |enter: bool, esc: bool| {
        let mut spans = vec![
            Span::raw(format!("view: {} ", done_between(app, recent))),
            Span::styled("←→", bold),
        ];
        if enter {
            spans.extend([Span::raw(" "), Span::styled("⏎", bold)]);
        }
        spans.extend([
            Span::raw("  "),
            Span::styled("d", bold),
            Span::raw(format!(" dates:{}", state(app.opts.dates))),
            Span::raw("  "),
            Span::styled("s", bold),
            Span::raw(format!(" stale:{}", state(app.opts.stale))),
        ]);
        if esc {
            spans.push(Span::raw("  esc"));
        }
        Line::from(spans)
    };
    [menu(true, true), menu(true, false)]
        .into_iter()
        .find(|line| line.width() <= width)
        .unwrap_or_else(|| menu(false, false))
}

/// The done window as the bar words it: `recent` days back to today.
fn done_between(app: &App, recent: u32) -> String {
    format!(
        "done between {} and {}",
        app.today.minus_days(recent).marker(),
        app.today.dd_mm()
    )
}

/// The move menu: every target, the ones the move would refuse dimmed.
fn move_menu(task: &Task) -> Line<'static> {
    let mut spans = vec![Span::raw(format!("move {} to", task.name()))];
    for (key, to) in MOVES {
        let style = if task.status.can_move_to(to) {
            Style::new()
        } else {
            Style::new().add_modifier(Modifier::DIM)
        };
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            key.to_string(),
            style.add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {}", to.dir()), style));
    }
    spans.push(Span::raw("  esc"));
    Line::from(spans)
}

/// The primary's name and the done window on the left, the second project's
/// name at the right edge when it fits, and between them a running job, the
/// last result, or key hints. Hints yield to the right name; a result
/// outranks it.
fn status_bar(app: &App, width: usize) -> Line<'static> {
    let left = format!(
        "{}, {}",
        app.columns[0].project,
        done_between(app, app.opts.recent)
    );
    let right = app.columns.get(1).map(|c| c.project.clone());
    let dim = Style::new().add_modifier(Modifier::DIM);
    // Queued with nothing reported yet, there is no count to show.
    let syncing = match app.progress {
        Some(p) => format!("syncing {}/{}…", p.done, p.total),
        None => "syncing…".to_string(),
    };
    let (mids, style, outranks): (Vec<&str>, Style, bool) = if app.in_flight > 0 {
        (vec![syncing.as_str()], Style::new(), true)
    } else if let Some(m) = &app.message {
        let style = if m.error {
            Style::new().fg(Color::Red)
        } else {
            Style::new()
        };
        (vec![m.text.as_str()], style, true)
    } else if app.cursor.is_some() {
        (CURSOR_HINTS.to_vec(), dim, false)
    } else {
        (IDLE_HINTS.to_vec(), dim, false)
    };

    let lw = text_width(&left);
    let rw = right.as_deref().map(text_width);
    let room = |with_right: bool| {
        width.saturating_sub(
            lw + if with_right {
                rw.map_or(0, |w| w + 1)
            } else {
                0
            },
        )
    };
    let mut with_right = rw.is_some_and(|w| lw + 1 + w <= width);
    let mut mid = mids
        .iter()
        .find(|m| text_width(m) < room(with_right))
        .map(|m| m.to_string());
    if mid.is_none() && outranks {
        with_right = false;
        mid = Some(clip(mids[0], room(false).saturating_sub(1))).filter(|m| !m.is_empty());
    }

    let mut used = lw;
    let mut spans = vec![Span::raw(left)];
    if let Some(m) = mid {
        used += 1 + text_width(&m);
        spans.push(Span::raw(" "));
        spans.push(Span::styled(m, style));
    }
    if let (true, Some(r), Some(rw)) = (with_right, right, rw) {
        spans.push(Span::raw(" ".repeat(width.saturating_sub(used + rw))));
        spans.push(Span::raw(r));
    }
    Line::from(spans)
}

fn render_form(area: Rect, buf: &mut Buffer, app: &App) -> Option<Position> {
    let Mode::Form(f) = &app.mode else {
        return None;
    };
    let focus = Style::new().add_modifier(Modifier::BOLD).fg(Color::Cyan);
    let border = |on: bool| if on { focus } else { Style::new() };
    let create = f.target.is_none();
    let prio = Rect {
        width: if create { 6.min(area.width) } else { 0 },
        ..area
    };
    let title = Rect {
        x: area.x + prio.width,
        width: area.width - prio.width,
        ..area
    };

    let (label, hints) = if let Some(task) = &f.target {
        (format!("rename {}", task.name()), "⏎ rename, esc")
    } else {
        Block::bordered()
            .title("prio")
            .border_style(border(f.in_prio))
            .render(prio, buf);
        let hints = if app.columns.len() > 1 {
            "⏎ create, tab project, esc"
        } else {
            "⏎ create, esc"
        };
        (format!("title ({})", app.columns[f.col].project), hints)
    };
    Block::bordered()
        .title(label)
        .title_bottom(Line::from(format!(" {hints} ")).right_aligned())
        .border_style(border(!f.in_prio))
        .render(title, buf);
    if area.height < 3 || title.width < 3 {
        return None;
    }
    if create {
        buf.set_stringn(
            prio.x + 1,
            prio.y + 1,
            f.letter.to_string(),
            1,
            Style::new(),
        );
    }
    let inner = Position::new(title.x + 1, title.y + 1);
    let (start, cursor_x) = title_window(f, (title.width - 2) as usize);
    let shown: String = f.title[start..].iter().collect();
    buf.set_stringn(
        inner.x,
        inner.y,
        shown,
        (title.width - 2) as usize,
        Style::new(),
    );
    Some(if f.in_prio {
        Position::new(prio.x + 1, prio.y + 1)
    } else {
        Position::new(inner.x + cursor_x as u16, inner.y)
    })
}

/// The first title character shown, scrolled so the text cursor stays in a box
/// `width` cells wide, and the cursor's cell within it.
fn title_window(f: &Form, width: usize) -> (usize, usize) {
    let width_of = |s: &[char]| text_width(&s.iter().collect::<String>());
    let mut start = 0;
    while start < f.pos && width_of(&f.title[start..f.pos]) >= width.max(1) {
        start += 1;
    }
    (start, width_of(&f.title[start..f.pos]))
}

/// The task file, wrapped, over the whole pane: its path on top, keys below.
fn render_peek(area: Rect, buf: &mut Buffer, app: &mut App) {
    let (w, h) = (area.width as usize, area.height as usize);
    if w == 0 || h == 0 {
        return;
    }
    let content_h = h.saturating_sub(2);
    let root = app.tasks_root.clone();
    let Mode::Peek(p) = &mut app.mode else { return };
    let title = p
        .path
        .strip_prefix(&root)
        .unwrap_or(&p.path)
        .display()
        .to_string();
    let reversed = Style::new().add_modifier(Modifier::REVERSED);
    buf.set_style(Rect { height: 1, ..area }, reversed);
    buf.set_stringn(area.x, area.y, title, w, reversed);

    let dim = Style::new().add_modifier(Modifier::DIM);
    let (lines, style) = match &p.text {
        Some(t) if t.is_empty() => (vec!["(empty file)".to_string()], dim),
        Some(t) => (wrap(t, w), Style::new()),
        None => (vec!["(the file is gone)".to_string()], dim),
    };
    p.scroll = p.scroll.min(lines.len().saturating_sub(content_h));
    for (i, line) in lines.iter().skip(p.scroll).take(content_h).enumerate() {
        buf.set_stringn(area.x, area.y + 1 + i as u16, line, w, style);
    }
    if h >= 2 {
        let last = lines.len().min(p.scroll + content_h);
        let pos = format!("{}-{}/{}", (p.scroll + 1).min(last), last, lines.len());
        let keys = "↑↓ PgUp PgDn scroll, esc close";
        let y = area.y + area.height - 1;
        buf.set_stringn(area.x, y, keys, w, dim);
        let pw = text_width(&pos);
        if text_width(keys) + 1 + pw <= w {
            buf.set_stringn(area.x + (w - pw) as u16, y, pos, pw, dim);
        }
    }
    app.peek_rows = content_h;
}

/// Lines of `text` wrapped to `width` cells: at a space where one falls in the
/// line, mid-word where none does. Tabs become spaces.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for raw in text.strip_suffix('\n').unwrap_or(text).split('\n') {
        let mut line = String::new();
        let mut line_w = 0;
        // Byte index and width just past the line's last space.
        let mut space: Option<(usize, usize)> = None;
        for ch in raw.chars() {
            let (ch, n) = match ch {
                '\t' => (' ', 4),
                c if c.is_control() => continue,
                c => (c, 1),
            };
            let cw = char_width(ch);
            for _ in 0..n {
                // A space that would overflow ends the line instead.
                if ch == ' ' && line_w + cw > width {
                    out.push(std::mem::take(&mut line).trim_end().to_string());
                    line_w = 0;
                    space = None;
                    continue;
                }
                while line_w + cw > width && !line.is_empty() {
                    match space.take() {
                        Some((at, w)) if at < line.len() => {
                            let rest = line.split_off(at);
                            out.push(line.trim_end().to_string());
                            line = rest;
                            line_w -= w;
                        }
                        _ => {
                            out.push(std::mem::take(&mut line).trim_end().to_string());
                            line_w = 0;
                        }
                    }
                }
                line.push(ch);
                line_w += cw;
                if ch == ' ' {
                    space = Some((line.len(), line_w));
                }
            }
        }
        out.push(line);
    }
    out
}

fn char_width(c: char) -> usize {
    let mut b = [0; 4];
    c.encode_utf8(&mut b).cell_width() as usize
}

pub fn text_width(s: &str) -> usize {
    s.chars().filter(|c| !c.is_control()).map(char_width).sum()
}

/// `s` cut to `width` cells, an ellipsis marking the cut.
fn clip(s: &str, width: usize) -> String {
    if text_width(s) <= width {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars().filter(|c| !c.is_control()) {
        let cw = char_width(c);
        if w + cw + 1 > width {
            break;
        }
        out.push(c);
        w += cw;
    }
    if width > 0 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::Progress;
    use crate::testutil::{PROJ, app, draw, key, lines, put};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyCode;
    use std::fs;

    fn modifier(t: &Terminal<TestBackend>, x: u16, y: u16) -> Modifier {
        t.backend().buffer()[(x, y)].modifier
    }

    #[test]
    fn the_idle_frame_is_the_listing_and_the_bar() {
        let (mut a, _tmp) = app(false);
        let t = draw(&mut a, 60, 12);
        let got = lines(&t);
        let want: Vec<&str> = PROJ.lines().collect();
        assert_eq!(&got[..9], &want[..]);
        assert!(got[9..11].iter().all(String::is_empty));
        assert!(
            got[11].starts_with("proj, done between Mo 09-11 and 16-11 ("),
            "{}",
            got[11]
        );
    }

    #[test]
    fn a_second_project_sits_flush_right_and_names_itself_in_the_bar() {
        let (mut a, _tmp) = app(true);
        let t = draw(&mut a, 80, 12);
        let got = lines(&t);
        assert_eq!(
            got[1],
            format!("{:<64}{}", "  H010 busy", "  N007 left-pad")
        );
        assert_eq!(
            got[2].len(),
            80,
            "the widest second-column line ends in the last cell"
        );
        assert!(got[2].ends_with("  N008 right-pad"));
        assert_eq!(
            got[3], "  N001 alpha",
            "no padding where the second column has run out"
        );
        assert!(
            got[11].ends_with(" widget") && text_width(&got[11]) == 80,
            "{}",
            got[11]
        );
    }

    #[test]
    fn every_line_is_clipped_and_the_second_column_capped_at_half() {
        let (mut a, tmp) = app(true);
        let long = |c: &str, n| format!("{}.md", c.repeat(n));
        put(
            tmp.path(),
            "proj",
            "todo",
            &format!("N004-{}", long("y", 90)),
            "",
        );
        put(
            tmp.path(),
            "widget",
            "todo",
            &format!("N009-{}", long("x", 70)),
            "",
        );
        a.relist();
        let t = draw(&mut a, 80, 12);
        let got = lines(&t);
        assert!(got.iter().all(|l| text_width(l) <= 80));
        assert_eq!(split(80, 76), (39, 39));
        assert_eq!(
            got[1].find("  N007"),
            Some(41),
            "right column starts at 80 - 39"
        );
        assert_eq!(got[3].len(), 80);
    }

    #[test]
    fn the_split_follows_the_bash_watch() {
        assert_eq!(split(80, 17), (61, 17));
        assert_eq!(split(80, 100), (39, 39));
        assert_eq!(split(80, 0), (77, 1));
        assert_eq!(split(3, 5), (1, 1));
    }

    #[test]
    fn a_tall_list_ends_in_a_count_of_what_is_cut() {
        let (mut a, _tmp) = app(false);
        let t = draw(&mut a, 40, 6);
        let got = lines(&t);
        assert_eq!(&got[..4], &PROJ.lines().take(4).collect::<Vec<_>>()[..]);
        assert_eq!(got[4], "  +2 (+1) more", "open tasks, then done and canceled");
    }

    #[test]
    fn the_cut_count_drops_the_closed_part_when_none_is_cut() {
        let (mut a, _tmp) = app(true);
        let got = lines(&draw(&mut a, 80, 3));
        assert!(got[1].ends_with("  +2 more"), "{got:?}");
    }

    #[test]
    fn the_cursor_row_is_reversed_across_its_column() {
        let (mut a, _tmp) = app(true);
        key(&mut a, KeyCode::Down); // H010, row 1
        let t = draw(&mut a, 80, 12);
        assert!(modifier(&t, 0, 1).contains(Modifier::REVERSED));
        assert!(
            modifier(&t, 61, 1).contains(Modifier::REVERSED),
            "to the column's edge"
        );
        assert!(
            !modifier(&t, 64, 1).contains(Modifier::REVERSED),
            "not into the second column"
        );
        assert!(!modifier(&t, 0, 3).contains(Modifier::REVERSED));
        assert!(
            lines(&t)[11].contains("(⏎ e m p r n v esc)"),
            "the bar shows the row keys"
        );
    }

    #[test]
    fn only_the_cursors_column_scrolls() {
        let (mut a, _tmp) = app(true);
        for _ in 0..5 {
            key(&mut a, KeyCode::Down); // N050, row 8 of 9
        }
        let t = draw(&mut a, 80, 6);
        let got = lines(&t);
        assert!(got[4].starts_with("  N050 shipped"), "{got:?}");
        assert!(modifier(&t, 0, 4).contains(Modifier::REVERSED));
        assert!(
            got[0].starts_with("  N002 beta"),
            "scrolled as little as shows the cursor"
        );

        key(&mut a, KeyCode::Right);
        let got = lines(&draw(&mut a, 80, 6));
        assert!(
            got[0].starts_with("ACTIVE (1)"),
            "back to its top once the cursor leaves"
        );
        assert!(got[4].starts_with("  +2 (+1) more"));
    }

    #[test]
    fn the_window_scrolls_as_little_as_it_must() {
        let mut off = 0;
        assert_eq!(window(9, 5, None, &mut off), (0, 4, 5));
        assert_eq!(window(3, 5, None, &mut off), (0, 3, 0));
        assert_eq!(window(9, 5, Some(3), &mut off), (0, 4, 5));
        assert_eq!(window(9, 5, Some(4), &mut off), (1, 4, 4));
        assert_eq!(window(9, 5, Some(8), &mut off), (4, 5, 0));
        assert_eq!(
            window(9, 5, Some(6), &mut off),
            (4, 5, 0),
            "already in view: no scroll"
        );
        assert_eq!(window(9, 5, Some(2), &mut off), (2, 4, 3));
        assert_eq!(window(9, 1, Some(6), &mut off), (6, 1, 0));
        assert_eq!(window(9, 0, Some(6), &mut off), (0, 0, 0));
    }

    #[test]
    fn the_bar_drops_hints_before_the_second_name() {
        let (mut a, _tmp) = app(true);
        let t = draw(&mut a, 64, 4);
        let bar = &lines(&t)[3];
        assert!(bar.ends_with(" widget"), "{bar}");
        assert!(bar.contains("(↑↓←→ n v q)"), "the short hints: {bar}");
        let t = draw(&mut a, 45, 4);
        assert_eq!(
            lines(&t)[3],
            format!("{:<39}widget", "proj, done between Mo 09-11 and 16-11")
        );
        let t = draw(&mut a, 30, 4);
        assert_eq!(lines(&t)[3], "proj, done between Mo 09-11 an");
    }

    #[test]
    fn a_wide_bar_spells_the_hints_out() {
        let (mut a, _tmp) = app(false);
        let left = "proj, done between Mo 09-11 and 16-11";
        assert_eq!(
            lines(&draw(&mut a, 100, 4))[3],
            format!("{left} (↑↓←→ select, n new, v view, q)")
        );
        key(&mut a, KeyCode::Down);
        assert_eq!(
            lines(&draw(&mut a, 100, 4))[3],
            format!("{left} (⏎ peek, e edit, m move, p prio, r rename, n new, v view, esc)")
        );
    }

    #[test]
    fn the_view_menu_takes_the_bar_with_the_pending_date() {
        let (mut a, _tmp) = app(true);
        key(&mut a, KeyCode::Char('v'));
        let t = draw(&mut a, 80, 4);
        let menu = "view: done between Mo 09-11 and 16-11 ←→ ⏎  d dates:off  s stale:off  esc";
        assert_eq!(lines(&t)[3], menu);
        let bold = |k: &str| {
            let x = menu[..menu.find(k).unwrap()].chars().count() as u16;
            modifier(&t, x, 3).contains(Modifier::BOLD)
        };
        assert!(bold("←→") && bold("⏎") && bold("d dates") && bold("s stale"));
        assert!(!bold("view") && !bold("dates:") && !bold("esc"));

        key(&mut a, KeyCode::Left);
        assert!(
            lines(&draw(&mut a, 80, 4))[3].starts_with("view: done between Su 08-11 and 16-11 ←→"),
            "the pending date, not the applied one"
        );
        a.opts.dates = true;
        a.opts.stale = true;
        assert!(lines(&draw(&mut a, 80, 4))[3].ends_with("  d dates:on  s stale:on  esc"));
    }

    #[test]
    fn a_narrow_view_menu_drops_esc_then_enter_then_clips() {
        let (mut a, _tmp) = app(false);
        key(&mut a, KeyCode::Char('v'));
        let date = "view: done between Mo 09-11 and 16-11";
        let bar = |a: &mut App, w| lines(&draw(a, w, 4))[3].clone();
        assert_eq!(
            bar(&mut a, 73),
            format!("{date} ←→ ⏎  d dates:off  s stale:off  esc")
        );
        assert_eq!(
            bar(&mut a, 72),
            format!("{date} ←→ ⏎  d dates:off  s stale:off")
        );
        assert_eq!(
            bar(&mut a, 67),
            format!("{date} ←→  d dates:off  s stale:off")
        );
        assert_eq!(bar(&mut a, 50), format!("{date} ←→  d dates:"));
        assert_eq!(bar(&mut a, 30), "view: done between Mo 09-11 an");
    }

    #[test]
    fn a_result_outranks_the_second_name() {
        let (mut a, _tmp) = app(true);
        a.in_flight = 1;
        a.progress = Some(Progress { done: 2, total: 5 });
        let t = draw(&mut a, 80, 4);
        assert!(lines(&t)[3].contains(" syncing 2/5…"), "{}", lines(&t)[3]);
        a.in_flight = 0;
        a.progress = None;
        a.message = Some(crate::app::Message::test(
            "error: a long line from git's stderr, cut to fit",
            true,
        ));
        let t = draw(&mut a, 80, 4);
        let bar = &lines(&t)[3];
        assert!(
            bar.starts_with("proj, done between Mo 09-11 and 16-11 error: a long"),
            "{bar}"
        );
        assert!(bar.ends_with('…') && !bar.contains("widget"), "{bar}");
        assert_eq!(t.backend().buffer()[(40, 3)].fg, Color::Red);
    }

    #[test]
    fn the_bar_counts_the_running_batch_until_its_results() {
        let (mut a, _tmp) = app(false);
        let bar = |a: &mut App| lines(&draw(a, 80, 4))[3].clone();
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Down); // N001, then N002 slides under the cursor
        let mut moved = vec![];
        for _ in 0..2 {
            key(&mut a, KeyCode::Char('m'));
            moved.extend(key(&mut a, KeyCode::Char('d')));
        }
        let mut jobs = moved.into_iter().map(|e| match e {
            crate::app::Effect::Run(job) => job,
            e => panic!("{e:?}"),
        });
        // Queued, nothing reported yet.
        assert!(bar(&mut a).ends_with(" syncing…"), "{}", bar(&mut a));
        a.on_progress(Progress { done: 0, total: 5 });
        assert!(bar(&mut a).ends_with(" syncing 0/5…"), "{}", bar(&mut a));
        a.on_progress(Progress { done: 4, total: 5 });
        assert!(bar(&mut a).ends_with(" syncing 4/5…"), "{}", bar(&mut a));
        // Its results are in; the next batch has not reported.
        let now = std::time::Instant::now();
        a.on_job_done(jobs.next().unwrap(), Ok(String::new()), now);
        assert_eq!(a.progress, None);
        assert!(bar(&mut a).ends_with(" syncing…"), "{}", bar(&mut a));
        a.on_job_done(jobs.next().unwrap(), Ok(String::new()), now);
        assert!(
            bar(&mut a).ends_with(" moved N002 to done"),
            "{}",
            bar(&mut a)
        );
    }

    #[test]
    fn the_move_menu_dims_what_the_move_would_refuse() {
        let (mut a, _tmp) = app(false);
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Down); // N001, todo
        key(&mut a, KeyCode::Char('m'));
        let t = draw(&mut a, 80, 12);
        let bar = &lines(&t)[11];
        let menu = "move N001 to  t todo  p planned  s stale  d done  c canceled  esc";
        assert_eq!(bar, menu);
        let dim = |k: &str| modifier(&t, menu.find(k).unwrap() as u16, 11).contains(Modifier::DIM);
        assert!(dim("t todo") && dim("s stale"));
        assert!(!dim("p planned") && !dim("d done") && !dim("c canceled"));
    }

    #[test]
    fn the_priority_prompt_takes_the_bar() {
        let (mut a, _tmp) = app(false);
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Char('p'));
        let t = draw(&mut a, 80, 12);
        assert_eq!(lines(&t)[11], "priority for H010: a-z  esc");
    }

    #[test]
    fn the_form_is_two_boxes_on_the_bottom_rows() {
        let (mut a, _tmp) = app(true);
        key(&mut a, KeyCode::Char('n'));
        for c in "fix the sensor".chars() {
            key(&mut a, KeyCode::Char(c));
        }
        let mut t = draw(&mut a, 48, 12);
        let got = lines(&t);
        assert_eq!(
            got[8], "  N050 shipped",
            "the body keeps the rows above the form"
        );
        assert!(got[9].starts_with("┌prio┐┌title (proj)─"), "{}", got[9]);
        assert_eq!(got[10], format!("│N   ││fix the sensor{}│", " ".repeat(26)));
        assert!(
            got[11].starts_with("└────┘└") && got[11].contains("⏎ create, tab project, esc"),
            "{}",
            got[11]
        );
        assert_eq!(t.get_cursor_position().unwrap(), Position::new(21, 10));

        key(&mut a, KeyCode::Home);
        key(&mut a, KeyCode::Left);
        key(&mut a, KeyCode::Tab);
        let mut t = draw(&mut a, 48, 12);
        assert!(lines(&t)[9].starts_with("┌prio┐┌title (widget)─"));
        assert_eq!(
            t.get_cursor_position().unwrap(),
            Position::new(1, 10),
            "in the prio box"
        );
    }

    #[test]
    fn the_rename_form_shows_one_box_with_the_task_id() {
        let (mut a, _tmp) = app(true);
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Char('r'));
        let mut t = draw(&mut a, 32, 12);
        let got = lines(&t);
        assert!(got[9].starts_with("┌rename H010─"), "{}", got[9]);
        assert_eq!(got[10], format!("│busy{}│", " ".repeat(26)));
        assert!(got[11].ends_with(" ⏎ rename, esc ┘"), "{}", got[11]);
        assert_eq!(t.get_cursor_position().unwrap(), Position::new(5, 10));
    }

    #[test]
    fn a_long_title_scrolls_to_keep_the_text_cursor_in_its_box() {
        let (mut a, _tmp) = app(false);
        key(&mut a, KeyCode::Char('n'));
        for c in "abcdefghijklmnopqrstuvwxyz".chars() {
            key(&mut a, KeyCode::Char(c));
        }
        let mut t = draw(&mut a, 20, 6);
        assert_eq!(lines(&t)[4], "│N   ││pqrstuvwxyz │");
        assert_eq!(t.get_cursor_position().unwrap(), Position::new(18, 4));
    }

    #[test]
    fn peek_overlays_the_pane_with_the_file() {
        let (mut a, tmp) = app(false);
        let body =
            "# Busy\nsecond\n\nA paragraph long enough that it has to wrap at the pane's edge.\n";
        fs::write(tmp.path().join("proj/active/H010-busy.md"), body).unwrap();
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Enter);
        let t = draw(&mut a, 30, 8);
        let got = lines(&t);
        assert_eq!(got[0], "proj/active/H010-busy.md");
        assert!(modifier(&t, 0, 0).contains(Modifier::REVERSED));
        assert_eq!(
            &got[1..6],
            [
                "# Busy",
                "second",
                "",
                "A paragraph long enough that",
                "it has to wrap at the pane's"
            ]
        );
        assert!(got[7].starts_with("↑↓ PgUp PgDn scroll"));
        assert_eq!(a.peek_rows, 6);

        key(&mut a, KeyCode::PageDown);
        let got = lines(&draw(&mut a, 30, 7));
        assert_eq!(got[1], "second", "scrolled to the end, no further");
    }

    #[test]
    fn peek_says_when_the_file_is_empty_or_gone() {
        let (mut a, tmp) = app(false);
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Down); // N002, empty
        key(&mut a, KeyCode::Enter);
        assert_eq!(lines(&draw(&mut a, 30, 6))[1], "(empty file)");
        fs::remove_file(tmp.path().join("proj/todo/N002-beta.md")).unwrap();
        a.refresh_peek();
        assert_eq!(lines(&draw(&mut a, 30, 6))[1], "(the file is gone)");
    }

    #[test]
    fn wrapping_breaks_at_spaces_and_mid_word_when_it_must() {
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("a\n\nb\n", 10), ["a", "", "b"]);
        assert_eq!(wrap("\tx", 10), ["    x"]);
        assert_eq!(wrap("", 10), [""]);
    }
}
