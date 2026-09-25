use std::collections::HashSet;

use rgit_model::{Section, SectionId, Span, Target};

/// A line-level selection resolved from the visual region: the hunk it lives in
/// and the indices of the selected lines within that hunk.
pub struct LineSelection {
    pub path: String,
    pub new_start: u32,
    pub staged: bool,
    pub lines: Vec<usize>,
}

/// One visible line, borrowing its content from the tree so rendering copies no
/// string data. Produced lazily by [`Buffer::rows`].
pub struct Row<'a> {
    pub depth: usize,
    pub spans: &'a [Span],
    pub foldable: bool,
    pub folded: bool,
    pub id: &'a SectionId,
    pub target: Option<&'a Target>,
}

/// An active visual selection: whole rows (vim `V`, and the magit `v` used for
/// staging) or a character range (vim `v`).
#[derive(Clone, Copy)]
enum Visual {
    /// Anchor row; the region spans `anchor..=cursor` rows.
    Line(usize),
    /// Anchor (row, col); the region spans anchor to the (cursor, col) caret.
    Char { row: usize, col: usize },
}

/// The interactive view over a content tree: fold state, cursor, and scroll,
/// all keyed so they survive the rebuild after every refresh. Fold state is a
/// set of section ids, not a flag on the node, so a rebuilt tree keeps its folds.
#[derive(Default)]
pub struct Buffer {
    root: Vec<Section>,
    folded: HashSet<SectionId>,
    cursor: usize,
    /// Char column of the caret within the cursor row, for charwise visual.
    col: usize,
    scroll: usize,
    height: usize,
    visible_len: usize,
    visual: Option<Visual>,
}

impl Buffer {
    /// Replace the content, preserving fold state, cursor, and scroll position.
    /// A rebuild invalidates row indices, so any selection is cleared.
    pub fn set_content(&mut self, root: Vec<Section>) {
        self.root = root;
        self.visual = None;
        self.recount();
        self.cursor = self.cursor.min(self.visible_len.saturating_sub(1));
        self.col = self.col.min(self.cursor_row_len());
        self.scroll_into_view();
    }

    /// Start (or clear) a linewise visual selection at the cursor - vim `V`, and
    /// the magit `v` used for line-range staging.
    pub fn toggle_selection(&mut self) {
        self.visual = match self.visual {
            Some(Visual::Line(_)) => None,
            _ => Some(Visual::Line(self.cursor)),
        };
    }

    /// Start (or clear) a charwise visual selection at the caret - vim `v`.
    pub fn toggle_char_selection(&mut self) {
        self.visual = match self.visual {
            Some(Visual::Char { .. }) => None,
            _ => Some(Visual::Char {
                row: self.cursor,
                col: self.col.min(self.cursor_row_len()),
            }),
        };
    }

