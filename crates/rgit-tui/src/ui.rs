use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style as RStyle};
use ratatui::text::{Line, Span as RSpan};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Padding, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState,
};
use rgit_model::{Span, Style, Target};

use crate::app::{App, ViewKind};
use crate::buffer::Row;
use crate::config::Transparency;
use crate::theme;

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    if app.help {
        render_help(frame, area);
        return;
    }

    // The whole app lives in a rounded frame whose top border carries the repo
    // identity and live state badges; everything else renders inside it.
    let frame_block = app_frame(app);
    let inner = frame_block.inner(area);
    // "body" mode paints solid bands behind the header row and the action bar
    // while leaving the body see-through. The bands cover only the interior
    // width, so the left and right border lines stay clean instead of the
    // background bleeding up to and under them.
    if app.transparency == Transparency::Body {
        // Fill from just inside the left border to just inside the right, so the
        // band meets the border lines edge to edge without painting over them.
        let bx = area.x + 1;
        let bw = area.width.saturating_sub(2);
        let mut bg = |y: u16, height: u16| {
            frame.render_widget(
                Block::default().style(RStyle::default().bg(theme::base_bg())),
                Rect {
                    x: bx,
                    width: bw,
                    y,
                    height,
                },
            );
        };
        bg(area.y, 1); // title row (the identity sits on the top border)
        // The action bar row only, not the bottom border below it, so the border
        // stays clean like the sides.
        if area.height >= 2 {
            bg(area.y + area.height - 2, 1);
        }
    }
    frame.render_widget(frame_block, area);

    if let Some(candidates) = app
        .prompt
        .as_ref()
        .map(|p| p.filtered().len().min(8) as u16)
    {
        let [body, list, input] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(candidates),
            Constraint::Length(1),
        ])
        .areas(inner);
        app.buffer_mut().set_height(body.height as usize);
        render_body(frame, app, body);
        if let Some(prompt) = &app.prompt {
            render_prompt(frame, prompt, list, input);
        }
        return;
    }

    if let Some(height) = app.transient.as_ref().map(transient_height) {
        let [body, menu] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(height)]).areas(inner);
        app.buffer_mut().set_height(body.height as usize);
        render_body(frame, app, body);
        if let Some(transient) = &app.transient {
            render_transient(frame, transient, menu);
        }
        return;
    }

    // The live code finder takes over the body: a query line and results list on
    // the left, a preview of the selected hit on the right.
    if app.code_finder.is_some() {
        let [top, body, action] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(inner);
        let split = body.width >= 90;
        let [list_area, prev_area] = if split {
            Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(body)
        } else {
            [body, Rect { width: 0, ..body }]
        };
        let has_preview = split && app.refresh_preview(prev_area.height.saturating_sub(1) as usize);
        app.set_preview_visible(has_preview);
        render_code_finder(frame, app, top, list_area);
        if has_preview {
            app.preview_top = prev_area.y + 1;
            render_preview(frame, app, prev_area);
        }
        render_action_bar(frame, app, action);
        render_toasts(frame, app, inner);
        return;
    }

    // The action bar sits directly above the bottom border, no separator row.
    let [body, action] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    // When there's room, status and log views split into a navigator and a live
    // preview of the file/commit under the cursor; otherwise a single column.
    // List views whose rows point at previewable content (files, commits, refs)
    // get the side preview; detail views (Diff, Commit, Blame) fill the width.
    let previewable_view = matches!(
        app.active_kind(),
        ViewKind::Status | ViewKind::Log | ViewKind::Smartlog | ViewKind::Stack | ViewKind::Refs
    );
    let wide = app.preview_enabled && body.width >= 100 && previewable_view;
    let [nav, prev] =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)]).areas(body);
    let has_preview = wide && app.refresh_preview(prev.height.saturating_sub(1) as usize);
    app.set_preview_visible(has_preview);
    // Record the split boundary so a mouse wheel scrolls the pane it is over.
    app.split_x = has_preview.then_some(prev.x);
    // The navigator occupies the whole column (single or split); it carries no
    // label header, so its body keeps the same top row whether or not the
    // preview is open (the change count already shows in the title bar).
    let nav_area = if has_preview { nav } else { body };
    app.body_top = nav_area.y;
    app.buffer_mut().set_height(nav_area.height as usize);
    render_body(frame, app, nav_area);
    render_scrollbar(
        frame,
        nav_area,
        app.buffer().len(),
        app.buffer().scroll(),
        nav_area.height as usize,
    );
    if has_preview {
        app.preview_top = prev.y + 1; // + the PREVIEW label row inside the pane
        render_preview(frame, app, prev);
    }
    render_action_bar(frame, app, action);

    // A pending confirmation floats over the dimmed screen.
    if let Some(confirm) = &app.confirm {
        render_confirm(frame, confirm, inner);
    }
    // The command palette floats above everything else in the body.
    if let Some(palette) = &app.palette {
        render_palette(frame, palette, inner);
    }
    if let Some(editor) = &app.commit_editor {
        render_commit_editor(frame, editor, inner);
    }
    if let Some(console) = &app.hook_console {
        render_hook_console(frame, console, app.tick_count, body);
    }
    if let Some(todo) = &app.rebase_todo {
        render_rebase_todo(frame, todo, inner);
    }
    if let Some(picker) = &app.split_picker {
        render_split_picker(frame, picker, inner);
    }
    if let Some(level) = app.leader {
        render_which_key(frame, level, inner);
    }
    render_toasts(frame, app, inner);
}

/// The vim-profile leader (which-key) popup: the keys available after Space.
fn render_which_key(frame: &mut Frame, level: crate::app::Leader, area: Rect) {
    use crate::app::Leader;
    let (title, entries): (&str, Vec<(char, &str)>) = match level {
        Leader::Root => ("leader", crate::keymap::leader_entries()),
        Leader::Window => (
            "leader ▸ window",
            vec![('h', "focus navigator"), ('l', "focus preview")],
        ),
    };
    // Two columns keep the popup compact for the ~20 root entries.
    let cols = 2;
    let rows_per_col = entries.len().div_ceil(cols);
    let width = 60.min(area.width.saturating_sub(2));
    let height = (rows_per_col as u16 + 3).min(area.height.saturating_sub(2));
    let inner = popup_panel(frame, title, width, height, area);
    let [body, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);

    // Keys read as raised "keycaps"; labels stay dim.
    let keycap = keycap_style();
    let dim = theme::resolve(Style::Dim);
    let lines: Vec<Line> = (0..rows_per_col)
        .map(|r| {
            let mut spans = Vec::new();
            for c in 0..cols {
                if let Some((k, label)) = entries.get(r + c * rows_per_col) {
                    spans.push(RSpan::styled(format!(" {k} "), keycap));
                    spans.push(RSpan::styled(format!(" {label:<22}"), dim));
                }
            }
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), body);
    frame.render_widget(
        Paragraph::new(Line::from(RSpan::styled(" esc cancel", dim))),
        footer,
    );
}

