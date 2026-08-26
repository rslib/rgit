//! Our own terminal prompt widgets: a fuzzy select, a fuzzy multiselect, a text
//! input, a yes/no confirm, and a streaming spinner. Rendered in rgit's house
//! style (a teal marker and left rail, amber match highlights, a green/red
//! collapsed summary), driven directly by crossterm so navigation keys such as
//! Ctrl-P/Ctrl-N are unambiguous. Prompts are shown only on a real terminal;
//! the callers gate on that before reaching here.

use std::io::{Write, stderr};
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};
use std::thread::JoinHandle;

use crossterm::cursor::{Hide, MoveToColumn, MoveUp, Show};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, read};
use crossterm::style::{
    Attribute, Color, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
};
use crossterm::terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode, size};
use crossterm::{ExecutableCommand, QueueableCommand};

mod fuzzy;

/// The user aborted the prompt (Esc or Ctrl-C).
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Why a rich prompt did not complete: the user aborted, or the terminal cannot
/// drive raw-mode/ANSI rendering so the caller should fall back to line mode.
enum Fail {
    Cancel,
    Unsupported,
}

/// Whether the terminal can render the rich (raw-mode, ANSI) widgets. A `dumb`
/// or unset `TERM` gets the plain line-mode prompts instead. Resolved once.
fn capable() -> bool {
    static CAPABLE: OnceLock<bool> = OnceLock::new();
    *CAPABLE.get_or_init(|| matches!(std::env::var("TERM"), Ok(t) if !t.is_empty() && t != "dumb"))
}

/// Read one line from stdin for line-mode prompts; `None` on EOF or error.
fn read_line() -> Option<String> {
    let mut s = String::new();
    match std::io::stdin().read_line(&mut s) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(s.trim_end_matches(['\n', '\r']).to_owned()),
    }
}

/// One choice in a select/multiselect: an opaque value plus the text shown and
/// filtered against.
pub struct Item<T> {
    pub value: T,
    pub label: String,
}

impl<T> Item<T> {
    pub fn new(value: T, label: impl Into<String>) -> Self {
        Self {
            value,
            label: label.into(),
        }
    }
}

const ACCENT: Color = Color::Rgb {
    r: 45,
    g: 212,
    b: 191,
};
const RAIL: Color = Color::Rgb {
    r: 22,
    g: 110,
    b: 101,
};
const SUCCESS: Color = Color::Rgb {
    r: 74,
    g: 222,
    b: 128,
};
const ERROR: Color = Color::Rgb {
    r: 248,
    g: 113,
    b: 113,
};
const MUTED: Color = Color::Rgb {
    r: 118,
    g: 126,
    b: 138,
};
const MATCH: Color = Color::Rgb {
    r: 251,
    g: 191,
    b: 36,
};

/// The glyphs a prompt draws, chosen once from the locale (Unicode unless the
/// terminal looks non-UTF, or `RGIT_ASCII` is set).
struct Glyphs {
    marker: &'static str,
    done: &'static str,
    cancel: &'static str,
    rail: &'static str,
    pointer: &'static str,
    on: &'static str,
    off: &'static str,
    check_on: &'static str,
    check_off: &'static str,
    more_up: &'static str,
    more_down: &'static str,
    spinner: &'static [&'static str],
}

const UNICODE: Glyphs = Glyphs {
    marker: "\u{25c6}",
    done: "\u{25c7}",
    cancel: "\u{25a0}",
    rail: "\u{2502}",
    pointer: "\u{276f}",
    on: "\u{25cf}",
    off: "\u{25cb}",
    check_on: "\u{25fc}",
    check_off: "\u{25fb}",
    more_up: "\u{2191}",
    more_down: "\u{2193}",
    spinner: &[
        "\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283c}", "\u{2834}", "\u{2826}",
        "\u{2827}", "\u{2807}", "\u{280f}",
    ],
};

const ASCII: Glyphs = Glyphs {
    marker: "*",
    done: "o",
    cancel: "x",
    rail: "|",
    pointer: ">",
    on: "(*)",
    off: "( )",
    check_on: "[x]",
    check_off: "[ ]",
    more_up: "^",
    more_down: "v",
    spinner: &["|", "/", "-", "\\"],
};

