//! Shared plumbing for every overlay box: placement and mouse resize, scroll
//! state, soft wrap with continuation marks and horizontal scroll for text
//! that does not wrap.

use std::cell::{Cell, RefCell};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style as RStyle};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use rgit_model::Style;
use unicode_width::UnicodeWidthChar;

use crate::theme;

/// Every box that keeps its own size, wrap mode or scroll position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxId {
    CommitEditor,
    Console,
    Confirm,
    Palette,
    RebaseTodo,
    SplitPicker,
    WhichKey,
    Help,
    Prompt,
    Finder,
    Search,
}

const BOX_COUNT: usize = 11;

impl BoxId {
    fn bit(self) -> u32 {
        1 << self as u32
    }

    /// The view pins itself to its target (the caret, or the tail of a
    /// stream) until the user scrolls away.
    fn follows(self) -> bool {
        matches!(self, BoxId::CommitEditor | BoxId::Console)
    }
}

/// How a box sits in its area, which decides how a border drag resizes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    Center,
    Bottom,
}

/// Which dimensions a border drag changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edges {
    pub width: bool,
    pub height: bool,
}

/// A scroll request for a box view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollBy {
    Rows(isize),
    Pages(isize),
    /// Display columns sideways, for text that does not wrap.
    Cols(isize),
    Top,
    Bottom,
}

/// Scroll state of one box. Rendering clamps it and records the content and
/// page height, so key handlers can page and detect the tail.
#[derive(Debug, Default)]
pub struct Viewport {
    /// First visible visual row.
    pub top: Cell<usize>,
    /// First visible display column, when the text does not wrap.
    pub left: Cell<usize>,
    pub follow: Cell<bool>,
    rows: Cell<usize>,
    page: Cell<usize>,
}

impl Viewport {
    fn reset(&self, follow: bool) {
        self.top.set(0);
        self.left.set(0);
        self.follow.set(follow);
    }

    fn max_top(&self) -> usize {
        self.rows.get().saturating_sub(self.page.get())
    }

    pub fn at_bottom(&self) -> bool {
        self.top.get() >= self.max_top()
    }

    pub fn scroll(&self, by: ScrollBy) {
        let page = self.page.get().max(1) as isize;
        let top = match by {
            ScrollBy::Rows(n) => self.top.get().saturating_add_signed(n),
            ScrollBy::Pages(n) => self.top.get().saturating_add_signed(n * page),
            ScrollBy::Top => 0,
            ScrollBy::Bottom => usize::MAX,
            ScrollBy::Cols(n) => {
                self.left.set(self.left.get().saturating_add_signed(n));
                return;
            }
        };
        self.top.set(top.min(self.max_top()));
        self.follow.set(false);
    }

    fn settle(&self, rows: usize, page: usize) {
        self.rows.set(rows);
        self.page.set(page);
        self.top.set(self.top.get().min(self.max_top()));
    }
}

#[derive(Debug, Default)]
struct BoxState {
    size: Option<(u16, u16)>,
    maximized: bool,
    nowrap: bool,
    view: Viewport,
}

#[derive(Debug, Clone, Copy)]
struct Placed {
    id: BoxId,
    rect: Rect,
    area: Rect,
    anchor: Anchor,
}

/// Size, wrap and scroll state for every box, plus where each box was drawn
/// in the last frame for mouse hit tests.
#[derive(Debug, Default)]
pub struct Boxes {
    states: [BoxState; BOX_COUNT],
    placed: RefCell<Vec<Placed>>,
    shown: Cell<u32>,
    shown_before: u32,
    drag: Option<(BoxId, Edges)>,
}

/// The smallest size a drag can shrink a box to.
const MIN_SIZE: (u16, u16) = (20, 5);

impl Boxes {
    /// Start a frame. A box that was not drawn in the previous frame opens
    /// with a fresh view.
    pub fn begin_frame(&mut self) {
        self.shown_before = self.shown.get();
        self.shown.set(0);
        self.placed.get_mut().clear();
    }

    /// Mark `id` as drawn this frame and return its view.
    pub fn show(&self, id: BoxId) -> &Viewport {
        let state = &self.states[id as usize];
        if self.shown_before & id.bit() == 0 && self.shown.get() & id.bit() == 0 {
            state.view.reset(id.follows());
        }
        self.shown.set(self.shown.get() | id.bit());
        &state.view
    }