fn render_rebase_todo(frame: &mut Frame, todo: &crate::app::RebaseTodo, area: Rect) {
    let width = 82.min(area.width.saturating_sub(2));
    // Grow to the history, but leave room for the frame, title and footer so the
    // list scrolls instead of overflowing the popup for long histories.
    let cap = (area.height.saturating_sub(6)).max(1) as usize;
    let visible = todo.entries.len().clamp(1, cap);
    let height = visible as u16 + 4;
    let title = format!(
        "interactive rebase  {}/{}",
        todo.cursor + 1,
        todo.entries.len()
    );
    let inner = popup_panel(frame, &title, width, height, area);
    let [list, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);

    // Scroll so the cursor stays in view (anchored to the bottom when moving down).
    let vis = (list.height as usize).max(1);
    let scroll = todo.cursor.saturating_sub(vis - 1);
    let end = (scroll + vis).min(todo.entries.len());
    let rows: Vec<Line> = todo.entries[scroll..end]
        .iter()
        .enumerate()
        .map(|(vi, e)| {
            let i = scroll + vi;
            let (word, style) = match e.action {
                's' => ("squash", Style::Modified),
                'f' => ("fixup", Style::Branch),
                'd' => ("drop", Style::Deleted),
                'r' => ("reword", Style::Added),
                'e' => ("edit", Style::Added),
                _ => ("pick", Style::Dim),
            };
            let bar = if i == todo.cursor { "▎" } else { " " };
            let line = Line::from(vec![
                RSpan::styled(bar, RStyle::default().fg(theme::accent())),
                RSpan::styled(format!("{word:<7}"), theme::resolve(style)),
                RSpan::styled(format!("{} ", e.short), theme::resolve(Style::Hash)),
                RSpan::raw(e.subject.clone()),
            ]);
            if i == todo.cursor {
                line.style(RStyle::default().bg(theme::cursor_bg()))
            } else {
                line
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), list);

    let accent = RStyle::default().fg(theme::accent());
    let dim = theme::resolve(Style::Dim);
    // A squash/fixup at the top has nothing to fold into; warn instead of the hint.
    let footer_line = match todo.entries.first().map(|e| e.action) {
        Some('s') | Some('f') => Line::from(RSpan::styled(
            " first commit cannot be squash/fixup - nothing precedes it",
            theme::resolve(Style::Deleted),
        )),
        _ => Line::from(vec![
            RSpan::styled("j/k", accent),
            RSpan::styled(" move  ", dim),
            RSpan::styled("J/K", accent),
            RSpan::styled(" reorder  ", dim),
            RSpan::styled("p/s/f/d/r/e", accent),
            RSpan::styled(" mark  ", dim),
            RSpan::styled("⏎", accent),
            RSpan::styled(" run  ", dim),
            RSpan::styled("esc", accent),
            RSpan::styled(" cancel", dim),
        ]),
    };
    frame.render_widget(Paragraph::new(footer_line), footer);
}

fn render_split_picker(frame: &mut Frame, picker: &crate::app::SplitPicker, area: Rect) {
    let width = 82.min(area.width.saturating_sub(2));
    let cap = (area.height.saturating_sub(6)).max(1) as usize;
    let visible = picker.files.len().clamp(1, cap);
    let height = visible as u16 + 4;
    let title = format!(
        "split {}  {}/{}",
        picker.rev,
        picker.cursor + 1,
        picker.files.len()
    );
    let inner = popup_panel(frame, &title, width, height, area);
    let [list, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);

    let vis = (list.height as usize).max(1);
    let scroll = picker.cursor.saturating_sub(vis - 1);
    let end = (scroll + vis).min(picker.files.len());
    let rows: Vec<Line> = picker.files[scroll..end]
        .iter()
        .enumerate()
        .map(|(vi, (path, on))| {
            let i = scroll + vi;
            let bar = if i == picker.cursor { "▎" } else { " " };
            let box_char = if *on { "[x]" } else { "[ ]" };
            let style = if *on {
                theme::resolve(Style::Added)
            } else {
                theme::resolve(Style::Dim)
            };
            let line = Line::from(vec![
                RSpan::styled(bar, RStyle::default().fg(theme::accent())),
                RSpan::styled(format!("{box_char} "), style),
                RSpan::raw(path.clone()),
            ]);
            if i == picker.cursor {
                line.style(RStyle::default().bg(theme::cursor_bg()))
            } else {
                line
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), list);

    let accent = RStyle::default().fg(theme::accent());
    let dim = theme::resolve(Style::Dim);
    let footer_line = Line::from(vec![
        RSpan::styled("j/k", accent),
        RSpan::styled(" move  ", dim),
        RSpan::styled("space", accent),
        RSpan::styled(" toggle  ", dim),
        RSpan::styled("⏎", accent),
        RSpan::styled(" split  ", dim),
        RSpan::styled("esc", accent),
        RSpan::styled(" cancel", dim),
    ]);
    frame.render_widget(Paragraph::new(footer_line), footer);
}

fn render_commit_editor(frame: &mut Frame, editor: &crate::app::CommitEditor, area: Rect) {
    let title = if editor.amend { "amend" } else { "commit" };
    let width = 66.min(area.width.saturating_sub(2));
    let height = 12.min(area.height.saturating_sub(2));
    let inner = popup_panel(frame, title, width, height, area);
    let [text_area, footer] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);

    // The subject line renders as-is; a dim rule separates it from the body.
    let lines: Vec<Line> = editor
        .lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if i == 0 {
                Line::from(RSpan::styled(l.clone(), theme::resolve(Style::Plain)))
            } else {
                Line::from(RSpan::styled(l.clone(), theme::resolve(Style::Dim)))
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), text_area);

    // Place the real terminal cursor at the editor's caret when it is in view.
    let cy = text_area.y.saturating_add(editor.row as u16);
    let cx = text_area.x.saturating_add(editor.col as u16);
    if cy < text_area.bottom() && cx < text_area.right() {
        frame.set_cursor_position((cx, cy));
    }

    let subject = editor.subject_len();
    let counter = if subject > 50 {
        theme::resolve(Style::Modified)
    } else {
        theme::resolve(Style::Dim)
    };
    let accent = RStyle::default().fg(theme::accent());
    let dim = theme::resolve(Style::Dim);
    let footer_line = Line::from(vec![
        RSpan::styled(format!("{subject}/50  "), counter),
        RSpan::styled("^S", accent),
        RSpan::styled(" commit  ", dim),
        RSpan::styled("^G", accent),
        RSpan::styled(" generate  ", dim),
        RSpan::styled("^O", accent),
        RSpan::styled(" $EDITOR  ", dim),
        RSpan::styled("esc", accent),
        RSpan::styled(" cancel", dim),
    ]);
    frame.render_widget(Paragraph::new(footer_line), footer);
}

/// Parse one line of streamed op output (hook script or network progress)
/// into styled spans. SGR sequences (`ESC [ ... m`) become ratatui styles,
/// so a hook that forces color renders as intended; every other escape
/// sequence is dropped — cursor movement has no meaning inside the pane.
/// Lines without any escape take a zero-allocation fast path beyond the one
/// String the pane already had to own.
pub fn ansi_line(line: &str) -> Line<'static> {
    if !line.contains('\x1b') {
        return Line::from(RSpan::styled(line.to_owned(), RStyle::default()));
    }
    let mut spans: Vec<RSpan<'static>> = Vec::new();
    let mut text = String::new();
    let mut style = RStyle::default();
    let mut rest = line;
    while let Some(esc) = rest.find('\x1b') {
        text.push_str(&rest[..esc]);
        let after = &rest[esc + 1..];
        let Some(after) = after.strip_prefix('[') else {
            rest = after; // a lone ESC: drop it
            continue;
        };
        // CSI: parameter/intermediate bytes, then a final byte 0x40..=0x7e.
        match after.find(|c: char| (0x40..=0x7e).contains(&(c as u32))) {
            Some(i) => {
                if after.as_bytes()[i] == b'm' {
                    if !text.is_empty() {
                        spans.push(RSpan::styled(std::mem::take(&mut text), style));
                    }
                    apply_sgr(&after[..i], &mut style);
                }
                rest = &after[i + 1..];
            }
            None => break,
        }
    }
    text.push_str(rest);
    if !text.is_empty() {
        spans.push(RSpan::styled(text, style));
    }
    Line::from(spans)
}