    /// The row span the selection covers (both visual modes), for staging and
    /// whole-row highlighting.
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor_row = match self.visual? {
            Visual::Line(r) => r,
            Visual::Char { row, .. } => row,
        };
        Some((anchor_row.min(self.cursor), anchor_row.max(self.cursor)))
    }

    /// The charwise selection as normalized inclusive `(row, col)` endpoints,
    /// only when a charwise visual is active.
    pub fn char_selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let Visual::Char { row, col } = self.visual? else {
            return None;
        };
        let a = (row, col);
        let b = (self.cursor, self.col);
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    pub fn char_col(&self) -> usize {
        self.col
    }

    pub fn is_char_visual(&self) -> bool {
        matches!(self.visual, Some(Visual::Char { .. }))
    }

    pub fn clear_selection(&mut self) {
        self.visual = None;
    }

    pub fn has_selection(&self) -> bool {
        self.visual.is_some()
    }

    /// The text to yank: the charwise substring, the linewise rows (diff lines
    /// stripped to clean code), or - with no selection - the cursor row.
    pub fn selected_text(&self) -> String {
        if let Some(((r0, c0), (r1, c1))) = self.char_selection() {
            let texts: Vec<String> = self.rows().map(|r| row_text(&r)).collect();
            if r0 == r1 {
                return substr_inclusive(texts.get(r0).map(String::as_str).unwrap_or(""), c0, c1);
            }
            let end = r1.min(texts.len().saturating_sub(1));
            let mut out = Vec::new();
            for (r, t) in texts.iter().enumerate().take(end + 1).skip(r0) {
                let chars: Vec<char> = t.chars().collect();
                let piece: String = if r == r0 {
                    chars[c0.min(chars.len())..].iter().collect()
                } else if r == r1 {
                    chars[..(c1 + 1).min(chars.len())].iter().collect()
                } else {
                    t.clone()
                };
                out.push(piece);
            }
            return out.join("\n");
        }
        let (lo, hi) = self.selection_range().unwrap_or((self.cursor, self.cursor));
        self.rows()
            .skip(lo)
            .take(hi - lo + 1)
            .map(|r| row_yank_text(&r))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Resolve the visual region to the diff lines it covers within a single
    /// hunk, for line-level staging. Returns `None` if no region is active or it
    /// covers no diff lines.
    pub fn line_selection(&self) -> Option<LineSelection> {
        let (lo, hi) = self.selection_range()?;
        let rows: Vec<Row> = self.rows().collect();
        let mut sel: Option<LineSelection> = None;

        for row in rows.get(lo..=hi.min(rows.len().saturating_sub(1)))? {
            if row.foldable {
                continue;
            }
            // Only working-tree hunks (staged = Some) are stageable; a read-only
            // diff (a commit or diff between revs) has no staging.
            let Some(Target::Hunk {
                path,
                new_start,
                staged: Some(staged),
                ..
            }) = &row.target
            else {
                continue;
            };
            let Some(index) = row.id.rsplit_once('/').and_then(|(_, j)| j.parse().ok()) else {
                continue;
            };
            match &mut sel {
                None => {
                    sel = Some(LineSelection {
                        path: path.clone(),
                        new_start: *new_start,
                        staged: *staged,
                        lines: vec![index],
                    })
                }
                // Restrict to the first hunk the region touches.
                Some(s) if s.path == *path && s.new_start == *new_start => s.lines.push(index),
                Some(_) => {}
            }
        }
        sel.filter(|s| !s.lines.is_empty())
    }

    /// The diff line under the cursor, as `(path, new-side file line, caret
    /// column)`. `None` unless the cursor is on a diff line (a leaf carrying a
    /// `Target::Hunk`). The file line rides in the target, so this works in every
    /// diff view (status, commit, diff) through one code path.
    pub fn cursor_diff_line(&self) -> Option<(String, u32, usize)> {
        let row = self.rows().nth(self.cursor)?;
        if row.foldable {
            return None;
        }
        let Target::Hunk { path, line, .. } = row.target? else {
            return None;
        };
        Some((path.clone(), *line, self.col))
    }

    /// Whether the row under the cursor can fold (a file or section header).
    pub fn cursor_is_foldable(&self) -> bool {
        self.rows().nth(self.cursor).is_some_and(|r| r.foldable)
    }

    pub fn is_empty(&self) -> bool {
        self.visible_len == 0
    }

    /// Number of visible rows.
    pub fn len(&self) -> usize {
        self.visible_len
    }

    /// Park the cursor on the first foldable row (a file or hunk), skipping any
    /// leading title/info rows. Used when focus enters a preview pane so the
    /// first fold keypress acts on something.
    pub fn cursor_to_first_foldable(&mut self) {
        if let Some(idx) = self.rows().position(|r| r.foldable) {
            self.set_cursor(idx);
        }
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Char length of the row the cursor is on (its rendered text).
    fn cursor_row_len(&self) -> usize {
        self.rows()
            .nth(self.cursor)
            .map(|r| row_text(&r).chars().count())
            .unwrap_or(0)
    }

    /// Move the caret within the current row by `delta` chars (vim h/l). Clamps
    /// to the row; the caret may rest one past the last char for selection ends.
    pub fn move_col(&mut self, delta: isize) {
        let len = self.cursor_row_len();
        self.col = (self.col as isize + delta).clamp(0, len as isize) as usize;
    }

    pub fn col_line_start(&mut self) {
        self.col = 0;
    }

    pub fn col_line_end(&mut self) {
        self.col = self.cursor_row_len();
    }

    /// Move the caret to the start of the next word on the row (vim `w`).
    pub fn col_word_forward(&mut self) {
        let chars: Vec<char> = self.cursor_row_text().chars().collect();
        let mut i = self.col.min(chars.len());
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        self.col = i;
    }

    /// Move the caret to the start of the current/previous word (vim `b`).
    pub fn col_word_back(&mut self) {
        let chars: Vec<char> = self.cursor_row_text().chars().collect();
        let mut i = self.col.min(chars.len());
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        self.col = i;
    }

    /// Move the caret to the end of the current/next word (vim `e`).
    pub fn col_word_end(&mut self) {
        let chars: Vec<char> = self.cursor_row_text().chars().collect();
        let mut i = (self.col + 1).min(chars.len());
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        self.col = i.saturating_sub(1).min(chars.len().saturating_sub(1));
    }

    /// Move the caret to the first non-blank char of the row (vim `^`).
    pub fn col_first_nonblank(&mut self) {
        let chars: Vec<char> = self.cursor_row_text().chars().collect();
        let mut i = 0;
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        self.col = i;
    }

    fn cursor_row_text(&self) -> String {
        self.rows()
            .nth(self.cursor)
            .map(|r| row_text(&r))
            .unwrap_or_default()
    }

    /// Move the cursor to a specific visible row, clamped and scrolled into view.
    pub fn set_cursor(&mut self, idx: usize) {
        self.cursor = idx.min(self.visible_len.saturating_sub(1));
        self.col = self.col.min(self.cursor_row_len());
        self.scroll_into_view();
    }

    /// Put the cursor on the section `id` and scroll it to the top of the view.
    pub fn jump_to(&mut self, id: &str) {
        if let Some(idx) = self.rows().position(|r| r.id == id) {
            self.set_cursor(idx);
            self.scroll = idx.min(self.visible_len.saturating_sub(self.height.max(1)));
        }
    }

    pub fn cursor_top(&mut self) {
        self.set_cursor(0);
    }

    pub fn cursor_bottom(&mut self) {
        self.set_cursor(self.visible_len.saturating_sub(1));
    }

    /// The viewport height in rows, for half-page motions.
    pub fn page(&self) -> usize {
        self.height
    }

    /// The first visible row at or after `from` (wrapping) whose text contains
    /// `query` case-insensitively, searched forward or backward.
    pub fn search(&self, query: &str, from: usize, forward: bool) -> Option<usize> {
        if query.is_empty() || self.visible_len == 0 {
            return None;
        }
        let needle = query.to_lowercase();
        let texts: Vec<String> = self.rows().map(|r| row_text(&r)).collect();
        let n = texts.len();
        (0..n)
            .map(|step| {
                if forward {
                    (from + step) % n
                } else {
                    (from + n - step) % n
                }
            })
            .find(|&idx| texts[idx].to_lowercase().contains(&needle))
    }

    /// Set the viewport height (rows) from the render area; adjusts scroll.
    pub fn set_height(&mut self, height: usize) {
        self.height = height;
        self.scroll_into_view();
    }

    pub fn move_cursor(&mut self, delta: isize) {
        if self.visible_len == 0 {
            return;
        }
        let max = self.visible_len as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, max) as usize;
        self.col = self.col.min(self.cursor_row_len());
        self.scroll_into_view();
    }

    /// Scroll the viewport by `delta` rows (a mouse wheel). Moves the scroll
    /// offset and the cursor together so the content shifts even when the cursor
    /// would otherwise stay on screen, and the two never fight `scroll_into_view`.
    pub fn scroll_by(&mut self, delta: isize) {
        if self.visible_len == 0 {
            return;
        }
        let max_cursor = self.visible_len as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, max_cursor) as usize;
        let max_scroll = self.visible_len.saturating_sub(self.height.max(1)) as isize;
        self.scroll = (self.scroll as isize + delta).clamp(0, max_scroll) as usize;
        self.col = self.col.min(self.cursor_row_len());
    }

    /// Toggle the fold of the section at the cursor. When the cursor sits on a
    /// non-foldable line (a diff line inside a hunk), fold the enclosing
    /// foldable section instead and park the cursor on it, so Tab anywhere in a
    /// hunk collapses it.
    pub fn toggle_fold(&mut self) {
        let Some((idx, id)) = self.foldable_at_cursor() else {
            return;
        };
        if !self.folded.remove(&id) {
            self.folded.insert(id);
        }
        self.recount();
        self.cursor = idx.min(self.visible_len.saturating_sub(1));
        self.scroll_into_view();
    }

    /// The foldable section the cursor acts on: the cursor row when it is itself
    /// foldable, otherwise the nearest enclosing foldable ancestor. Returns its
    /// visible index and id.
    fn foldable_at_cursor(&self) -> Option<(usize, SectionId)> {
        let rows: Vec<Row<'_>> = self.rows().collect();
        let cur = rows.get(self.cursor)?;
        if cur.foldable {
            return Some((self.cursor, cur.id.clone()));
        }
        // Climb outward: the nearest preceding row at a shallower depth is the
        // parent; keep climbing until one is foldable.
        let mut depth = cur.depth;
        for (i, r) in rows[..self.cursor].iter().enumerate().rev() {
            if r.depth < depth {
                if r.foldable {
                    return Some((i, r.id.clone()));
                }
                depth = r.depth;
            }
        }
        None
    }

    /// The rows currently in the viewport, borrowing content (no allocation of
    /// the row data itself).
    pub fn viewport(&self) -> impl Iterator<Item = Row<'_>> {
        self.rows().skip(self.scroll).take(self.height.max(1))
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// The staging target under the cursor, if any.
    pub fn target_at_cursor(&self) -> Option<Target> {
        self.rows().nth(self.cursor).and_then(|r| r.target.cloned())
    }

    /// The section id of the row under the cursor, for views that key their
    /// actions off structured ids (e.g. the lanes view).
    pub fn cursor_id(&self) -> Option<String> {
        self.rows().nth(self.cursor).map(|r| r.id.clone())
    }

    /// A lazy pre-order walk of the visible tree. The only per-call allocation
    /// is the descent stack, sized to the tree depth.
    pub fn rows(&self) -> Rows<'_> {
        Rows {
            stack: vec![self.root.iter()],
            folded: &self.folded,
        }
    }

    fn recount(&mut self) {
        self.visible_len = self.rows().count();
    }

    fn scroll_into_view(&mut self) {
        let height = self.height.max(1);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + height {
            self.scroll = self.cursor + 1 - height;
        }
        // Never leave blank rows below the content: if it all fits (or a taller
        // viewport opened after a scroll), pull the view back up. This also
        // corrects an over-scroll from scrolling to the tail before the height
        // was known.
        self.scroll = self.scroll.min(self.visible_len.saturating_sub(height));
    }
}