    /// Give `id` a fresh view, for a box that replaces another of its kind
    /// without a frame between them.
    pub fn reset(&self, id: BoxId) {
        self.states[id as usize].view.reset(id.follows());
    }

    pub fn view(&self, id: BoxId) -> &Viewport {
        &self.states[id as usize].view
    }

    pub fn wrap(&self, id: BoxId) -> bool {
        !self.states[id as usize].nowrap
    }

    pub fn toggle_wrap(&mut self, id: BoxId) {
        let state = &mut self.states[id as usize];
        state.nowrap = !state.nowrap;
        state.view.left.set(0);
    }

    pub fn toggle_maximized(&mut self, id: BoxId) {
        let state = &mut self.states[id as usize];
        state.maximized = !state.maximized;
    }

    /// Place box `id` in `area` and record it for hit tests. `natural` is the
    /// size the content asks for; a drag or maximize overrides it.
    pub fn place(&self, id: BoxId, natural: (u16, u16), area: Rect, anchor: Anchor) -> Rect {
        self.show(id);
        let state = &self.states[id as usize];
        let (w, h) = if state.maximized {
            (area.width, area.height)
        } else {
            let (w, h) = state.size.unwrap_or(natural);
            (w.min(area.width), h.min(area.height))
        };
        let rect = match anchor {
            Anchor::Center => Rect {
                x: area.x + (area.width - w) / 2,
                y: area.y + (area.height - h) / 2,
                width: w,
                height: h,
            },
            Anchor::Bottom => Rect {
                x: area.x,
                y: area.bottom() - h,
                width: area.width,
                height: h,
            },
        };
        self.placed.borrow_mut().push(Placed {
            id,
            rect,
            area,
            anchor,
        });
        rect
    }

    /// The topmost box whose border is under the pointer, and the dimensions
    /// a drag from there changes.
    pub fn border_at(&self, col: u16, row: u16) -> Option<(BoxId, Edges)> {
        let placed = self.placed.borrow();
        let top = placed.last()?;
        let r = top.rect;
        if !contains(r, col, row) {
            return None;
        }
        let on_side = col == r.x || col + 1 == r.right();
        let on_end = row == r.y || row + 1 == r.bottom();
        let edges = match top.anchor {
            Anchor::Center => Edges {
                width: on_side,
                height: on_end,
            },
            Anchor::Bottom => Edges {
                width: false,
                height: row == r.y,
            },
        };
        (edges.width || edges.height).then_some((top.id, edges))
    }

    /// The topmost drawn box under the pointer.
    pub fn box_at(&self, col: u16, row: u16) -> Option<BoxId> {
        let placed = self.placed.borrow();
        let top = placed.last()?;
        contains(top.rect, col, row).then_some(top.id)
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Start a border drag when the pointer is on the topmost box border.
    pub fn press(&mut self, col: u16, row: u16) -> bool {
        self.drag = self.border_at(col, row);
        self.drag.is_some()
    }

    /// Resize the dragged box so its border follows the pointer.
    pub fn drag_to(&mut self, col: u16, row: u16) {
        let Some((id, edges)) = self.drag else { return };
        let Some(p) = self
            .placed
            .borrow()
            .iter()
            .rev()
            .find(|p| p.id == id)
            .copied()
        else {
            return;
        };
        let state = &mut self.states[id as usize];
        let (mut w, mut h) = (p.rect.width, p.rect.height);
        match p.anchor {
            Anchor::Center => {
                let cx = p.rect.x + p.rect.width / 2;
                let cy = p.rect.y + p.rect.height / 2;
                if edges.width {
                    w = col.abs_diff(cx) * 2 + 1;
                }
                if edges.height {
                    h = row.abs_diff(cy) * 2 + 1;
                }
            }
            Anchor::Bottom => {
                if edges.height {
                    h = p.area.bottom().saturating_sub(row);
                }
            }
        }
        state.size = Some((
            w.clamp(MIN_SIZE.0.min(p.area.width), p.area.width),
            h.clamp(MIN_SIZE.1.min(p.area.height), p.area.height),
        ));
        state.maximized = false;
    }

    pub fn release(&mut self) {
        self.drag = None;
    }
}

fn contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x && col < r.right() && row >= r.y && row < r.bottom()
}