/// Apply one SGR parameter string (`"1;31"`) to a style.
fn apply_sgr(params: &str, style: &mut RStyle) {
    let mut parts = params.split(';').peekable();
    while let Some(p) = parts.next() {
        let n: u16 = if p.is_empty() {
            0
        } else {
            p.parse().unwrap_or(0)
        };
        match n {
            0 => *style = RStyle::default(),
            1 => *style = style.add_modifier(Modifier::BOLD),
            2 => *style = style.add_modifier(Modifier::DIM),
            3 => *style = style.add_modifier(Modifier::ITALIC),
            4 => *style = style.add_modifier(Modifier::UNDERLINED),
            7 => *style = style.add_modifier(Modifier::REVERSED),
            9 => *style = style.add_modifier(Modifier::CROSSED_OUT),
            22 => *style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => *style = style.remove_modifier(Modifier::ITALIC),
            24 => *style = style.remove_modifier(Modifier::UNDERLINED),
            27 => *style = style.remove_modifier(Modifier::REVERSED),
            29 => *style = style.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 => *style = style.fg(ansi16(n - 30)),
            38 => match parts.next() {
                Some("5") => {
                    let c = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                    *style = style.fg(Color::Indexed(c));
                }
                Some("2") => {
                    let next = |v: Option<&str>| v.and_then(|s| s.parse().ok()).unwrap_or(0);
                    let (r, g, b) = (next(parts.next()), next(parts.next()), next(parts.next()));
                    *style = style.fg(Color::Rgb(r, g, b));
                }
                _ => {}
            },
            39 => style.fg = None,
            40..=47 => *style = style.bg(ansi16(n - 40)),
            48 => match parts.next() {
                Some("5") => {
                    let c = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                    *style = style.bg(Color::Indexed(c));
                }
                Some("2") => {
                    let next = |v: Option<&str>| v.and_then(|s| s.parse().ok()).unwrap_or(0);
                    let (r, g, b) = (next(parts.next()), next(parts.next()), next(parts.next()));
                    *style = style.bg(Color::Rgb(r, g, b));
                }
                _ => {}
            },
            49 => style.bg = None,
            90..=97 => *style = style.fg(ansi16(n - 90 + 8)),
            100..=107 => *style = style.bg(ansi16(n - 100 + 8)),
            _ => {}
        }
    }
}

/// The 16 standard colors as named variants, so a terminal palette theme
/// applies; 256-color and RGB stays indexed.
fn ansi16(n: u16) -> Color {
    match n {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::White,
        8 => Color::DarkGray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        _ => Color::Gray,
    }
}

/// The live hook console: a large panel streaming `git commit` output, with a
/// status title and a footer that offers retry/dismiss once the run finishes.
fn render_hook_console(
    frame: &mut Frame,
    console: &crate::app::HookConsole,
    tick: usize,
    area: Rect,
) {
    use crate::app::HookStatus;
    let name = &console.title;
    let (title, title_style) = match console.status {
        HookStatus::Running => (
            format!("{} {name}", spinner_frame(tick)),
            RStyle::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ),
        HookStatus::Passed => (
            format!("✓ {name}"),
            theme::resolve(Style::Added).add_modifier(Modifier::BOLD),
        ),
        HookStatus::Failed => (
            format!("✗ {name} failed"),
            theme::resolve(Style::Deleted).add_modifier(Modifier::BOLD),
        ),
    };

    // Dock the console as a full-width pane along the bottom, like an editor's
    // integrated terminal - more intuitive for streaming output than a centered
    // popup, and it leaves the view above visible.
    let height = (area.height * 2 / 5)
        .clamp(8, 18)
        .min(area.height.saturating_sub(1));
    let rect = Rect {
        x: area.x,
        y: area.y + area.height.saturating_sub(height),
        width: area.width,
        height,
    };
    frame.render_widget(Clear, rect);
    let border = match console.status {
        HookStatus::Failed => theme::resolve(Style::Deleted),
        _ => RStyle::default().fg(theme::accent()),
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border)
        .style(RStyle::default().bg(theme::overlay_bg()))
        .padding(Padding::horizontal(1))
        .title_top(RSpan::styled(format!(" {title} "), title_style));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    // A progress bar sits between the output and the footer while transferring.
    let bar_h = u16::from(console.progress.is_some_and(|(_, t)| t > 0));
    let [body, bar, footer] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(bar_h),
        Constraint::Length(1),
    ])
    .areas(inner);

    // Follow the tail unless the user scrolled up; then honor their offset.
    let rows = body.height as usize;
    let top = if console.follow {
        console.lines.len().saturating_sub(rows)
    } else {
        console.scroll.min(console.lines.len().saturating_sub(1))
    };
    let lines: Vec<Line> = if console.lines.is_empty() {
        vec![Line::from(RSpan::styled(
            "starting…",
            theme::resolve(Style::Dim),
        ))]
    } else {
        // Spans were parsed at ingest; the visible window clones them as-is.
        console.lines[top..].iter().take(rows).cloned().collect()
    };
    frame.render_widget(Paragraph::new(lines), body);

    if let Some((received, total)) = console.progress.filter(|(_, t)| *t > 0) {
        render_transfer_bar(frame, bar, received, total);
    }

    let accent = RStyle::default().fg(theme::accent());
    let dim = theme::resolve(Style::Dim);
    let footer_line = match console.status {
        HookStatus::Running => Line::from(RSpan::styled(" working…", dim)),
        // Retry only applies to a failed run; a held success just dismisses.
        HookStatus::Failed => Line::from(vec![
            RSpan::styled(" e ", keycap_style()),
            RSpan::styled(" retry   ", dim),
            RSpan::styled(" q ", keycap_style()),
            RSpan::styled(" dismiss   ", dim),
            RSpan::styled("j/k", accent),
            RSpan::styled(" scroll", dim),
        ]),
        HookStatus::Passed => Line::from(vec![
            RSpan::styled(" q ", keycap_style()),
            RSpan::styled(" close   ", dim),
            RSpan::styled("j/k", accent),
            RSpan::styled(" scroll", dim),
        ]),
    };
    frame.render_widget(Paragraph::new(footer_line), footer);
}

/// A transfer progress bar: `▕████░░░░▏ 45% (450/1000)`.
fn render_transfer_bar(frame: &mut Frame, area: Rect, received: usize, total: usize) {
    let frac = (received as f64 / total as f64).clamp(0.0, 1.0);
    let pct = (frac * 100.0).round() as u32;
    let label = format!(" {pct}% ({received}/{total})");
    let track = (area.width as usize).saturating_sub(label.chars().count() + 2);
    let filled = (frac * track as f64).round() as usize;
    let (open, fill, empty, close) = if rgit_model::unicode() {
        ("▕", "█", "░", "▏")
    } else {
        ("[", "#", "-", "]")
    };
    let bar = format!(
        "{open}{}{}{close}",
        fill.repeat(filled),
        empty.repeat(track.saturating_sub(filled))
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            RSpan::styled(bar, RStyle::default().fg(theme::accent())),
            RSpan::styled(label, theme::resolve(Style::Dim)),
        ])),
        area,
    );
}