fn glyphs() -> &'static Glyphs {
    static G: OnceLock<&'static Glyphs> = OnceLock::new();
    G.get_or_init(|| {
        if std::env::var_os("RGIT_ASCII").is_some() {
            return &ASCII;
        }
        let utf = ["LC_ALL", "LC_CTYPE", "LANG"].iter().any(|k| {
            std::env::var(k)
                .map(|v| v.to_uppercase().contains("UTF"))
                .unwrap_or(false)
        });
        if utf { &UNICODE } else { &ASCII }
    })
}

#[derive(Clone, Copy, Default)]
struct Style {
    fg: Option<Color>,
    bg: Option<Color>,
    bold: bool,
    dim: bool,
}

impl Style {
    fn fg(color: Color) -> Self {
        Self {
            fg: Some(color),
            ..Self::default()
        }
    }
    fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
    fn dim(mut self) -> Self {
        self.dim = true;
        self
    }
}

struct Span {
    text: String,
    style: Style,
}

fn span(text: impl Into<String>, style: Style) -> Span {
    Span {
        text: text.into(),
        style,
    }
}

type Line = Vec<Span>;

/// Draws a re-rendered block of lines in place (moving up over the previous
/// frame and clearing), truncating each line to the terminal width so nothing
/// wraps and breaks the line math. Restores raw mode and the cursor on drop.
struct Screen {
    drawn: usize,
    width: usize,
}

impl Screen {
    fn new() -> std::io::Result<Self> {
        enable_raw_mode()?;
        stderr().execute(Hide)?;
        let width = size().map(|(w, _)| w as usize).unwrap_or(80).max(20);
        Ok(Self { drawn: 0, width })
    }

    fn render(&mut self, lines: &[Line]) -> std::io::Result<()> {
        let mut out = stderr();
        if self.drawn > 0 {
            out.queue(MoveToColumn(0))?;
            if self.drawn > 1 {
                out.queue(MoveUp((self.drawn - 1) as u16))?;
            }
            out.queue(Clear(ClearType::FromCursorDown))?;
        }
        for (i, line) in lines.iter().enumerate() {
            self.render_line(&mut out, line)?;
            if i + 1 < lines.len() {
                out.write_all(b"\r\n")?;
            }
        }
        out.flush()?;
        self.drawn = lines.len();
        Ok(())
    }

    fn render_line(&self, out: &mut impl Write, line: &Line) -> std::io::Result<()> {
        let mut remaining = self.width;
        for s in line {
            if remaining == 0 {
                break;
            }
            let text: String = if s.text.chars().count() > remaining {
                s.text.chars().take(remaining).collect()
            } else {
                s.text.clone()
            };
            remaining -= text.chars().count();
            if let Some(fg) = s.style.fg {
                out.queue(SetForegroundColor(fg))?;
            }
            if let Some(bg) = s.style.bg {
                out.queue(SetBackgroundColor(bg))?;
            }
            if s.style.bold {
                out.queue(SetAttribute(Attribute::Bold))?;
            }
            if s.style.dim {
                out.queue(SetAttribute(Attribute::Dim))?;
            }
            out.write_all(text.as_bytes())?;
            out.queue(SetAttribute(Attribute::Reset))?;
            out.queue(ResetColor)?;
        }
        Ok(())
    }

    /// Replace the interactive frame with a single collapsed summary line, then
    /// leave the cursor on a fresh line below it.
    fn finish(&mut self, header: &str, marker: &str, marker_color: Color, value: Span) {
        let line = vec![
            span(format!("{marker}  "), Style::fg(marker_color)),
            span(format!("{header}  "), Style::default().dim()),
            value,
        ];
        let _ = self.render(&[line]);
        let mut out = stderr();
        let _ = out.write_all(b"\r\n");
        let _ = out.flush();
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = stderr().execute(Show);
        let _ = disable_raw_mode();
    }
}

enum Nav {
    Up,
    Down,
    Enter,
    Toggle,
    Cancel,
    Char(char),
    Backspace,
    Other,
}