fn char_width(c: char) -> usize {
    c.width().unwrap_or(0)
}

fn text_width(chars: &[char]) -> usize {
    chars.iter().map(|&c| char_width(c)).sum()
}

/// Split `chars` into rows of at most `width` display columns, breaking after
/// the last space where one exists. Returns char ranges; an empty line is one
/// empty row. A space at a break hangs on the row before it.
pub fn wrap_ranges(chars: &[char], width: usize) -> Vec<(usize, usize)> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut cols = 0;
        let mut end = start;
        let mut space_break = None;
        while end < chars.len() {
            let w = char_width(chars[end]);
            if cols + w > width && end > start {
                break;
            }
            cols += w;
            end += 1;
            if chars[end - 1].is_whitespace() {
                space_break = Some(end);
            }
        }
        if end < chars.len() {
            if chars[end].is_whitespace() {
                end += 1;
            } else if let Some(at) = space_break {
                end = at;
            }
        }
        out.push((start, end));
        start = end;
    }
    if out.is_empty() {
        out.push((0, 0));
    }
    out
}

/// The char range of `chars` that fits in display columns
/// `left..left + width`.
fn col_range(chars: &[char], left: usize, width: usize) -> (usize, usize) {
    let mut col = 0;
    let mut start = chars.len();
    let mut end = chars.len();
    for (i, &c) in chars.iter().enumerate() {
        let w = char_width(c);
        if start == chars.len() && col >= left {
            start = i;
        }
        if col + w > left + width {
            end = i;
            break;
        }
        col += w;
    }
    (start.min(end), end)
}

fn line_chars(line: &Line<'_>) -> Vec<char> {
    line.spans.iter().flat_map(|s| s.content.chars()).collect()
}

/// The chars `start..end` of a styled line, keeping each span's style.
pub fn slice_line(line: &Line<'_>, start: usize, end: usize) -> Line<'static> {
    let mut spans = Vec::new();
    let mut at = 0;
    for span in &line.spans {
        let n = span.content.chars().count();
        let (lo, hi) = (start.max(at), end.min(at + n));
        if lo < hi {
            let text: String = span.content.chars().skip(lo - at).take(hi - lo).collect();
            spans.push(Span::styled(text, span.style));
        }
        at += n;
    }
    Line::from(spans).style(line.style)
}

/// Soft-wrap a styled line to `width` columns, without marks.
pub fn wrap_line(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    let chars = line_chars(line);
    wrap_ranges(&chars, width)
        .into_iter()
        .map(|(s, e)| slice_line(line, s, e))
        .collect()
}

/// One screen row of a laid-out text: a char range of a source line.
struct VisualRow {
    line: usize,
    start: usize,
    end: usize,
}

fn layout(lines: &[Vec<char>], wrap: bool, width: usize) -> Vec<VisualRow> {
    let mut rows = Vec::new();
    for (i, chars) in lines.iter().enumerate() {
        if wrap {
            for (start, end) in wrap_ranges(chars, width) {
                rows.push(VisualRow {
                    line: i,
                    start,
                    end,
                });
            }
        } else {
            rows.push(VisualRow {
                line: i,
                start: 0,
                end: chars.len(),
            });
        }
    }
    rows
}

fn mark_style() -> RStyle {
    theme::resolve(Style::Dim)
}

const WRAP_MARK: (&str, &str) = ("\u{21aa}", ">");
const LEFT_MARK: (&str, &str) = ("\u{2039}", "<");
const RIGHT_MARK: (&str, &str) = ("\u{203a}", ">");

fn glyph(pair: (&'static str, &'static str)) -> &'static str {
    rgit_model::glyph(pair.0, pair.1)
}