/// The concatenated text of a row's spans, for searching.
fn row_text(row: &Row<'_>) -> String {
    row.spans.iter().map(|s| s.text.as_str()).collect()
}

/// The inclusive char slice `text[lo..=hi]`, clamped to the string.
fn substr_inclusive(text: &str, lo: usize, hi: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let lo = lo.min(chars.len());
    let hi = (hi + 1).min(chars.len());
    chars[lo..hi.max(lo)].iter().collect()
}

/// Row text for yanking: on a diff line, strip the line-number gutter and the
/// +/- marker so the clipboard gets clean code; other rows yank as rendered.
fn row_yank_text(row: &Row<'_>) -> String {
    // The diff gutter is always the first span: a fixed 10-column field of
    // right-aligned old/new line numbers, so only digits and spaces.
    if let Some(first) = row.spans.first() {
        let g = first.text.as_str();
        let is_gutter =
            g.chars().count() == 10 && g.bytes().all(|b| b.is_ascii_digit() || b == b' ');
        if is_gutter {
            let rest: String = row.spans[1..].iter().map(|s| s.text.as_str()).collect();
            return rest
                .strip_prefix(['+', '-', ' '])
                .unwrap_or(&rest)
                .to_owned();
        }
    }
    row_text(row)
}

/// Lazy pre-order iterator over visible rows.
pub struct Rows<'a> {
    stack: Vec<std::slice::Iter<'a, Section>>,
    folded: &'a HashSet<SectionId>,
}