fn next_nav() -> std::io::Result<Nav> {
    loop {
        let Event::Key(k) = read()? else {
            continue;
        };
        if k.kind == KeyEventKind::Release {
            continue;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let nav = match k.code {
            KeyCode::Up => Nav::Up,
            KeyCode::Down => Nav::Down,
            KeyCode::Enter => Nav::Enter,
            KeyCode::Tab => Nav::Toggle,
            KeyCode::Esc => Nav::Cancel,
            KeyCode::Backspace => Nav::Backspace,
            KeyCode::Char('c') if ctrl => Nav::Cancel,
            KeyCode::Char('p' | 'k') if ctrl => Nav::Up,
            KeyCode::Char('n' | 'j') if ctrl => Nav::Down,
            KeyCode::Char(c) if !ctrl => Nav::Char(c),
            _ => Nav::Other,
        };
        return Ok(nav);
    }
}

fn header_line(marker: &str, color: Color, prompt: &str) -> Line {
    vec![
        span(format!("{marker}  "), Style::fg(color).bold()),
        span(prompt.to_owned(), Style::default().bold()),
    ]
}

fn rail(spans: Vec<Span>) -> Line {
    let g = glyphs();
    let mut line = vec![span(format!("{}  ", g.rail), Style::fg(RAIL))];
    line.extend(spans);
    line
}

/// Split `label` into styled spans, highlighting the matched character
/// positions in amber over `base`.
fn highlight(label: &str, positions: &[usize], base: Style) -> Vec<Span> {
    if positions.is_empty() {
        return vec![span(label.to_owned(), base)];
    }
    let hl = {
        let mut s = Style::fg(MATCH).bold();
        s.dim = base.dim;
        s
    };
    let mut spans = Vec::new();
    let mut buf = String::new();
    let mut buf_hot = false;
    for (i, ch) in label.chars().enumerate() {
        let hot = positions.contains(&i);
        if hot != buf_hot && !buf.is_empty() {
            spans.push(span(
                std::mem::take(&mut buf),
                if buf_hot { hl } else { base },
            ));
        }
        buf_hot = hot;
        buf.push(ch);
    }
    if !buf.is_empty() {
        spans.push(span(buf, if buf_hot { hl } else { base }));
    }
    spans
}

const PAGE: usize = 10;

/// Fuzzy single-select. Returns the chosen value, or `Cancelled`.
pub fn select<T>(prompt: &str, mut items: Vec<Item<T>>) -> Result<T, Cancelled> {
    let picked = list_indices(prompt, &items, false)?;
    let idx = picked.into_iter().next().ok_or(Cancelled)?;
    Ok(items.swap_remove(idx).value)
}

/// Fuzzy multiselect (Tab toggles, Enter confirms). Returns chosen values in
/// list order, or `Cancelled`.
pub fn multiselect<T>(prompt: &str, items: Vec<Item<T>>) -> Result<Vec<T>, Cancelled> {
    let mut chosen = list_indices(prompt, &items, true)?;
    chosen.sort_unstable();
    let mut out = Vec::with_capacity(chosen.len());
    for (i, item) in items.into_iter().enumerate() {
        if chosen.binary_search(&i).is_ok() {
            out.push(item.value);
        }
    }
    Ok(out)
}

/// Pick indices with the rich widget when the terminal supports it, else fall
/// back to the line-mode prompt.
fn list_indices<T>(prompt: &str, items: &[Item<T>], multi: bool) -> Result<Vec<usize>, Cancelled> {
    if capable() {
        match run_list(prompt, items, multi) {
            Ok(v) => return Ok(v),
            Err(Fail::Cancel) => return Err(Cancelled),
            Err(Fail::Unsupported) => {}
        }
    }
    list_line(prompt, items, multi)
}

/// Plain numbered-list prompt for dumb terminals: no raw mode, no ANSI. Accepts
/// a number, or text that is fuzzy-matched against the labels. For multiselect,
/// several space/comma-separated numbers (empty line selects nothing).
fn list_line<T>(prompt: &str, items: &[Item<T>], multi: bool) -> Result<Vec<usize>, Cancelled> {
    let mut err = stderr();
    let _ = writeln!(err, "{prompt}");
    for (i, item) in items.iter().enumerate() {
        let _ = writeln!(err, "  {}) {}", i + 1, item.label);
    }
    loop {
        if multi {
            let _ = write!(err, "numbers (1-{}, space separated), or enter for none: ", items.len());
        } else {
            let _ = write!(err, "number (1-{}) or text: ", items.len());
        }
        let _ = err.flush();
        let line = read_line().ok_or(Cancelled)?;
        let line = line.trim();
        if multi {
            if line.is_empty() {
                return Ok(Vec::new());
            }
            let mut picked = Vec::new();
            let mut bad = false;
            for tok in line.split([' ', ',']).filter(|t| !t.is_empty()) {
                match tok.parse::<usize>() {
                    Ok(n) if (1..=items.len()).contains(&n) => picked.push(n - 1),
                    _ => bad = true,
                }
            }
            if bad {
                let _ = writeln!(err, "invalid selection");
                continue;
            }
            return Ok(picked);
        }
        if line.is_empty() {
            continue;
        }
        if let Ok(n) = line.parse::<usize>() {
            if (1..=items.len()).contains(&n) {
                return Ok(vec![n - 1]);
            }
            let _ = writeln!(err, "out of range");
            continue;
        }
        if let Some(top) = fuzzy::filter(line, items).first() {
            return Ok(vec![top.index]);
        }
        let _ = writeln!(err, "no match");
    }
}

/// The shared list engine. Returns the selected original indices (one for
/// select, zero-or-more for multiselect).
fn run_list<T>(prompt: &str, items: &[Item<T>], multi: bool) -> Result<Vec<usize>, Fail> {
    let g = glyphs();
    let mut screen = Screen::new().map_err(|_| Fail::Unsupported)?;
    let mut query = String::new();
    let mut cursor = 0usize;
    let mut offset = 0usize;
    let mut selected = vec![false; items.len()];

    loop {
        let matches = fuzzy::filter(&query, items);
        if cursor >= matches.len() {
            cursor = matches.len().saturating_sub(1);
        }
        if cursor < offset {
            offset = cursor;
        } else if cursor >= offset + PAGE {
            offset = cursor + 1 - PAGE;
        }

        let mut lines = vec![header_line(g.marker, ACCENT, prompt)];
        let count = span(
            format!("  {}/{}", matches.len(), items.len()),
            Style::fg(MUTED),
        );
        lines.push(rail(vec![
            span(format!("{} ", g.pointer), Style::fg(ACCENT)),
            span(query.clone(), Style::default()),
            span("\u{2589}", Style::fg(ACCENT)),
            count,
        ]));

        if matches.is_empty() {
            lines.push(rail(vec![span("no matches", Style::fg(MUTED).dim())]));
        }
        if offset > 0 {
            lines.push(rail(vec![span(
                format!("{} {} more", g.more_up, offset),
                Style::fg(MUTED).dim(),
            )]));
        }
        for (row, m) in matches.iter().enumerate().skip(offset).take(PAGE) {
            let item = &items[m.index];
            let active = row == cursor;
            let mark = if multi {
                if selected[m.index] {
                    g.check_on
                } else {
                    g.check_off
                }
            } else if active {
                g.on
            } else {
                g.off
            };
            let mark_color = if multi && selected[m.index] {
                SUCCESS
            } else if active {
                ACCENT
            } else {
                MUTED
            };
            let base = if active {
                Style::default().bold()
            } else {
                Style::default().dim()
            };
            let pointer = if active { g.pointer } else { " " };
            let mut spans = vec![
                span(format!("{pointer} "), Style::fg(ACCENT)),
                span(format!("{mark} "), Style::fg(mark_color)),
            ];
            spans.extend(highlight(&item.label, &m.positions, base));
            lines.push(rail(spans));
        }
        let shown_end = (offset + PAGE).min(matches.len());
        if shown_end < matches.len() {
            lines.push(rail(vec![span(
                format!("{} {} more", g.more_down, matches.len() - shown_end),
                Style::fg(MUTED).dim(),
            )]));
        }

        let hint = if multi {
            "type to filter \u{00b7} \u{2191}\u{2193}/C-p C-n move \u{00b7} tab toggle \u{00b7} enter confirm \u{00b7} esc cancel"
        } else {
            "type to filter \u{00b7} \u{2191}\u{2193}/C-p C-n move \u{00b7} enter select \u{00b7} esc cancel"
        };
        lines.push(rail(vec![span(hint, Style::fg(MUTED).dim())]));

        if screen.render(&lines).is_err() {
            return Err(Fail::Unsupported);
        }

        match next_nav().map_err(|_| Fail::Unsupported)? {
            Nav::Cancel => {
                screen.finish(prompt, g.cancel, ERROR, span("cancelled", Style::fg(ERROR)));
                return Err(Fail::Cancel);
            }
            Nav::Up => cursor = cursor.saturating_sub(1),
            Nav::Down => {
                if cursor + 1 < matches.len() {
                    cursor += 1;
                }
            }
            Nav::Char(c) => {
                query.push(c);
                cursor = 0;
                offset = 0;
            }
            Nav::Backspace => {
                query.pop();
                cursor = 0;
                offset = 0;
            }
            Nav::Toggle if multi => {
                if let Some(m) = matches.get(cursor) {
                    selected[m.index] = !selected[m.index];
                }
            }
            Nav::Enter => {
                if multi {
                    let chosen: Vec<usize> = selected
                        .iter()
                        .enumerate()
                        .filter_map(|(i, &s)| s.then_some(i))
                        .collect();
                    let summary = match chosen.len() {
                        0 => "none".to_owned(),
                        1 => items[chosen[0]].label.clone(),
                        n => format!("{n} selected"),
                    };
                    screen.finish(prompt, g.done, SUCCESS, span(summary, Style::fg(SUCCESS)));
                    return Ok(chosen);
                }
                if let Some(m) = matches.get(cursor) {
                    let label = items[m.index].label.clone();
                    screen.finish(prompt, g.done, SUCCESS, span(label, Style::fg(SUCCESS)));
                    return Ok(vec![m.index]);
                }
            }
            _ => {}
        }
    }
}

/// Free-text input; `validate` rejects a value with a message shown in red.
pub fn input(
    prompt: &str,
    validate: impl Fn(&str) -> Result<(), String>,
) -> Result<String, Cancelled> {
    if capable() {
        match input_rich(prompt, &validate) {
            Ok(v) => return Ok(v),
            Err(Fail::Cancel) => return Err(Cancelled),
            Err(Fail::Unsupported) => {}
        }
    }
    input_line(prompt, &validate)
}

fn input_rich(prompt: &str, validate: &dyn Fn(&str) -> Result<(), String>) -> Result<String, Fail> {
    let g = glyphs();
    let mut screen = Screen::new().map_err(|_| Fail::Unsupported)?;
    let mut value = String::new();
    let mut error: Option<String> = None;

    loop {
        let mut lines = vec![header_line(g.marker, ACCENT, prompt)];
        lines.push(rail(vec![
            span(format!("{} ", g.pointer), Style::fg(ACCENT)),
            span(value.clone(), Style::default()),
            span("\u{2589}", Style::fg(ACCENT)),
        ]));
        if let Some(e) = &error {
            lines.push(rail(vec![span(e.clone(), Style::fg(ERROR))]));
        }
        if screen.render(&lines).is_err() {
            return Err(Fail::Unsupported);
        }

        match next_nav().map_err(|_| Fail::Unsupported)? {
            Nav::Cancel => {
                screen.finish(prompt, g.cancel, ERROR, span("cancelled", Style::fg(ERROR)));
                return Err(Fail::Cancel);
            }
            Nav::Char(c) => {
                value.push(c);
                error = None;
            }
            Nav::Backspace => {
                value.pop();
                error = None;
            }
            Nav::Enter => match validate(&value) {
                Ok(()) => {
                    screen.finish(prompt, g.done, SUCCESS, span(value.clone(), Style::fg(SUCCESS)));
                    return Ok(value);
                }
                Err(e) => error = Some(e),
            },
            _ => {}
        }
    }
}

fn input_line(
    prompt: &str,
    validate: &dyn Fn(&str) -> Result<(), String>,
) -> Result<String, Cancelled> {
    let mut err = stderr();
    loop {
        let _ = write!(err, "{prompt}: ");
        let _ = err.flush();
        let value = read_line().ok_or(Cancelled)?;
        match validate(&value) {
            Ok(()) => return Ok(value),
            Err(e) => {
                let _ = writeln!(err, "{e}");
            }
        }
    }
}

/// Yes/no confirm; `default` is the initially highlighted option.
pub fn confirm(prompt: &str, default: bool) -> Result<bool, Cancelled> {
    if capable() {
        match confirm_rich(prompt, default) {
            Ok(v) => return Ok(v),
            Err(Fail::Cancel) => return Err(Cancelled),
            Err(Fail::Unsupported) => {}
        }
    }
    confirm_line(prompt, default)
}

fn confirm_line(prompt: &str, default: bool) -> Result<bool, Cancelled> {
    let mut err = stderr();
    let hint = if default { "[Y/n]" } else { "[y/N]" };
    loop {
        let _ = write!(err, "{prompt} {hint}: ");
        let _ = err.flush();
        let line = read_line().ok_or(Cancelled)?;
        match line.trim() {
            "" => return Ok(default),
            "y" | "Y" | "yes" => return Ok(true),
            "n" | "N" | "no" => return Ok(false),
            _ => {}
        }
    }
}

fn confirm_rich(prompt: &str, default: bool) -> Result<bool, Fail> {
    let g = glyphs();
    let mut screen = Screen::new().map_err(|_| Fail::Unsupported)?;
    let mut yes = default;

    loop {
        let pill = |label: &str, on: bool| {
            if on {
                span(format!(" {label} "), Style::fg(ACCENT).bold())
            } else {
                span(format!(" {label} "), Style::fg(MUTED).dim())
            }
        };
        let lines = vec![
            header_line(g.marker, ACCENT, prompt),
            rail(vec![
                span(format!("{} ", g.pointer), Style::fg(ACCENT)),
                pill("Yes", yes),
                span(" ", Style::default()),
                pill("No", !yes),
            ]),
            rail(vec![span(
                "y/n \u{00b7} \u{2190}\u{2192}/tab toggle \u{00b7} enter confirm \u{00b7} esc cancel",
                Style::fg(MUTED).dim(),
            )]),
        ];
        if screen.render(&lines).is_err() {
            return Err(Fail::Unsupported);
        }

        match next_nav().map_err(|_| Fail::Unsupported)? {
            Nav::Cancel => {
                screen.finish(prompt, g.cancel, ERROR, span("cancelled", Style::fg(ERROR)));
                return Err(Fail::Cancel);
            }
            Nav::Char('y' | 'Y') => yes = true,
            Nav::Char('n' | 'N') => yes = false,
            Nav::Toggle | Nav::Up | Nav::Down => yes = !yes,
            Nav::Enter => {
                let label = if yes { "yes" } else { "no" };
                screen.finish(prompt, g.done, SUCCESS, span(label, Style::fg(SUCCESS)));
                return Ok(yes);
            }
            _ => {}
        }
    }
}

enum Tick {
    Message(String),
    Stop { ok: bool, message: String },
}

/// A braille spinner that shows a live message (fed the latest progress line of
/// a running op) and collapses to a green/red summary on stop.
pub struct Spinner {
    tx: Sender<Tick>,
    handle: Option<JoinHandle<()>>,
}

/// Start a spinner with an initial `title`. On a dumb terminal it degrades to
/// plain start/stop lines with no animation or escape sequences.
pub fn spinner(title: &str) -> Spinner {
    if !capable() {
        let mut out = stderr();
        let _ = writeln!(out, "{title}");
        let _ = out.flush();
        return Spinner {
            tx: channel().0,
            handle: None,
        };
    }
    let (tx, rx) = channel::<Tick>();
    let title = title.to_owned();
    let handle = std::thread::spawn(move || {
        use std::time::Duration;
        let g = glyphs();
        let mut frame = 0usize;
        let mut message = title;
        let mut out = stderr();
        loop {
            match rx.recv_timeout(Duration::from_millis(80)) {
                Ok(Tick::Message(m)) => message = m,
                Ok(Tick::Stop { ok, message }) => {
                    let (mark, color) = if ok {
                        (g.done, SUCCESS)
                    } else {
                        (g.cancel, ERROR)
                    };
                    let _ = out.write_all(b"\r");
                    let _ = out.execute(Clear(ClearType::CurrentLine));
                    let _ = out.execute(SetForegroundColor(color));
                    let _ = write!(out, "{mark}");
                    let _ = out.execute(ResetColor);
                    let _ = writeln!(out, "  {message}");
                    let _ = out.flush();
                    return;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return,
            }
            let glyph = g.spinner[frame % g.spinner.len()];
            frame += 1;
            let _ = out.write_all(b"\r");
            let _ = out.execute(Clear(ClearType::CurrentLine));
            let _ = out.execute(SetForegroundColor(ACCENT));
            let _ = write!(out, "{glyph}");
            let _ = out.execute(ResetColor);
            let _ = write!(out, "  {message}");
            let _ = out.flush();
        }
    });
    Spinner {
        tx,
        handle: Some(handle),
    }
}

impl Spinner {
    /// Replace the live message (e.g. with the latest git progress line).
    pub fn set_message(&self, message: impl Into<String>) {
        let _ = self.tx.send(Tick::Message(message.into()));
    }

    /// Stop with a green success summary.
    pub fn stop_ok(self, message: impl Into<String>) {
        self.stop(true, message.into());
    }

    /// Stop with a red failure summary.
    pub fn stop_err(self, message: impl Into<String>) {
        self.stop(false, message.into());
    }

    fn stop(mut self, ok: bool, message: String) {
        match self.handle.take() {
            Some(h) => {
                let _ = self.tx.send(Tick::Stop { ok, message });
                let _ = h.join();
            }
            None => {
                let mark = if ok { glyphs().done } else { glyphs().cancel };
                let mut out = stderr();
                let _ = writeln!(out, "{mark} {message}");
                let _ = out.flush();
            }
        }
    }
}