fn render_code_finder(frame: &mut Frame, app: &App, input_area: Rect, list_area: Rect) {
    let Some(f) = &app.code_finder else { return };
    // Query line: prompt caret, the text, a caret, then a state hint.
    let hint = if f.searching {
        " searching…".to_owned()
    } else if f.semantic {
        format!("  {} hits (meaning + text)  ^o: editor", f.hits.len())
    } else if f.hits.is_empty() {
        "  type to search, Tab for meaning".to_owned()
    } else {
        format!("  {} hits (text)  Tab: meaning  ^o: editor", f.hits.len())
    };
    // Split the query at the caret so it renders between the two halves.
    let chars: Vec<char> = f.input.chars().collect();
    let at = f.cursor.min(chars.len());
    let before: String = chars[..at].iter().collect();
    let after: String = chars[at..].iter().collect();
    let input_line = Line::from(vec![
        RSpan::styled("code › ", RStyle::default().fg(theme::accent())),
        RSpan::raw(before),
        RSpan::styled("\u{258f}", RStyle::default().fg(theme::accent())),
        RSpan::raw(after),
        RSpan::styled(hint, theme::resolve(Style::Dim)),
    ]);
    frame.render_widget(Paragraph::new(input_line), input_area);

    let rows: Vec<Line> = f
        .hits
        .iter()
        .take(list_area.height as usize)
        .enumerate()
        .map(|(i, h)| {
            let selected = i == f.selected;
            let loc = RSpan::styled(
                format!("{}:{}", h.path, h.line),
                theme::resolve(Style::Hash),
            );
            let tag = RSpan::styled(format!("  [{}]  ", h.tag), theme::resolve(Style::Branch));
            let preview = RSpan::styled(
                h.preview.clone(),
                if selected {
                    theme::resolve(Style::Plain)
                } else {
                    theme::resolve(Style::Dim)
                },
            );
            let line = Line::from(vec![loc, tag, preview]);
            if selected {
                line.style(RStyle::default().bg(theme::select_bg()))
            } else {
                line
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), list_area);
}

fn render_palette(frame: &mut Frame, palette: &crate::app::Palette, area: Rect) {
    use crate::app::Palette;
    let matches = Palette::matches(&palette.input);
    let width = 52.min(area.width.saturating_sub(2));
    let height = (matches.len().min(8) as u16) + 3;
    let inner = popup_panel(frame, "run a command", width, height, area);

    let [input_area, list_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);

    // Split the query at the caret so it renders between the two halves.
    let pchars: Vec<char> = palette.input.chars().collect();
    let pat = palette.cursor.min(pchars.len());
    let pbefore: String = pchars[..pat].iter().collect();
    let pafter: String = pchars[pat..].iter().collect();
    let input_line = Line::from(vec![
        RSpan::styled("› ", RStyle::default().fg(theme::accent())),
        RSpan::raw(pbefore),
        RSpan::styled("\u{258f}", RStyle::default().fg(theme::accent())),
        RSpan::raw(pafter),
    ]);
    frame.render_widget(Paragraph::new(input_line), input_area);

    let rows: Vec<Line> = matches
        .iter()
        .take(list_area.height as usize)
        .enumerate()
        .map(|(i, entry)| {
            let selected = i == palette.selected;
            // The shortcut sits in a keycap chip; unbound entries get blank space
            // the same width so the labels still line up.
            let key = if entry.keyhint.is_empty() {
                RSpan::raw("   ")
            } else {
                RSpan::styled(format!(" {} ", entry.keyhint), keycap_style())
            };
            let label = RSpan::styled(
                format!("  {}", entry.label),
                if selected {
                    RStyle::default()
                        .fg(theme::accent())
                        .add_modifier(Modifier::BOLD)
                } else {
                    theme::resolve(Style::Plain)
                },
            );
            let line = Line::from(vec![key, label]);
            if selected {
                line.style(RStyle::default().bg(theme::select_bg()))
            } else {
                line
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), list_area);
}

/// A rect of `width` x `height` centered within `area` (clamped to fit).
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

/// Dim every cell in `area` so a popup drawn over it reads as elevated.
fn dim_background(frame: &mut Frame, area: Rect) {
    let buf = frame.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            buf[(x, y)].set_style(RStyle::default().add_modifier(Modifier::DIM));
        }
    }
}