/// Draw one visual row: a gutter cell (a wrap or a hidden-left mark), the
/// text and a hidden-right mark over the last cell when the line goes on.
fn row_line(
    source: &Line<'_>,
    chars: &[char],
    row: &VisualRow,
    wrap: bool,
    left: usize,
    width: usize,
) -> Line<'static> {
    let mut spans = Vec::new();
    if wrap {
        let gutter = if row.start > 0 { glyph(WRAP_MARK) } else { " " };
        spans.push(Span::styled(gutter, mark_style()));
        spans.extend(slice_line(source, row.start, row.end).spans);
    } else {
        let (s, e) = col_range(chars, left, width);
        let hidden_left = left > 0 && !chars.is_empty();
        let hidden_right = e < chars.len();
        let gutter = if hidden_left { glyph(LEFT_MARK) } else { " " };
        spans.push(Span::styled(gutter, mark_style()));
        let e = if hidden_right && e > s { e - 1 } else { e };
        spans.extend(slice_line(source, s, e).spans);
        if hidden_right {
            let used = text_width(&chars[s..e]);
            spans.push(Span::raw(" ".repeat(width.saturating_sub(used + 1))));
            spans.push(Span::styled(glyph(RIGHT_MARK), mark_style()));
        }
    }
    Line::from(spans).style(source.style)
}