impl<'a> Iterator for Rows<'a> {
    type Item = Row<'a>;

    fn next(&mut self) -> Option<Row<'a>> {
        loop {
            if self.stack.is_empty() {
                return None;
            }
            let depth = self.stack.len() - 1;
            match self.stack.last_mut().unwrap().next() {
                Some(section) => {
                    let foldable = section.is_foldable();
                    // The fold set overrides each node's default fold state.
                    let folded = section.default_folded ^ self.folded.contains(&section.id);
                    if foldable && !folded {
                        self.stack.push(section.children.iter());
                    }
                    return Some(Row {
                        depth,
                        spans: &section.spans,
                        foldable,
                        folded,
                        id: &section.id,
                        target: section.target.as_ref(),
                    });
                }
                None => {
                    self.stack.pop();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgit_model::{NodeKind, Span};

    fn leaf(id: &str) -> Section {
        Section::leaf(id, NodeKind::File, vec![Span::plain(id)])
    }

    fn section_with_two_files() -> Vec<Section> {
        vec![Section::branch(
            "unstaged",
            NodeKind::Section,
            vec![Span::plain("Unstaged")],
            vec![leaf("unstaged/a"), leaf("unstaged/b")],
        )]
    }

    #[test]
    fn yank_strips_diff_gutter_and_marker() {
        // A diff line: 10-char line-number gutter, then a +marker glued to code.
        let added = Section::leaf(
            "h/0",
            NodeKind::DiffLine,
            vec![
                Span::plain("   1    2 "), // gutter (exactly 10 cols)
                Span::plain("+let x = 1;"),
            ],
        );
        let context = Section::leaf(
            "h/1",
            NodeKind::DiffLine,
            vec![Span::plain("   2    3 "), Span::plain(" untouched")],
        );
        let mut b = buffer(vec![added, context], 10);
        assert_eq!(b.selected_text(), "let x = 1;");
        b.set_cursor(1);
        assert_eq!(b.selected_text(), "untouched");
        // A whole selection yields clean code on every line.
        b.set_cursor(0);
        b.toggle_selection();
        b.set_cursor(1);
        assert_eq!(b.selected_text(), "let x = 1;\nuntouched");
    }

    #[test]
    fn charwise_yank_extracts_substring() {
        let row = Section::leaf(
            "r",
            NodeKind::Commit,
            vec![Span::plain("abc1234 summary here")],
        );
        let mut b = buffer(vec![row], 10);
        b.toggle_char_selection(); // anchor at (0, 0)
        for _ in 0..6 {
            b.move_col(1); // caret at col 6 -> inclusive [0,6]
        }
        assert_eq!(b.selected_text(), "abc1234");
        // Word/line-end motions land the caret sensibly.
        b.col_line_end();
        assert_eq!(b.char_col(), "abc1234 summary here".chars().count());
        b.col_line_start();
        b.col_word_forward();
        assert_eq!(b.char_col(), 8); // start of "summary"
    }

    #[test]
    fn charwise_yank_spans_multiple_rows() {
        let a = Section::leaf("a", NodeKind::Commit, vec![Span::plain("hello")]);
        let c = Section::leaf("b", NodeKind::Commit, vec![Span::plain("world")]);
        let mut b = buffer(vec![a, c], 10);
        // Anchor mid-first-row, extend down to the same col on the second row.
        b.move_col(2); // caret at col 2 of "hello"
        b.toggle_char_selection(); // anchor (0, 2)
        b.move_cursor(1); // row 1, col clamped to 2 -> caret (1, 2)
        assert_eq!(b.selected_text(), "llo\nwor");
    }

    #[test]
    fn yank_leaves_non_diff_rows_intact() {
        // A commit row (no gutter) yanks exactly as rendered.
        let commit = Section::leaf(
            "log/abc",
            NodeKind::Commit,
            vec![Span::plain("abc1234  do the thing")],
        );
        let b = buffer(vec![commit], 10);
        assert_eq!(b.selected_text(), "abc1234  do the thing");
    }

    fn buffer(content: Vec<Section>, height: usize) -> Buffer {
        let mut b = Buffer::default();
        b.set_height(height);
        b.set_content(content);
        b
    }

    #[test]
    fn search_finds_and_wraps() {
        let b = buffer(section_with_two_files(), 10);
        // rows: 0 "Unstaged", 1 "unstaged/a", 2 "unstaged/b"
        assert_eq!(b.search("/b", 0, true), Some(2));
        assert_eq!(b.search("/a", 0, true), Some(1));
        // forward from the last row wraps around to an earlier match
        assert_eq!(b.search("/a", 2, true), Some(1));
        // backward search
        assert_eq!(b.search("/b", 0, false), Some(2));
        assert_eq!(b.search("zzz", 0, true), None);
    }

    #[test]
    fn cursor_to_first_foldable_skips_leading_leaves() {
        // A preview-shaped tree: a non-foldable title, then a foldable file.
        let content = vec![
            leaf("diff/title"),
            Section::branch(
                "diff/file",
                NodeKind::File,
                vec![Span::plain("f.txt")],
                vec![leaf("diff/file/line")],
            ),
        ];
        let mut b = buffer(content, 10);
        assert_eq!(b.cursor(), 0);
        b.cursor_to_first_foldable();
        assert_eq!(b.cursor(), 1);
        // Fold now acts on the file, hiding its line.
        b.toggle_fold();
        assert_eq!(b.rows().count(), 2);
    }

    #[test]
    fn folding_the_section_hides_its_files() {
        let mut b = buffer(section_with_two_files(), 10);
        assert_eq!(b.rows().count(), 3);
        b.toggle_fold();
        assert_eq!(b.rows().count(), 1);
        b.toggle_fold();
        assert_eq!(b.rows().count(), 3);
    }

    #[test]
    fn fold_state_survives_a_rebuild_with_the_same_ids() {
        let mut b = buffer(section_with_two_files(), 10);
        b.toggle_fold();
        assert_eq!(b.rows().count(), 1);
        b.set_content(section_with_two_files());
        assert_eq!(b.rows().count(), 1, "fold persists by section id");
    }

    #[test]
    fn cursor_clamps_to_visible_rows() {
        let mut b = buffer(section_with_two_files(), 10);
        b.move_cursor(100);
        assert_eq!(b.cursor(), 2);
        b.move_cursor(-100);
        assert_eq!(b.cursor(), 0);
    }

    fn hunk_tree() -> Vec<Section> {
        let target = Target::Hunk {
            path: "a".into(),
            new_start: 1,
            line: 1,
            staged: Some(false),
        };
        let lines = (0..3)
            .map(|j| {
                Section::leaf(
                    format!("unstaged/a#0/{j}"),
                    NodeKind::DiffLine,
                    vec![Span::plain("x")],
                )
                .with_target(target.clone())
            })
            .collect();
        let hunk = Section::branch(
            "unstaged/a#0",
            NodeKind::Hunk,
            vec![Span::plain("@@")],
            lines,
        )
        .with_target(target);
        let file = Section::branch(
            "unstaged/a",
            NodeKind::File,
            vec![Span::plain("a")],
            vec![hunk],
        );
        vec![Section::branch(
            "unstaged",
            NodeKind::Section,
            vec![Span::plain("Unstaged")],
            vec![file],
        )]
    }

    #[test]
    fn region_selects_diff_lines_within_a_hunk() {
        // rows: 0 section, 1 file, 2 hunk, 3..6 the three diff lines
        let mut b = buffer(hunk_tree(), 10);
        b.move_cursor(3); // first diff line
        b.toggle_selection();
        b.move_cursor(1); // extend to the second diff line

        let sel = b.line_selection().expect("a line selection");
        assert_eq!(sel.path, "a");
        assert_eq!(sel.new_start, 1);
        assert!(!sel.staged);
        assert_eq!(sel.lines, vec![0, 1]);

        b.toggle_selection();
        assert!(b.line_selection().is_none(), "cleared");
    }

    #[test]
    fn scroll_follows_the_cursor_into_view() {
        let content: Vec<Section> = (0..6).map(|i| leaf(&format!("f{i}"))).collect();
        let mut b = buffer(content, 2);
        assert_eq!(b.scroll(), 0);
        b.move_cursor(5);
        assert_eq!(b.cursor(), 5);
        assert_eq!(b.scroll(), 4, "cursor stays within the 2-row viewport");
    }
}