/// A centered, shadowed, rounded popup panel titled `title`; returns the inner
/// content area to render into.
fn popup_panel(frame: &mut Frame, title: &str, width: u16, height: u16, area: Rect) -> Rect {
    dim_background(frame, area);
    let rect = centered_rect(width, height, area);
    draw_shadow(frame, rect);
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(RStyle::default().fg(theme::accent()))
        .style(RStyle::default().bg(theme::overlay_bg()))
        .padding(Padding::horizontal(1))
        .title_top(RSpan::styled(
            format!(" {title} "),
            RStyle::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    inner
}

/// A soft drop-shadow one cell down-right of `rect`.
fn draw_shadow(frame: &mut Frame, rect: Rect) {
    let area = frame.area();
    let shade = RStyle::default().bg(Color::Rgb(0, 0, 0));
    let buf = frame.buffer_mut();
    for y in (rect.y + 1)..=(rect.y + rect.height) {
        for x in (rect.x + 1)..=(rect.x + rect.width) {
            if x < area.right() && y < area.bottom() {
                buf[(x, y)].set_style(shade);
            }
        }
    }
}

fn render_confirm(frame: &mut Frame, confirm: &crate::app::PendingConfirm, area: Rect) {
    let width = (confirm.prompt.len() as u16 + 10).clamp(28, area.width.saturating_sub(4));
    let inner = popup_panel(frame, "confirm", width, 4, area);
    let lines = vec![
        Line::from(RSpan::styled(
            confirm.prompt.clone(),
            theme::resolve(Style::Plain),
        )),
        Line::from(vec![
            RSpan::styled(" y ", keycap_style()),
            RSpan::styled(" yes    ", theme::resolve(Style::Dim)),
            RSpan::styled(" n ", keycap_style()),
            RSpan::styled(" no", theme::resolve(Style::Dim)),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The rounded outer frame; identity sits top-left, state badges top-right.
fn app_frame(app: &App) -> Block<'static> {
    let identity = Line::from(vec![
        RSpan::styled(
            rgit_model::glyph(" ◆ rgit ", " rgit "),
            RStyle::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ),
        RSpan::styled(format!("{} ", app.repo_name()), theme::resolve(Style::Dim)),
    ]);
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(RStyle::default().fg(theme::border()))
        .padding(Padding::horizontal(1))
        .title_top(identity)
        .title_top(title_badges(app).right_aligned());
    // Paint the theme's solid background so the app reads as one contained
    // surface. In `body` mode the chrome bands are painted separately, and in
    // `full` nothing is painted.
    if app.transparency == Transparency::Off {
        block = block.style(RStyle::default().bg(theme::base_bg()));
    }
    block
}

/// Branch, divergence, change count, sequencer state, and activity.
fn title_badges(app: &App) -> Line<'static> {
    let mut spans = Vec::new();
    if let Some(head) = &app.head {
        spans.push(RSpan::styled(
            format!(" {}", head.describe()),
            RStyle::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ));
        // Ahead/behind as separate colored chips, each shown only when nonzero.
        if head.ahead > 0 {
            spans.push(RSpan::styled(
                format!("  {}{}", rgit_model::glyph("↑", "+"), head.ahead),
                theme::resolve(Style::Added),
            ));
        }
        if head.behind > 0 {
            spans.push(RSpan::styled(
                format!("  {}{}", rgit_model::glyph("↓", "-"), head.behind),
                theme::resolve(Style::Deleted),
            ));
        }
    }
    if app.changed > 0 {
        spans.push(RSpan::styled(
            format!("  {} {}", rgit_model::glyph("●", "*"), app.changed),
            theme::resolve(Style::Modified),
        ));
    }
    let stashes = app.stash_count();
    if stashes > 0 {
        spans.push(RSpan::styled(
            format!("  {} {stashes}", rgit_model::glyph("⚑", "#")),
            theme::resolve(Style::Branch),
        ));
    }
    if let Some(state) = app.state.label() {
        spans.push(RSpan::styled(
            format!("  [{state}]"),
            theme::resolve(Style::Modified).add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(busy) = &app.busy {
        spans.push(RSpan::styled(
            format!("  {} {busy}", spinner_frame(app.tick_count)),
            RStyle::default().fg(theme::accent()),
        ));
    } else if app.loading {
        spans.push(RSpan::styled(
            format!("  {}", spinner_frame(app.tick_count)),
            theme::resolve(Style::Dim),
        ));
    }
    spans.push(RSpan::raw(" "));
    Line::from(spans)
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const SPINNER_ASCII: [&str; 4] = ["|", "/", "-", "\\"];

fn spinner_frame(tick: usize) -> &'static str {
    if rgit_model::unicode() {
        SPINNER[tick % SPINNER.len()]
    } else {
        SPINNER_ASCII[tick % SPINNER_ASCII.len()]
    }
}

/// Stack active toasts in the lower-right corner, newest at the bottom.
fn render_toasts(frame: &mut Frame, app: &App, area: Rect) {
    use crate::app::ToastKind;
    for (i, toast) in app.toasts.iter().rev().enumerate() {
        let text = format!(" {} {} ", toast_glyph(toast.kind), toast.text);
        let w = (text.chars().count() as u16 + 3).min(area.width.saturating_sub(2));
        let h = 3;
        let y = area.bottom().saturating_sub(2 + h * (i as u16 + 1));
        if y < area.top() {
            break;
        }
        let rect = Rect {
            x: area.right().saturating_sub(w + 2),
            y,
            width: w,
            height: h,
        };
        let accent = match toast.kind {
            ToastKind::Success => theme::resolve(Style::Added),
            ToastKind::Error => theme::resolve(Style::Deleted),
        };
        draw_shadow(frame, rect);
        frame.render_widget(Clear, rect);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(accent)
            .style(RStyle::default().bg(theme::overlay_bg()));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        // The glyph rides in a filled chip; the message stays bold beside it.
        let chip = RStyle::default()
            .fg(theme::overlay_bg())
            .bg(match toast.kind {
                ToastKind::Success => theme::resolve(Style::Added).fg.unwrap_or(theme::accent()),
                ToastKind::Error => theme::resolve(Style::Deleted).fg.unwrap_or(theme::accent()),
            })
            .add_modifier(Modifier::BOLD);
        let line = Line::from(vec![
            RSpan::styled(format!(" {} ", toast_glyph(toast.kind)), chip),
            RSpan::styled(
                format!(" {}", toast.text),
                accent.add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(Paragraph::new(line), inner);
    }
}

fn toast_glyph(kind: crate::app::ToastKind) -> &'static str {
    match kind {
        crate::app::ToastKind::Success => rgit_model::glyph("✓", "+"),
        crate::app::ToastKind::Error => rgit_model::glyph("✕", "x"),
    }
}

fn transient_height(t: &crate::app::Transient) -> u16 {
    // title + (Arguments header + args, if any) + Actions header + actions + hint
    let args = if t.args.is_empty() {
        0
    } else {
        t.args.len() + 1
    };
    (2 + args + t.actions.len() + 1) as u16
}

fn render_transient(frame: &mut Frame, t: &crate::app::Transient, area: Rect) {
    let keycap = keycap_style();
    let heading = pane_label_style();
    let dim = theme::resolve(Style::Dim);

    let mut lines: Vec<Line> = vec![Line::from(RSpan::styled(
        format!(" {} ", t.title),
        RStyle::default()
            .fg(theme::accent())
            .add_modifier(Modifier::BOLD),
    ))];

    if !t.args.is_empty() {
        lines.push(Line::from(RSpan::styled(" ARGUMENTS", heading)));
    }
    for arg in &t.args {
        let mark = if arg.on { "◉ " } else { "○ " };
        lines.push(Line::from(vec![
            RSpan::raw(" "),
            RSpan::styled(format!(" {} ", arg.key), keycap),
            RSpan::styled(
                format!("  {mark}"),
                theme::resolve(if arg.on { Style::Added } else { Style::Dim }),
            ),
            RSpan::raw(arg.label.clone()),
        ]));
    }

    lines.push(Line::from(RSpan::styled(" ACTIONS", heading)));
    for action in &t.actions {
        lines.push(Line::from(vec![
            RSpan::raw(" "),
            RSpan::styled(format!(" {} ", action.key), keycap),
            RSpan::raw("  "),
            RSpan::raw(action.label.clone()),
        ]));
    }
    lines.push(Line::from(RSpan::styled(" esc cancel", dim)));

    frame.render_widget(Paragraph::new(lines), area);
}

fn render_prompt(frame: &mut Frame, prompt: &crate::app::Prompt, list: Rect, input: Rect) {
    let filtered = prompt.filtered();
    let rows: Vec<Line> = filtered
        .iter()
        .enumerate()
        .map(|(i, cand)| {
            let matched = prompt.matched_positions(cand);
            let mut spans = vec![RSpan::raw("  ")];
            for (x, ch) in cand.chars().enumerate() {
                let style = if matched.contains(&x) {
                    RStyle::default().fg(theme::accent())
                } else {
                    theme::resolve(Style::Dim)
                };
                spans.push(RSpan::styled(ch.to_string(), style));
            }
            let mut line = Line::from(spans);
            if i == prompt.selected {
                line = line.style(RStyle::default().bg(theme::select_bg()).fg(theme::accent()));
            }
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), list);

    // Render the caret as a reversed cell at the cursor's char position. A
    // masked prompt (a password/passphrase) shows dots instead of the text.
    let chars: Vec<char> = if prompt.masked {
        std::iter::repeat_n('\u{2022}', prompt.input.chars().count()).collect()
    } else {
        prompt.input.chars().collect()
    };
    let cur = prompt.cursor.min(chars.len());
    let before: String = chars[..cur].iter().collect();
    let (at, after) = if cur < chars.len() {
        (chars[cur].to_string(), chars[cur + 1..].iter().collect())
    } else {
        (" ".to_string(), String::new())
    };
    let line = Line::from(vec![
        RSpan::styled(
            format!(" {}: ", prompt.label),
            RStyle::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ),
        RSpan::raw(before),
        RSpan::styled(at, RStyle::default().add_modifier(Modifier::REVERSED)),
        RSpan::raw(after),
    ]);
    frame.render_widget(Paragraph::new(line), input);
}

/// The adaptive preview pane: a left divider and the cursor file's cached diff.
fn render_preview(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.preview_focus;
    let border_style = if focused {
        RStyle::default().fg(theme::accent())
    } else {
        RStyle::default().fg(theme::border())
    };
    let mut block = Block::default()
        .borders(Borders::LEFT)
        .border_style(border_style)
        .padding(Padding::horizontal(1));
    // A raised surface tint sets the preview off from the navigator - depth
    // without a heavy divider. Only when the app is painted solid.
    if app.transparency == Transparency::Off {
        block = block.style(RStyle::default().bg(theme::surface_bg()));
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // A header labels the pane and names the file/commit it is showing.
    let [header, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
    let title = app.preview_title();
    let mut spans = vec![RSpan::styled("PREVIEW", pane_label_style())];
    if !title.is_empty() {
        spans.push(RSpan::styled(
            format!(" · {}", title.to_uppercase()),
            theme::resolve(Style::Dim),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), header);

    // A background diff build shows a centered spinner until its result lands.
    if app.preview_loading() && app.preview_buffer().is_empty() {
        let line = Line::from(vec![
            RSpan::styled(
                format!("{} ", spinner_frame(app.tick_count)),
                RStyle::default().fg(theme::accent()),
            ),
            RSpan::styled("rendering diff", theme::resolve(Style::Dim)),
        ]);
        let mid = Rect {
            y: body.y + body.height / 2,
            height: 1,
            ..body
        };
        frame.render_widget(Paragraph::new(line).centered(), mid);
        return;
    }

    // Show the cursor only while the pane is focused; otherwise it is a passive
    // follower of the navigator.
    let buf = app.preview_buffer();
    let cursor = buf.cursor();
    let scroll = buf.scroll();
    let vim = crate::keymap::profile() == crate::keymap::Profile::Vim;
    let lines: Vec<Line> = buf
        .viewport()
        .enumerate()
        .map(|(i, row)| {
            render_row(
                &row,
                row_hl(buf, scroll + i, cursor, focused, vim),
                body.width,
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), body);
    render_scrollbar(frame, body, buf.len(), buf.scroll(), body.height as usize);
}

/// The dim uppercase style shared by the pane labels.
fn pane_label_style() -> RStyle {
    theme::resolve(Style::Dim).add_modifier(Modifier::BOLD)
}

/// The raised-keycap look shared by the help, which-key, and transient menus:
/// accent glyph on the cursor tint, bold.
fn keycap_style() -> RStyle {
    RStyle::default()
        .fg(theme::accent())
        .bg(theme::cursor_bg())
        .add_modifier(Modifier::BOLD)
}

/// The highlight for one visible row: cursor line, linewise/charwise selection,
/// and the block caret (vim charwise).
fn row_hl(
    buf: &crate::buffer::Buffer,
    idx: usize,
    cursor: usize,
    show_cursor: bool,
    vim: bool,
) -> Hl {
    let on_cursor = show_cursor && idx == cursor;
    // While a selection is live, drop the full-row cursor tint so the highlight
    // (a similar background) stands out; keep the block caret on the cursor row.
    let cursorline = on_cursor && !buf.has_selection();
    // Draw the block caret whenever the char caret is engaged - always in vim,
    // and in the magit/emacs profile once the caret leaves column 0 (C-f/C-b) or
    // a selection is active - so character movement is visible in both profiles.
    let caret_engaged = vim || buf.char_col() > 0 || buf.has_selection();
    let caret = (on_cursor && caret_engaged).then(|| buf.char_col());

    if let Some(((r0, c0), (r1, c1))) = buf.char_selection() {
        let sel = if idx < r0 || idx > r1 {
            Sel::None
        } else if r0 == r1 {
            Sel::Range(c0, c1)
        } else if idx == r0 {
            Sel::Range(c0, usize::MAX)
        } else if idx == r1 {
            Sel::Range(0, c1)
        } else {
            Sel::Full
        };
        return Hl {
            cursorline,
            sel,
            caret,
        };
    }
    if buf
        .selection_range()
        .is_some_and(|(lo, hi)| idx >= lo && idx <= hi)
    {
        return Hl {
            cursorline,
            sel: Sel::Full,
            caret,
        };
    }
    Hl {
        cursorline,
        sel: Sel::None,
        caret,
    }
}

/// A thin scrollbar on the right edge of `area`, drawn only when the content
/// overflows the viewport.
fn render_scrollbar(frame: &mut Frame, area: Rect, len: usize, pos: usize, page: usize) {
    if len <= page || area.height < 2 {
        return;
    }
    // Ratatui maps position over 0..=content_length-1, but our scroll only
    // reaches len-page; scale it so the thumb spans the whole track and lands
    // at the bottom when the last row is in view.
    let position = pos * (len - 1) / (len - page);
    let mut state = ScrollbarState::new(len)
        .viewport_content_length(page)
        .position(position);
    let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_symbol(Some("│"))
        .thumb_symbol("█")
        .track_style(theme::resolve(Style::Dim))
        .thumb_style(RStyle::default().fg(theme::border()));
    frame.render_stateful_widget(bar, area, &mut state);
}

fn render_body(frame: &mut Frame, app: &App, area: Rect) {
    if app.buffer().is_empty() {
        // Center a spinner while loading, or a quiet resting message otherwise.
        let line = if app.loading {
            Line::from(vec![
                RSpan::styled(
                    format!("{} ", spinner_frame(app.tick_count)),
                    RStyle::default().fg(theme::accent()),
                ),
                RSpan::styled("loading", theme::resolve(Style::Dim)),
            ])
        } else {
            Line::from(RSpan::styled("no changes", theme::resolve(Style::Dim)))
        };
        let mid = Rect {
            y: area.y + area.height / 2,
            height: 1,
            ..area
        };
        frame.render_widget(Paragraph::new(line).centered(), mid);
        return;
    }

    let buf = app.buffer();
    let cursor = buf.cursor();
    let scroll = buf.scroll();
    let vim = crate::keymap::profile() == crate::keymap::Profile::Vim;
    let lines: Vec<Line> = buf
        .viewport()
        .enumerate()
        .map(|(i, row)| render_row(&row, row_hl(buf, scroll + i, cursor, true, vim), area.width))
        .collect();

    frame.render_widget(Paragraph::new(lines), area);
}

/// Per-row highlight state, computed by the body/preview renderers.
#[derive(Clone, Copy)]
enum Sel {
    None,
    /// The whole content row (linewise visual, or a charwise middle row).
    Full,
    /// An inclusive char-column range of the content (charwise boundary rows).
    Range(usize, usize),
}

#[derive(Clone, Copy)]
struct Hl {
    /// Full-row cursor tint (the cursor line).
    cursorline: bool,
    sel: Sel,
    /// Block caret at this content char column (charwise cursor).
    caret: Option<usize>,
}

fn row_prefix<'a>(row: &Row<'a>, cursorline: bool) -> Vec<RSpan<'a>> {
    let mut spans = Vec::with_capacity(row.spans.len() + 3);
    // An accent bar marks the cursor row; other rows keep the column blank.
    if cursorline {
        spans.push(RSpan::styled("▎", RStyle::default().fg(theme::accent())));
    } else {
        spans.push(RSpan::raw(" "));
    }
    spans.push(RSpan::raw("  ".repeat(row.depth)));
    // Only top-level section headers show a fold chevron; file and hunk rows
    // stay clean and rely on indentation.
    if row.foldable && row.depth == 0 {
        // Unicode chevrons where allowed, ASCII carets otherwise.
        let marker = if row.folded {
            rgit_model::glyph("▸ ", "> ")
        } else {
            rgit_model::glyph("▾ ", "v ")
        };
        spans.push(RSpan::styled(marker, theme::resolve(Style::Dim)));
    }
    spans
}

fn span_style(span: &Span) -> RStyle {
    let mut style = theme::resolve(span.style);
    if let Some(bg) = span.bg {
        style = style.bg(theme::bg_color(bg));
    }
    style
}

fn render_row<'a>(row: &Row<'a>, hl: Hl, width: u16) -> Line<'a> {
    // Slow path only when a caret or a partial range needs per-cell styling;
    // the common cursor/selection/plain rows take the cheap whole-line path.
    if hl.caret.is_some() || matches!(hl.sel, Sel::Range(..)) {
        return render_row_cells(row, hl, width);
    }

    let mut spans = row_prefix(row, hl.cursorline);
    for span in row.spans {
        spans.push(RSpan::styled(span.text.as_str(), span_style(span)));
    }
    let mut line = Line::from(spans);
    // An added/removed diff line carries a background wash that fills the row.
    let diff_bg = row_diff_bg(row);
    let highlighted = hl.cursorline || matches!(hl.sel, Sel::Full) || diff_bg.is_some();
    if highlighted {
        // Pad so the background spans the full width even on short rows.
        let used = line.width();
        if (width as usize) > used {
            line.push_span(RSpan::raw(" ".repeat(width as usize - used)));
        }
    }
    if hl.cursorline {
        line.style(
            RStyle::default()
                .bg(theme::cursor_bg())
                .add_modifier(Modifier::BOLD),
        )
    } else if matches!(hl.sel, Sel::Full) {
        line.style(RStyle::default().bg(theme::select_bg()))
    } else if let Some(bg) = diff_bg {
        line.style(RStyle::default().bg(bg))
    } else {
        line
    }
}

/// The full-row background for an added/removed diff line, taken from its washed
/// gutter; `None` for any other row.
fn row_diff_bg(row: &Row) -> Option<Color> {
    match row.spans.first().and_then(|s| s.bg) {
        Some(bg @ (Style::AddedBg | Style::DeletedBg)) => Some(theme::bg_color(bg)),
        _ => None,
    }
}

/// Per-cell rendering for rows carrying a block caret or a partial selection.
fn render_row_cells<'a>(row: &Row<'a>, hl: Hl, width: u16) -> Line<'a> {
    // Expand the content to (char, style) cells so a caret or range can override
    // individual columns; the cursorline/full-select tint is the line's base.
    let mut cells: Vec<(char, RStyle)> = Vec::new();
    for span in row.spans {
        let style = span_style(span);
        for ch in span.text.chars() {
            cells.push((ch, style));
        }
    }
    let len = cells.len();
    if let Sel::Range(lo, hi) = hl.sel {
        let hi = hi.min(len.saturating_sub(1));
        for cell in &mut cells[lo.min(len)..(hi + 1).min(len)] {
            cell.1 = cell.1.bg(theme::select_bg());
        }
    }
    let caret_past_end = matches!(hl.caret, Some(c) if c >= len);
    if let Some(c) = hl.caret
        && c < len
    {
        cells[c].1 = cells[c].1.add_modifier(Modifier::REVERSED);
    }

    // Coalesce equal-styled runs back into spans to keep the line compact.
    let mut spans = row_prefix(row, hl.cursorline);
    let mut i = 0;
    while i < cells.len() {
        let style = cells[i].1;
        let mut text = String::new();
        while i < cells.len() && cells[i].1 == style {
            text.push(cells[i].0);
            i += 1;
        }
        spans.push(RSpan::styled(text, style));
    }
    // A caret resting past the last char shows as a reversed cell in the pad.
    if caret_past_end {
        spans.push(RSpan::styled(
            " ",
            RStyle::default().add_modifier(Modifier::REVERSED),
        ));
    }

    let mut line = Line::from(spans);
    let used = line.width();
    if (width as usize) > used {
        line.push_span(RSpan::raw(" ".repeat(width as usize - used)));
    }
    let base = if hl.cursorline {
        RStyle::default()
            .bg(theme::cursor_bg())
            .add_modifier(Modifier::BOLD)
    } else if matches!(hl.sel, Sel::Full) {
        RStyle::default().bg(theme::select_bg())
    } else {
        RStyle::default()
    };
    line.style(base)
}

/// The bottom bar: the item under the cursor and the verbs that apply to it,
/// or a pending confirmation / error when one is active.
fn render_action_bar(frame: &mut Frame, app: &App, area: Rect) {
    if let Some(query) = &app.search {
        let line = Line::from(vec![
            RSpan::styled("/", RStyle::default().fg(theme::accent())),
            RSpan::raw(query.clone()),
            RSpan::styled("▏", RStyle::default().fg(theme::accent())),
            RSpan::styled("   n/N next/prev", theme::resolve(Style::Dim)),
        ]);
        frame.render_widget(Paragraph::new(line), area);
        return;
    }
    if let Some(err) = &app.error {
        let line = Line::from(RSpan::styled(
            format!(" ! {err} "),
            theme::resolve(Style::Deleted).add_modifier(Modifier::BOLD),
        ));
        frame.render_widget(Paragraph::new(line), area);
        return;
    }

    // A live visual selection shows a vim-style mode indicator.
    if app.active_buffer().has_selection() {
        let mode = if app.active_buffer().is_char_visual() {
            " -- VISUAL --"
        } else {
            " -- VISUAL LINE --"
        };
        // Staging keys go through the leader in vim; yank (to clipboard) is a
        // vim-profile action, so magit only advertises staging.
        let keys = if crate::keymap::profile() == crate::keymap::Profile::Vim {
            "  y yank · ␣s/␣u stage · esc cancel"
        } else {
            "  s/u stage · esc cancel"
        };
        let line = Line::from(vec![
            RSpan::styled(
                mode,
                RStyle::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD),
            ),
            RSpan::styled(keys, theme::resolve(Style::Dim)),
        ]);
        frame.render_widget(Paragraph::new(line), area);
        return;
    }

    let (label, hints) = context_hints(app);
    let mut spans = Vec::new();
    if let Some(label) = label {
        spans.push(RSpan::styled(
            format!("{label}  "),
            RStyle::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ));
    }
    // The session-log view's keys (f/g) are bare in both profiles, so don't
    // leader-prefix them.
    let vim = crate::keymap::profile() == crate::keymap::Profile::Vim
        && app.active_kind() != ViewKind::SessionLog;
    for (i, (key, desc)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(RSpan::raw("  "));
        }
        spans.push(RSpan::styled(
            leader_hint(vim, key),
            RStyle::default().fg(theme::accent()),
        ));
        spans.push(RSpan::styled(
            format!(" {desc}"),
            theme::resolve(Style::Dim),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);

    // The preview focus state is dynamic, so it rides the right edge while the
    // static command hints keep the left.
    if app.preview_visible {
        let (word, style) = if app.preview_focus {
            (
                "preview  j/k move · ⇥ fold hunk · ← back",
                RStyle::default().fg(theme::accent()),
            )
        } else {
            ("→ enter preview", theme::resolve(Style::Dim))
        };
        frame.render_widget(
            Paragraph::new(Line::from(RSpan::styled(format!("{word} "), style)))
                .alignment(Alignment::Right),
            area,
        );
    }
}

/// Git actions whose mnemonic collides with a vim motion (or visual/yank), so in
/// the vim profile they are reachable only through the Space leader: log (l),
/// branch (b), refs (y), refresh (g), and revert (V, since bare V line-selects).
/// Every other action letter is bound bare in both profiles, so its hint stays
/// bare; only these get the leader prefix.
const VIM_LEADER_ONLY: &[char] = &['l', 'b', 'y', 'g', 'V'];

/// The chord for a single key char in the vim profile: a `␣`-prefixed leader
/// chord for a motion-clashing git action, or the bare char otherwise.
fn key_chord(c: char) -> String {
    if VIM_LEADER_ONLY.contains(&c) {
        format!("␣{c}")
    } else {
        c.to_string()
    }
}

/// The key to show for a hint under the active profile. In the vim profile a
/// git action lives behind the Space leader, so single-letter action keys are
/// shown as `␣`-prefixed chords; motion and structural keys stay bare. A
/// combined `x / y` label (only when every token is a single char) has each
/// token converted, so `s / u` reads `␣s / ␣u` while `j / k · ^n / ^p` (mixed
/// tokens) is left alone.
fn leader_hint(vim: bool, key: &str) -> String {
    if !vim {
        return key.to_owned();
    }
    if key.contains(" / ") {
        let parts: Vec<&str> = key.split(" / ").collect();
        if parts.iter().all(|p| p.chars().count() == 1) {
            return parts
                .iter()
                .map(|p| key_chord(p.chars().next().unwrap()))
                .collect::<Vec<_>>()
                .join(" / ");
        }
        return key.to_owned();
    }
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => key_chord(c),
        _ => key.to_owned(),
    }
}

/// Context label and key hints for the item under the cursor, per view.
fn context_hints(app: &App) -> (Option<String>, Vec<(&'static str, &'static str)>) {
    let target = app.buffer().target_at_cursor();
    match app.active_kind() {
        ViewKind::Status => match target {
            Some(Target::File {
                path,
                staged: false,
            }) => (
                Some(path),
                vec![
                    ("s", "stage"),
                    ("x", "discard"),
                    ("⏎", "diff"),
                    ("⇥", "expand"),
                ],
            ),
            Some(Target::File { path, staged: true }) => {
                (Some(path), vec![("u", "unstage"), ("⏎", "diff")])
            }
            Some(Target::Hunk {
                staged: Some(false),
                ..
            }) => (
                Some("hunk".into()),
                vec![
                    ("s", "stage"),
                    ("v", "lines"),
                    ("⏎", "editor"),
                    ("x", "discard"),
                ],
            ),
            Some(Target::Hunk {
                staged: Some(true), ..
            }) => (Some("hunk".into()), vec![("u", "unstage"), ("⏎", "editor")]),
            // A read-only diff hunk (commit / diff view): open the file at the line.
            Some(Target::Hunk { staged: None, .. }) => (Some("hunk".into()), vec![("⏎", "editor")]),
            Some(Target::Stash { .. }) => (
                Some("stash".into()),
                vec![("p", "pop"), ("z", "menu"), ("x", "drop")],
            ),
            Some(Target::Commit { .. }) => {
                (Some("commit".into()), vec![("⏎", "view"), ("d", "diff")])
            }
            Some(Target::Ref { .. }) => {
                (Some("ref".into()), vec![("⏎", "checkout"), ("x", "delete")])
            }
            Some(Target::Worktree { .. }) => {
                (Some("worktree".into()), vec![("⏎", "inspect changes")])
            }
            _ => (
                None,
                vec![
                    ("s", "stage"),
                    ("c", "commit"),
                    ("l", "log"),
                    (":", "palette"),
                    ("?", "help"),
                ],
            ),
        },
        ViewKind::Log => (
            None,
            vec![("⏎", "view"), ("d", "diff"), ("/", "filter"), ("q", "back")],
        ),
        ViewKind::SessionLog => (
            Some(format!("session log · {}", app.log_filter_label())),
            vec![
                ("f", "filter"),
                ("g", "refresh"),
                ("/", "search"),
                ("q", "close"),
            ],
        ),
        ViewKind::Refs => (
            Some("ref".into()),
            vec![("⏎", "checkout"), ("x", "delete"), ("q", "back")],
        ),
        ViewKind::Commit | ViewKind::Diff => (None, vec![("⇥", "fold"), ("q", "back")]),
        ViewKind::Smartlog => (Some("smartlog".into()), vec![("q", "back")]),
        ViewKind::Oplog => (
            Some("op-log".into()),
            vec![("⏎", "restore here"), ("q", "back")],
        ),
        ViewKind::Stack => (Some("stack".into()), vec![("q", "back")]),
        ViewKind::Lanes => (
            Some("lanes".into()),
            vec![
                ("n", "new lane"),
                ("s", "stack on lane"),
                ("S", "restack lanes"),
                ("a", "assign file"),
                ("u", "unassign"),
                ("c", "commit lane"),
                ("R", "rename"),
                ("d", "delete lane"),
                ("p", "push"),
                ("P", "pull request"),
                ("q", "back"),
            ],
        ),
        ViewKind::Info => (None, vec![("q", "back")]),
        ViewKind::Blame => (None, vec![("^o", "editor"), ("q", "back")]),
        ViewKind::Remotes | ViewKind::Worktrees | ViewKind::Forge | ViewKind::Review => {
            (None, vec![("q", "back")])
        }
    }
}

/// Grouped keybinding reference, shown full-screen while `app.help` is set.
const HELP_GROUPS: &[(&str, &[(&str, &str)])] = &[
    (
        "Navigation",
        &[
            ("j / k · ^n / ^p", "move down / up"),
            ("^f / ^b", "forward / back a character"),
            ("^a / ^e", "start / end of line"),
            ("M-f / M-b", "forward / back a word"),
            ("^Spc / v", "set mark (char / line selection)"),
            ("M-w · ^w", "copy the selection (vim: y)"),
            ("^x o · arrows", "switch nav / preview pane"),
            ("Tab", "fold / unfold section"),
            ("RET", "open commit · blame file · checkout ref"),
            ("v", "select line(s) for staging"),
            ("q", "back a screen · quit at root"),
            ("/", "search · n / N next / prev"),
            (":", "command palette"),
            ("g", "refresh"),
        ],
    ),
    (
        "Staging",
        &[
            ("s / u", "stage / unstage at point"),
            ("S / U", "stage / unstage everything"),
            ("x", "discard · delete branch/tag at point"),
        ],
    ),
    (
        "Commit & history",
        &[
            ("c", "commit menu (a amend · e extend)"),
            ("l", "log menu (--all · filter by author)"),
            ("L", "session log (in-app diagnostics)"),
            ("d", "diff two revisions"),
            ("y", "refs (branches, remotes, tags)"),
            ("A / V", "cherry-pick / revert a commit"),
        ],
    ),
    (
        "Branch & stash",
        &[
            ("b", "branch menu (checkout · create · rename · delete)"),
            ("z", "stash menu (save · pop · apply · drop)"),
            ("p", "pop the stash under the cursor"),
            ("t", "tag menu (create · delete)"),
        ],
    ),
    (
        "Sync & power tools",
        &[
            ("f / F", "fetch / pull"),
            ("P", "push menu (--force-with-lease, …)"),
            ("M", "remote menu (list · add · remove)"),
            ("r", "rebase menu (onto · interactive · continue)"),
            ("m", "merge a branch"),
            ("O", "reset (soft · mixed · hard)"),
        ],
    ),
    (
        "Operations menu (o · space o)",
        &[
            ("u / r", "undo / redo the last operation"),
            ("o", "op-log timeline · RET restores to that point"),
            ("s", "smartlog (draft commits vs the trunk)"),
            ("k / n / R", "stack: view · new branch · restack"),
            ("a", "absorb changes into the owning commits"),
            ("f", "workflow status (flow init/start/finish: palette)"),
            ("w", "copy-on-write workspaces (new: palette)"),
        ],
    ),
];

/// Build the key-legend lines for a set of help groups, with keycap-styled keys.
fn help_lines(groups: &[(&str, &[(&str, &str)])]) -> Vec<Line<'static>> {
    let keycap = keycap_style();
    let dim = theme::resolve(Style::Dim);
    // In the vim profile the single-letter git actions live behind the leader,
    // so show them as `␣`-chords here too instead of the bare magit keys.
    let vim = crate::keymap::profile() == crate::keymap::Profile::Vim;
    let mut lines = Vec::new();
    for (title, keys) in groups {
        lines.push(Line::from(RSpan::styled(
            format!(" {}", title.to_uppercase()),
            pane_label_style(),
        )));
        for (key, desc) in *keys {
            lines.push(Line::from(vec![
                RSpan::raw("  "),
                RSpan::styled(format!(" {} ", leader_hint(vim, key)), keycap),
                RSpan::styled(format!("  {desc}"), dim),
            ]));
        }
        lines.push(Line::default());
    }
    lines
}

fn render_help(frame: &mut Frame, area: Rect) {
    // Paint a solid ground and a rounded frame titled with the app identity.
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(RStyle::default().fg(theme::accent()))
        .padding(Padding::horizontal(2))
        .title_top(Line::from(RSpan::styled(
            " ◆ rgit · keybindings ",
            RStyle::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        )))
        .title_bottom(
            Line::from(RSpan::styled(
                " press any key to close ",
                theme::resolve(Style::Dim),
            ))
            .centered(),
        )
        .style(RStyle::default().bg(theme::base_bg()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Two columns so the whole legend fits without scrolling.
    let mid = HELP_GROUPS.len().div_ceil(2);
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(inner);
    frame.render_widget(Paragraph::new(help_lines(&HELP_GROUPS[..mid])), left);
    frame.render_widget(Paragraph::new(help_lines(&HELP_GROUPS[mid..])), right);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier};

    /// The spans of a parsed line as (text, style) pairs for easy assertions.
    fn spans(line: &str) -> Vec<(String, RStyle)> {
        ansi_line(line)
            .spans
            .iter()
            .map(|s| (s.content.to_string(), s.style))
            .collect()
    }

    #[test]
    fn plain_line_is_a_single_default_span() {
        assert_eq!(
            spans("just text"),
            vec![("just text".to_owned(), RStyle::default())]
        );
    }

    #[test]
    fn sgr_color_runs_and_reset() {
        assert_eq!(
            spans("\x1b[31mred\x1b[0m plain"),
            vec![
                ("red".to_owned(), RStyle::default().fg(Color::Red)),
                (" plain".to_owned(), RStyle::default()),
            ]
        );
    }

    #[test]
    fn sgr_bold_combines_with_color() {
        assert_eq!(
            spans("\x1b[1;34mbold blue\x1b[m"),
            vec![(
                "bold blue".to_owned(),
                RStyle::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD)
            )]
        );
    }

    #[test]
    fn sgr_extended_colors() {
        assert_eq!(
            spans("\x1b[38;5;196mx"),
            vec![("x".to_owned(), RStyle::default().fg(Color::Indexed(196)))]
        );
        assert_eq!(
            spans("\x1b[48;2;1;2;3m y"),
            vec![(" y".to_owned(), RStyle::default().bg(Color::Rgb(1, 2, 3)))]
        );
    }

    #[test]
    fn sgr_bright_and_reset_to_default() {
        assert_eq!(
            spans("\x1b[92mbright\x1b[39m done"),
            vec![
                ("bright".to_owned(), RStyle::default().fg(Color::LightGreen)),
                (" done".to_owned(), RStyle::default()),
            ]
        );
    }

    #[test]
    fn non_sgr_escapes_are_dropped() {
        // Cursor erase and a stray ESC vanish; the text stays.
        assert_eq!(
            spans("a\x1b[2Kb\x1b c"),
            vec![("ab c".to_owned(), RStyle::default())]
        );
    }

    #[test]
    fn trailing_escape_without_text_adds_nothing() {
        assert_eq!(
            spans("x\x1b[31m"),
            vec![("x".to_owned(), RStyle::default())]
        );
    }
}