/// Render read-only text in `area` with a one-cell mark gutter on the left
/// and a scrollbar on the right. A following view stays on the last row.
pub fn render_text(
    frame: &mut Frame,
    area: Rect,
    lines: &[Line<'static>],
    wrap: bool,
    view: &Viewport,
) {
    if area.width < 3 || area.height == 0 {
        return;
    }
    let width = area.width as usize - 2;
    let chars: Vec<Vec<char>> = lines.iter().map(line_chars).collect();
    let rows = layout(&chars, wrap, width);
    let page = area.height as usize;
    view.settle(rows.len(), page);
    if view.follow.get() {
        view.top.set(view.max_top());
    }
    if !wrap {
        let widest = chars.iter().map(|c| text_width(c)).max().unwrap_or(0);
        view.left
            .set(view.left.get().min(widest.saturating_sub(width - 1)));
    }
    let top = view.top.get();
    let out: Vec<Line> = rows[top..]
        .iter()
        .take(page)
        .map(|r| {
            row_line(
                &lines[r.line],
                &chars[r.line],
                r,
                wrap,
                view.left.get(),
                width,
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(out), area);
    crate::ui::render_scrollbar(frame, area, rows.len(), top, page);
}

/// The text of an editor box.
pub struct EditorText<'a> {
    pub lines: &'a [String],
    /// Caret as (line, char column).
    pub caret: (usize, usize),
    pub style_of: &'a dyn Fn(usize) -> RStyle,
    /// A guide column tinted on each line's first row.
    pub ruler: Option<usize>,
}

/// Render an editable text in `area` and keep the caret in view while the
/// view follows it. Returns the caret's screen cell when it is visible.
pub fn render_editor(
    frame: &mut Frame,
    area: Rect,
    text: &EditorText<'_>,
    wrap: bool,
    view: &Viewport,
) -> Option<(u16, u16)> {
    let EditorText {
        lines,
        caret,
        style_of,
        ruler,
    } = *text;
    if area.width < 4 || area.height == 0 {
        return None;
    }
    // The gutter and the scrollbar take a cell each; one more column keeps
    // the caret on screen past the end of a full row.
    let width = area.width as usize - 2;
    let chars: Vec<Vec<char>> = lines.iter().map(|l| l.chars().collect()).collect();
    let rows = layout(&chars, wrap, width - 1);
    let page = area.height as usize;
    let (crow, ccol) = caret;
    let ccol = ccol.min(chars.get(crow).map_or(0, Vec::len));
    let caret_row = rows
        .iter()
        .rposition(|r| r.line == crow && r.start <= ccol)
        .unwrap_or(0);
    let caret_x = text_width(&chars[crow][rows[caret_row].start..ccol]);

    view.settle(rows.len(), page);
    if view.follow.get() {
        let top = view.top.get();
        if caret_row < top {
            view.top.set(caret_row);
        } else if caret_row >= top + page {
            view.top.set(caret_row + 1 - page);
        }
    }
    // Without wrap the caret stays off the last cell, which holds the
    // hidden-right mark.
    let mut left = if wrap { 0 } else { view.left.get() };
    if !wrap {
        if caret_x < left {
            left = caret_x;
        } else if caret_x + 2 > left + width {
            left = caret_x + 2 - width;
        }
        view.left.set(left);
    }

    let top = view.top.get();
    let out: Vec<Line> = rows[top..]
        .iter()
        .take(page)
        .map(|r| {
            let source = Line::styled(lines[r.line].clone(), style_of(r.line));
            row_line(&source, &chars[r.line], r, wrap, left, width)
        })
        .collect();
    frame.render_widget(Paragraph::new(out), area);

    if let Some(at) = ruler {
        let tint = RStyle::default().bg(theme::cursor_bg());
        let buf = frame.buffer_mut();
        for (y, r) in rows[top..].iter().take(page).enumerate() {
            if r.start == 0 && at >= left && at - left < width {
                let x = area.x + 1 + (at - left) as u16;
                buf[(x, area.y + y as u16)].set_style(tint);
            }
        }
    }
    crate::ui::render_scrollbar(frame, area, rows.len(), top, page);

    let y = caret_row.checked_sub(top).filter(|y| *y < page)?;
    Some((area.x + 1 + (caret_x - left) as u16, area.y + y as u16))
}

/// A one-line input in `width` cells with a block caret at `cursor` (in
/// chars). Text past either edge scrolls and shows a mark at that edge.
pub fn input_spans(
    text: &str,
    cursor: usize,
    masked: bool,
    width: usize,
    left: &Cell<usize>,
) -> Vec<Span<'static>> {
    let chars: Vec<char> = if masked {
        std::iter::repeat_n('\u{2022}', text.chars().count()).collect()
    } else {
        text.chars().collect()
    };
    let cursor = cursor.min(chars.len());
    let caret_x = text_width(&chars[..cursor]);
    let total = text_width(&chars) + 1;
    let width = width.max(3);
    let mut l = left.get();
    if total <= width {
        l = 0;
    } else {
        if caret_x <= l && l > 0 {
            l = caret_x.saturating_sub(1);
        } else if caret_x + 2 > l + width {
            l = caret_x + 2 - width;
        }
        l = l.min(total - width);
    }
    left.set(l);

    let (s, e) = col_range(&chars, l, width);
    let hidden_left = l > 0;
    let hidden_right = e < chars.len();
    let s = if hidden_left { s + 1 } else { s };
    let e = if hidden_right {
        e.saturating_sub(1).max(s)
    } else {
        e
    };
    let caret = RStyle::default().add_modifier(Modifier::REVERSED);
    let mut spans = Vec::new();
    if hidden_left {
        spans.push(Span::styled(glyph(LEFT_MARK), mark_style()));
    }
    let before: String = chars[s..cursor.max(s).min(e)].iter().collect();
    spans.push(Span::raw(before));
    if cursor < e {
        spans.push(Span::styled(chars[cursor].to_string(), caret));
        spans.push(Span::raw(chars[cursor + 1..e].iter().collect::<String>()));
    } else {
        spans.push(Span::styled(" ", caret));
    }
    if hidden_right {
        spans.push(Span::styled(glyph(RIGHT_MARK), mark_style()));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn texts(spans: &[Span<'_>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn wrap_breaks_after_spaces() {
        let c = chars("alpha beta gamma");
        assert_eq!(wrap_ranges(&c, 8), vec![(0, 6), (6, 11), (11, 16)]);
        assert_eq!(wrap_ranges(&c, 10), vec![(0, 11), (11, 16)]);
    }

    #[test]
    fn wrap_breaks_hard_inside_a_long_word() {
        let c = chars("abcdefghij");
        assert_eq!(wrap_ranges(&c, 4), vec![(0, 4), (4, 8), (8, 10)]);
    }

    #[test]
    fn wrap_keeps_an_empty_line_as_one_row() {
        assert_eq!(wrap_ranges(&[], 10), vec![(0, 0)]);
    }

    #[test]
    fn wrap_counts_wide_chars_as_two_columns() {
        let c = chars("\u{4e00}\u{4e01}\u{4e02}");
        assert_eq!(wrap_ranges(&c, 4), vec![(0, 2), (2, 3)]);
    }

    #[test]
    fn slice_keeps_span_styles() {
        let bold = RStyle::default().add_modifier(Modifier::BOLD);
        let line = Line::from(vec![Span::raw("ab"), Span::styled("cd", bold)]);
        let out = slice_line(&line, 1, 3);
        assert_eq!(out.spans, vec![Span::raw("b"), Span::styled("c", bold)]);
    }

    #[test]
    fn input_fits_without_scroll() {
        let left = Cell::new(5);
        let spans = input_spans("abc", 3, false, 10, &left);
        assert_eq!(left.get(), 0);
        assert_eq!(texts(&spans), "abc ");
    }

    #[test]
    fn input_scrolls_to_keep_the_caret_in_view() {
        let left = Cell::new(0);
        let text = "0123456789abcdefghij";
        let spans = input_spans(text, 20, false, 10, &left);
        let shown = texts(&spans);
        assert!(shown.starts_with(glyph(LEFT_MARK)), "{shown}");
        assert!(shown.ends_with("ghij "), "{shown}");
        assert_eq!(shown.chars().count(), 10);

        let spans = input_spans(text, 0, false, 10, &left);
        let shown = texts(&spans);
        assert_eq!(left.get(), 0);
        assert!(shown.starts_with("0123"), "{shown}");
        assert!(shown.ends_with(glyph(RIGHT_MARK)), "{shown}");
    }

    #[test]
    fn input_masks_secrets() {
        let left = Cell::new(0);
        let spans = input_spans("pw", 2, true, 10, &left);
        assert_eq!(texts(&spans), "\u{2022}\u{2022} ");
    }

    fn draw_editor(
        lines: &[&str],
        caret: (usize, usize),
        wrap: bool,
        view: &Viewport,
    ) -> (Vec<String>, Option<(u16, u16)>) {
        let lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        let mut term = Terminal::new(TestBackend::new(12, 3)).unwrap();
        let mut pos = None;
        term.draw(|f| {
            let text = EditorText {
                lines: &lines,
                caret,
                style_of: &|_| RStyle::default(),
                ruler: None,
            };
            pos = render_editor(f, f.area(), &text, wrap, view);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let rows = (0..3)
            .map(|y| (0..12).map(|x| buf[(x, y)].symbol().to_owned()).collect())
            .collect();
        (rows, pos)
    }

    #[test]
    fn editor_scrolls_down_to_the_caret() {
        let view = Viewport::default();
        view.follow.set(true);
        let lines = ["a", "b", "c", "d", "e", "f"];
        let (rows, pos) = draw_editor(&lines, (5, 1), true, &view);
        assert_eq!(view.top.get(), 3);
        assert_eq!(pos, Some((2, 2)));
        assert!(rows[2].starts_with(" f"), "{rows:?}");
    }

    #[test]
    fn editor_wraps_with_a_mark_on_continuation_rows() {
        let view = Viewport::default();
        view.follow.set(true);
        let (rows, pos) = draw_editor(&["aaaa bbbb cccc"], (0, 14), true, &view);
        assert!(rows[0].starts_with(" aaaa bbbb"), "{rows:?}");
        assert!(
            rows[1].starts_with(&format!("{}cccc", glyph(WRAP_MARK))),
            "{rows:?}"
        );
        assert_eq!(pos, Some((5, 1)));
    }

    #[test]
    fn editor_without_wrap_scrolls_sideways_to_the_caret() {
        let view = Viewport::default();
        view.follow.set(true);
        let line = "0123456789abcdefghij";
        let (rows, pos) = draw_editor(&[line], (0, 20), false, &view);
        assert!(view.left.get() > 0);
        assert!(rows[0].starts_with(glyph(LEFT_MARK)), "{rows:?}");
        let (x, _) = pos.unwrap();
        assert!(x < 11, "caret at {x}");

        let (rows, _) = draw_editor(&[line], (0, 0), false, &view);
        assert_eq!(view.left.get(), 0);
        assert!(rows[0].contains(glyph(RIGHT_MARK)), "{rows:?}");
    }

    fn draw_text(lines: &[&str], wrap: bool, view: &Viewport) -> Vec<String> {
        let lines: Vec<Line<'static>> = lines.iter().map(|s| Line::from(s.to_string())).collect();
        let mut term = Terminal::new(TestBackend::new(12, 3)).unwrap();
        term.draw(|f| render_text(f, f.area(), &lines, wrap, view))
            .unwrap();
        let buf = term.backend().buffer().clone();
        (0..3)
            .map(|y| (0..12).map(|x| buf[(x, y)].symbol().to_owned()).collect())
            .collect()
    }

    #[test]
    fn text_follows_the_tail_of_a_stream() {
        let view = Viewport::default();
        view.follow.set(true);
        let rows = draw_text(&["1", "2", "3", "4", "5"], true, &view);
        assert_eq!(view.top.get(), 2);
        assert!(rows[2].starts_with(" 5"), "{rows:?}");
    }

    #[test]
    fn text_wraps_long_lines_with_a_mark() {
        let view = Viewport::default();
        let rows = draw_text(&["0123456789abcdef"], true, &view);
        assert!(rows[0].starts_with(" 0123456789"), "{rows:?}");
        assert!(
            rows[1].starts_with(&format!("{}abcdef", glyph(WRAP_MARK))),
            "{rows:?}"
        );
    }

    #[test]
    fn text_without_wrap_marks_and_pans_hidden_columns() {
        let view = Viewport::default();
        let rows = draw_text(&["0123456789abcdef"], false, &view);
        assert!(rows[0].starts_with(" 012345678"), "{rows:?}");
        assert!(rows[0].contains(glyph(RIGHT_MARK)), "{rows:?}");
        view.scroll(ScrollBy::Cols(4));
        let rows = draw_text(&["0123456789abcdef"], false, &view);
        assert!(
            rows[0].starts_with(&format!("{}456789", glyph(LEFT_MARK))),
            "{rows:?}"
        );
    }

    #[test]
    fn wheel_scroll_stops_following() {
        let view = Viewport::default();
        view.follow.set(true);
        view.settle(10, 3);
        view.scroll(ScrollBy::Rows(2));
        assert_eq!(view.top.get(), 2);
        assert!(!view.follow.get());
        view.scroll(ScrollBy::Bottom);
        assert!(view.at_bottom());
    }

    #[test]
    fn border_drag_resizes_a_centered_box() {
        let mut boxes = Boxes::default();
        boxes.begin_frame();
        let area = Rect::new(0, 0, 100, 40);
        let rect = boxes.place(BoxId::CommitEditor, (40, 10), area, Anchor::Center);
        assert_eq!(rect, Rect::new(30, 15, 40, 10));
        assert!(boxes.press(rect.right() - 1, 20));
        boxes.drag_to(rect.right() + 9, 20);
        boxes.release();
        boxes.begin_frame();
        let rect = boxes.place(BoxId::CommitEditor, (40, 10), area, Anchor::Center);
        assert_eq!(rect.width, 59);
        assert_eq!(rect.height, 10);
    }

    #[test]
    fn border_drag_resizes_a_docked_box_from_its_top() {
        let mut boxes = Boxes::default();
        boxes.begin_frame();
        let area = Rect::new(0, 0, 80, 40);
        let rect = boxes.place(BoxId::Console, (80, 10), area, Anchor::Bottom);
        assert!(!boxes.press(5, rect.y + 1));
        assert!(boxes.press(5, rect.y));
        boxes.drag_to(5, 20);
        boxes.begin_frame();
        let rect = boxes.place(BoxId::Console, (80, 10), area, Anchor::Bottom);
        assert_eq!(rect.height, 20);
    }

    #[test]
    fn maximize_fills_the_area() {
        let mut boxes = Boxes::default();
        boxes.toggle_maximized(BoxId::Confirm);
        boxes.begin_frame();
        let area = Rect::new(1, 1, 50, 20);
        assert_eq!(
            boxes.place(BoxId::Confirm, (10, 4), area, Anchor::Center),
            area
        );
    }

    #[test]
    fn a_reopened_box_starts_with_a_fresh_view() {
        let mut boxes = Boxes::default();
        boxes.begin_frame();
        boxes.show(BoxId::Help).top.set(4);
        boxes.begin_frame();
        assert_eq!(boxes.show(BoxId::Help).top.get(), 4);
        boxes.begin_frame();
        boxes.begin_frame();
        assert_eq!(boxes.show(BoxId::Help).top.get(), 0);
    }
}
