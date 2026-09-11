use std::sync::OnceLock;

use rgit_git::{
    BlameLine, CommitDetails, CommitRef, Deco, DiffLine, FileDiff, Head, Hunk, LineOrigin, LogEntry,
    RefEntry, RefKind, Remote, RepoStatus, StatusCode, StatusEntry, Worktree, group_decorations,
};
use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color as SynColor, StyleModifier, Theme as SynTheme, ThemeItem, ThemeSet, ThemeSettings,
};
use syntect::parsing::{SyntaxReference, SyntaxSet};

fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    // two-face bundles bat's full syntax set (hundreds of languages), the same
    // broad coverage the web viewer uses - far more than syntect's defaults.
    SET.get_or_init(two_face::syntax::extra_newlines)
}

/// RGB colors the host theme maps onto syntax roles, so diff highlighting uses
/// the active palette instead of a fixed bundled theme.
#[derive(Debug, Clone, Copy)]
pub struct SyntaxColors {
    pub fg: (u8, u8, u8),
    pub comment: (u8, u8, u8),
    pub keyword: (u8, u8, u8),
    pub string: (u8, u8, u8),
    pub number: (u8, u8, u8),
    pub function: (u8, u8, u8),
    pub type_name: (u8, u8, u8),
}

static SYNTAX_COLORS: OnceLock<SyntaxColors> = OnceLock::new();

/// Install the syntax palette (from the host theme) before the first render.
pub fn set_syntax_colors(colors: SyntaxColors) {
    let _ = SYNTAX_COLORS.set(colors);
}

fn syn_theme() -> &'static SynTheme {
    static THEME: OnceLock<SynTheme> = OnceLock::new();
    THEME.get_or_init(|| match SYNTAX_COLORS.get() {
        Some(c) => palette_theme(c),
        None => ThemeSet::load_defaults().themes["base16-ocean.dark"].clone(),
    })
}

/// A minimal syntect theme that colors the common scopes from the host palette.
fn palette_theme(c: &SyntaxColors) -> SynTheme {
    use std::str::FromStr;
    let col = |(r, g, b): (u8, u8, u8)| SynColor { r, g, b, a: 0xff };
    let item = |scope: &str, color: (u8, u8, u8)| ThemeItem {
        scope: syntect::highlighting::ScopeSelectors::from_str(scope).unwrap_or_default(),
        style: StyleModifier {
            foreground: Some(col(color)),
            background: None,
            font_style: None,
        },
    };
    SynTheme {
        name: Some("rgit".to_owned()),
        settings: ThemeSettings {
            foreground: Some(col(c.fg)),
            ..ThemeSettings::default()
        },
        scopes: vec![
            item("comment, punctuation.definition.comment", c.comment),
            item(
                "keyword, storage, storage.type, storage.modifier, keyword.operator",
                c.keyword,
            ),
            item("string, string.quoted, constant.character", c.string),
            item("constant.numeric, constant.language, constant", c.number),
            item(
                "entity.name.function, support.function, meta.function-call, variable.function",
                c.function,
            ),
            item(
                "entity.name.type, entity.name.class, support.type, support.class",
                c.type_name,
            ),
        ],
        ..SynTheme::default()
    }
}

fn ext_of(path: &str) -> Option<&str> {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
}

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

static SIDE_BY_SIDE: AtomicBool = AtomicBool::new(false);
static GLYPHS: AtomicU8 = AtomicU8::new(GlyphMode::Unicode as u8);

/// How much of the glyph palette to draw: pure ASCII, unicode symbols, or
/// unicode plus nerd-font icons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphMode {
    Ascii = 0,
    Unicode = 1,
    Nerd = 2,
}

/// Toggle side-by-side rendering of read-only diffs (commit / diff views).
pub fn set_side_by_side(on: bool) {
    SIDE_BY_SIDE.store(on, Ordering::Relaxed);
}

fn side_by_side() -> bool {
    SIDE_BY_SIDE.load(Ordering::Relaxed)
}

/// Install the glyph mode (ASCII / unicode / nerd) before the first render.
pub fn set_glyph_mode(mode: GlyphMode) {
    GLYPHS.store(mode as u8, Ordering::Relaxed);
}

pub fn glyph_mode() -> GlyphMode {
    match GLYPHS.load(Ordering::Relaxed) {
        0 => GlyphMode::Ascii,
        2 => GlyphMode::Nerd,
        _ => GlyphMode::Unicode,
    }
}

/// Whether nerd-font icons should be drawn (only in nerd mode).
fn nerd_fonts() -> bool {
    glyph_mode() == GlyphMode::Nerd
}

/// Whether unicode symbols (chevrons, bars, arrows) are allowed (not ASCII mode).
pub fn unicode() -> bool {
    glyph_mode() != GlyphMode::Ascii
}

/// Pick a unicode glyph or its ASCII fallback for the current mode.
pub fn glyph(unicode_glyph: &'static str, ascii: &'static str) -> &'static str {
    if unicode() { unicode_glyph } else { ascii }
}

/// A leading icon span for a git entity, or `None` when nerd fonts are off.
fn icon_span(glyph: &'static str, color: Style) -> Option<Span> {
    nerd_fonts().then(|| Span::new(format!("{glyph} "), color))
}

/// A hunk header split into its `@@ … @@` range (accented) and trailing code
/// context (dim), e.g. `@@ -12,6 +12,9 @@ fn render`.
fn hunk_header_spans(header: &str) -> Vec<Span> {
    match header.match_indices("@@").nth(1) {
        Some((pos, _)) => vec![
            Span::new(header[..pos + 2].to_owned(), Style::Branch),
            Span::new(header[pos + 2..].to_owned(), Style::Dim),
        ],
        None => vec![Span::new(header.to_owned(), Style::Dim)],
    }
}

/// A nerd-font file-type icon for the path's extension (empty when disabled).
/// Returns the glyph and its brand color.
fn file_icon(path: &str) -> Option<(&'static str, Style)> {
    if !nerd_fonts() {
        return None;
    }
    let rgb = |r, g, b| Style::Rgb(r, g, b);
    let ext = ext_of(path).unwrap_or("");
    let name = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let (glyph, color) = match (name, ext) {
        (_, "rs") => ("\u{e7a8}", rgb(0xde, 0x62, 0x3e)), //
        (_, "md" | "markdown") => ("\u{e73e}", rgb(0x51, 0x9a, 0xba)),
        (_, "js" | "mjs" | "cjs") => ("\u{e781}", rgb(0xf1, 0xe0, 0x5a)),
        (_, "ts" | "tsx") => ("\u{e628}", rgb(0x3d, 0x7e, 0xc5)),
        (_, "py") => ("\u{e73c}", rgb(0xff, 0xd4, 0x3b)),
        (_, "go") => ("\u{e627}", rgb(0x00, 0xad, 0xd8)),
        (_, "toml") => ("\u{e6b2}", rgb(0x9c, 0x42, 0x21)),
        (_, "json") => ("\u{e60b}", rgb(0xcb, 0xcb, 0x41)),
        (_, "yml" | "yaml") => ("\u{e6a8}", rgb(0xcb, 0x17, 0x1e)),
        (_, "lock") => ("\u{f13e}", rgb(0x8a, 0x8a, 0x8a)),
        (_, "sh" | "bash" | "zsh") => ("\u{e795}", rgb(0x89, 0xe0, 0x51)),
        (_, "c" | "h") => ("\u{e61e}", rgb(0x59, 0x9e, 0xd4)),
        (_, "cpp" | "cc" | "hpp") => ("\u{e61d}", rgb(0x00, 0x59, 0x9c)),
        (_, "html") => ("\u{e736}", rgb(0xe4, 0x4d, 0x26)),
        (_, "css") => ("\u{e749}", rgb(0x56, 0x3d, 0x7c)),
        ("Dockerfile", _) => ("\u{e650}", rgb(0x38, 0x8f, 0xd6)),
        (n, _) if n.starts_with(".git") => ("\u{e702}", rgb(0xf1, 0x50, 0x2f)),
        _ => ("\u{f15b}", Style::Dim), // generic file
    };
    Some((glyph, color))
}

/// Per-side column width for side-by-side diffs (tuned for wide terminals).
const SXS_COL: usize = 56;

/// Render a hunk as two columns: old on the left, new on the right.
fn side_by_side_lines(hunk: &Hunk) -> Vec<Vec<Span>> {
    let lines = &hunk.lines;
    let mut old = hunk_old_start(&hunk.header);
    let mut new = hunk.new_start;
    let mut rows = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        match lines[i].origin {
            LineOrigin::Meta => i += 1,
            LineOrigin::Context => {
                let t = &lines[i].text;
                rows.push(sxs_row(
                    Some((old, t)),
                    Some((new, t)),
                    Style::Dim,
                    Style::Dim,
                ));
                old += 1;
                new += 1;
                i += 1;
            }
            LineOrigin::Removed | LineOrigin::Added => {
                let mut removed = Vec::new();
                let mut added = Vec::new();
                while i < lines.len() && lines[i].origin == LineOrigin::Removed {
                    removed.push(lines[i].text.as_str());
                    i += 1;
                }
                while i < lines.len() && lines[i].origin == LineOrigin::Added {
                    added.push(lines[i].text.as_str());
                    i += 1;
                }
                for k in 0..removed.len().max(added.len()) {
                    let left = removed.get(k).map(|t| {
                        let n = old;
                        old += 1;
                        (n, *t)
                    });
                    let right = added.get(k).map(|t| {
                        let n = new;
                        new += 1;
                        (n, *t)
                    });
                    rows.push(sxs_row(left, right, Style::Deleted, Style::Added));
                }
            }
        }
    }
    rows
}

/// One side-by-side row: `num text | num text`, each side padded to a column.
fn sxs_row(
    left: Option<(u32, &str)>,
    right: Option<(u32, &str)>,
    left_style: Style,
    right_style: Style,
) -> Vec<Span> {
    let cell = |side: Option<(u32, &str)>| -> (String, String) {
        match side {
            Some((n, t)) => (format!("{n:>4} "), pad_trunc(t, SXS_COL)),
            None => ("     ".to_owned(), " ".repeat(SXS_COL)),
        }
    };
    let (lnum, ltext) = cell(left);
    let (rnum, rtext) = cell(right);
    vec![
        Span::new(lnum, Style::Dim),
        Span::new(ltext, left_style),
        Span::new(" │ ", Style::Dim),
        Span::new(rnum, Style::Dim),
        Span::new(rtext, right_style),
    ]
}

/// Truncate to `width` chars and pad to that width.
fn pad_trunc(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count > width {
        text.chars().take(width).collect()
    } else {
        let mut s = text.to_owned();
        s.push_str(&" ".repeat(width - count));
        s
    }
}

/// Build the refs view: branches, remotes, and tags in labeled sections.
pub fn build_refs(refs: &[RefEntry]) -> Vec<Section> {
    [
        (RefKind::Local, "refs/local", "Branches"),
        (RefKind::Remote, "refs/remote", "Remotes"),
        (RefKind::Tag, "refs/tag", "Tags"),
    ]
    .into_iter()
    .filter_map(|(kind, id, title)| ref_section(refs, kind, id, title))
    .collect()
}

fn ref_section(refs: &[RefEntry], kind: RefKind, id: &str, title: &str) -> Option<Section> {
    let items: Vec<&RefEntry> = refs.iter().filter(|r| r.kind == kind).collect();
    if items.is_empty() {
        return None;
    }
    let nodes = items
        .iter()
        .map(|r| {
            let marker = if r.is_head {
                Span::new("* ", Style::Added)
            } else {
                Span::plain("  ")
            };
            let glyph = match kind {
                RefKind::Local => "\u{e725}",  // branch
                RefKind::Remote => "\u{f0c2}", // cloud
                RefKind::Tag => "\u{f02b}",    // tag
            };
            let mut spans = vec![marker];
            spans.extend(icon_span(glyph, Style::Branch));
            spans.push(Span::new(r.name.clone(), Style::Branch));
            Section::leaf(format!("{id}/{}", r.name), NodeKind::Commit, spans).with_target(
                Target::Ref {
                    name: r.name.clone(),
                    kind: match kind {
                        RefKind::Local => RefTarget::Local,
                        RefKind::Remote => RefTarget::Remote,
                        RefKind::Tag => RefTarget::Tag,
                    },
                },
            )
        })
        .collect();
    Some(Section::branch(
        id,
        NodeKind::Section,
        vec![Span::new(
            format!("{title} ({})", items.len()),
            Style::SectionHeader,
        )],
        nodes,
    ))
}

/// Build the remotes view: each configured remote with its fetch URL.
pub fn build_remotes(remotes: &[Remote]) -> Vec<Section> {
    if remotes.is_empty() {
        return vec![Section::leaf(
            "remotes/empty",
            NodeKind::DiffLine,
            vec![Span::new("no remotes configured".to_owned(), Style::Dim)],
        )];
    }
    remotes
        .iter()
        .map(|r| {
            Section::leaf(
                format!("remotes/{}", r.name),
                NodeKind::Commit,
                vec![
                    Span::new(format!("{:<12}", r.name), Style::Branch),
                    Span::new(r.url.clone(), Style::Dim),
                ],
            )
        })
        .collect()
}

/// Build the worktrees view: each linked worktree with its path.
pub fn build_worktrees(worktrees: &[Worktree]) -> Vec<Section> {
    if worktrees.is_empty() {
        return vec![Section::leaf(
            "worktrees/empty",
            NodeKind::DiffLine,
            vec![Span::new("no linked worktrees".to_owned(), Style::Dim)],
        )];
    }
    worktrees
        .iter()
        .map(|w| {
            let mut spans = vec![Span::new(format!("{:<18}", w.name), Style::Branch)];
            // branch @ short-head, then flags, then the path.
            match (&w.branch, &w.head) {
                (Some(b), Some(h)) => {
                    spans.push(Span::new(format!("{b} "), Style::Branch));
                    spans.push(Span::new(format!("{h}  "), Style::Hash));
                }
                (None, Some(h)) => spans.push(Span::new(format!("detached {h}  "), Style::Hash)),
                _ => spans.push(Span::new("(unborn)  ".to_owned(), Style::Dim)),
            }
            if w.dirty {
                spans.push(Span::new("\u{25cf} dirty  ".to_owned(), Style::Modified));
            }
            if w.locked {
                spans.push(Span::new("\u{f023} locked  ".to_owned(), Style::Deleted));
            }
            spans.push(Span::new(w.path.clone(), Style::Dim));
            Section::leaf(format!("worktrees/{}", w.name), NodeKind::Commit, spans).with_target(
                Target::Worktree {
                    name: w.name.clone(),
                    path: w.path.clone(),
                },
            )
        })
        .collect()
}

/// Build the blame view: each file line prefixed with its commit and author.
pub fn build_blame(path: &str, lines: &[BlameLine]) -> Vec<Section> {
    // A never-committed file (newly added, staged or not) has no blame at all,
    // so the hash/author gutter would be blank on every row - just wasted space.
    // Drop it and show the code alone in that case.
    let has_blame = lines.iter().any(|bl| !bl.short_id.is_empty());
    lines
        .iter()
        .enumerate()
        .map(|(i, bl)| {
            let mut spans = Vec::new();
            if has_blame {
                let author: String = bl.author.chars().take(12).collect();
                spans.push(Span::new(format!("{:>7} ", bl.short_id), Style::Hash));
                spans.push(Span::new(format!("{author:<12} "), Style::Dim));
            }
            spans.extend(highlight_code(path, &bl.line));
            Section::leaf(format!("blame/{i}"), NodeKind::DiffLine, spans)
        })
        .collect()
}

use crate::content::{NodeKind, RefTarget, Section, Target};
use crate::style::{Span, Style};

/// Build the log view's content: a node row per commit, plus a graph link row
/// wherever the history forks or merges, so branch topology reads at a glance.
/// Decorations for a log row, git --decorate style: the local branch, its
/// upstream, then tags, each styled by kind so `<hash> <branch> <upstream>
/// <summary>` reads at a glance.
fn ref_labels(refs: &[CommitRef]) -> Vec<Span> {
    let mut spans = Vec::new();
    for deco in group_decorations(refs) {
        match deco {
            Deco::Local(name) => spans.push(Span::new(format!("  {name}"), Style::Branch)),
            Deco::Remote { remote, branch } => {
                spans.push(Span::new(format!("  {remote}/{branch}"), Style::Dim))
            }
            Deco::Tag(name) => spans.push(Span::new(format!("  {name}"), Style::Modified)),
            Deco::Group {
                branch,
                local,
                remotes,
            } => {
                spans.push(Span::new("  {".to_owned(), Style::Dim));
                let mut first = true;
                if local {
                    spans.push(Span::new("local".to_owned(), Style::Branch));
                    first = false;
                }
                for r in remotes {
                    if !first {
                        spans.push(Span::new(",".to_owned(), Style::Dim));
                    }
                    spans.push(Span::new(r, Style::Dim));
                    first = false;
                }
                let name_style = if local { Style::Branch } else { Style::Dim };
                spans.push(Span::new(format!("}}/{branch}"), name_style));
            }
        }
    }
    spans
}

pub fn build_log(entries: &[LogEntry]) -> Vec<Section> {
    if entries.is_empty() {
        let bullet = if nerd_fonts() {
            "\u{e729}  "
        } else {
            glyph("○ ", "o ")
        };
        return vec![
            Section::leaf(
                "log/empty",
                NodeKind::Info,
                vec![
                    Span::new(bullet, Style::Dim),
                    Span::new("no commits yet", Style::Dim),
                ],
            ),
            Section::leaf(
                "log/empty/hint",
                NodeKind::Info,
                vec![Span::new(
                    "  make the first commit to start the history",
                    Style::Dim,
                )],
            ),
        ];
    }
    let mut sections = Vec::new();
    for row in log_graph(entries) {
        match row.entry {
            Some(idx) => {
                let e = &entries[idx];
                let mut spans = row.spans;
                spans.push(Span::new(e.short_id.clone(), Style::Hash));
                for span in ref_labels(&e.refs) {
                    spans.push(span);
                }
                spans.push(Span::plain(format!("  {}", e.summary)));
                spans.push(Span::new(
                    format!("  {} · {}", e.author, e.when),
                    Style::Dim,
                ));
                if e.unpushed {
                    spans.push(Span::new("  \u{2191}".to_owned(), Style::Added));
                }
                sections.push(
                    Section::leaf(format!("log/{}", e.short_id), NodeKind::Commit, spans)
                        .with_target(Target::Commit {
                            id: e.short_id.clone(),
                        }),
                );
            }
            // A connector-only row carries no commit and cannot be acted on.
            None => sections.push(Section::leaf(
                format!("log/link/{}", sections.len()),
                NodeKind::Info,
                row.spans,
            )),
        }
    }
    sections
}

/// One rendered graph row: its glyph spans and, for a commit row, the index of
/// the entry it belongs to (a link row has none).
struct GraphRow {
    spans: Vec<Span>,
    entry: Option<usize>,
}

/// Assign each commit a lane and render the DAG. Each lane is two columns wide
/// (glyph + gap) so link rows can route horizontal connectors: a node marks its
/// lane, merges fan extra parents out with `├─╮`, and a branch tip folding back
/// closes with `├─╯`.
fn log_graph(entries: &[LogEntry]) -> Vec<GraphRow> {
    let mut lanes: Vec<Option<String>> = Vec::new();
    let mut out = Vec::new();
    for (idx, e) in entries.iter().enumerate() {
        let col = lanes
            .iter()
            .position(|l| l.as_deref() == Some(e.oid.as_str()))
            .unwrap_or_else(|| {
                lanes.push(Some(e.oid.clone()));
                lanes.len() - 1
            });
        // Any other lane awaiting this same commit folds into `col`.
        let collapses: Vec<usize> = lanes
            .iter()
            .enumerate()
            .filter(|(i, l)| *i != col && l.as_deref() == Some(e.oid.as_str()))
            .map(|(i, _)| i)
            .collect();
        let is_merge = e.parents.len() >= 2;

        out.push(GraphRow {
            spans: node_row(&lanes, col, is_merge),
            entry: Some(idx),
        });

        // Advance the frontier: fold the collapsed lanes, keep the first parent
        // in `col`, and fan the rest out to their own lanes.
        for &j in &collapses {
            lanes[j] = None;
        }
        let mut branches: Vec<usize> = Vec::new();
        for parent in e.parents.iter().skip(1) {
            let j = lanes
                .iter()
                .position(|l| l.as_deref() == Some(parent.as_str()))
                .unwrap_or_else(|| match lanes.iter().position(|l| l.is_none()) {
                    Some(j) => {
                        lanes[j] = Some(parent.clone());
                        j
                    }
                    None => {
                        lanes.push(Some(parent.clone()));
                        lanes.len() - 1
                    }
                });
            branches.push(j);
        }
        lanes[col] = e.parents.first().cloned();

        if !collapses.is_empty() || !branches.is_empty() {
            out.push(GraphRow {
                spans: link_row(&lanes, col, &collapses, &branches),
                entry: None,
            });
        }
        while matches!(lanes.last(), Some(None)) {
            lanes.pop();
        }
    }
    out
}

/// The commit row: each lane's glyph over its two-column cell.
fn node_row(lanes: &[Option<String>], col: usize, is_merge: bool) -> Vec<Span> {
    let mut spans = Vec::with_capacity(lanes.len() * 2 + 1);
    for (i, lane) in lanes.iter().enumerate() {
        let glyph = if i == col {
            if is_merge { "◆" } else { "●" }
        } else if lane.is_some() {
            "│"
        } else {
            " "
        };
        spans.push(Span::new(glyph.to_owned(), lane_style(i)));
        spans.push(Span::new(" ".to_owned(), lane_style(i)));
    }
    spans.push(Span::plain(" "));
    spans
}

/// The connector row between two commits: straight bars for lanes that pass
/// through, with `╮`/`╯` ends and a horizontal run linking `col` to each branch
/// or collapse column (crossings become `┼`).
fn link_row(
    lanes: &[Option<String>],
    col: usize,
    collapses: &[usize],
    branches: &[usize],
) -> Vec<Span> {
    // (column, end glyph): collapses fold in, branches fan out.
    let mut ends: Vec<(usize, char)> = Vec::new();
    for &j in collapses {
        ends.push((j, if j > col { '╯' } else { '╰' }));
    }
    for &t in branches {
        ends.push((t, if t > col { '╮' } else { '╭' }));
    }
    let width = lanes
        .len()
        .max(col + 1)
        .max(ends.iter().map(|(c, _)| c + 1).max().unwrap_or(0));
    let mut grid = vec![' '; width * 2];
    // Lanes that continue past this row are straight bars.
    for (i, lane) in lanes.iter().enumerate() {
        if lane.is_some() {
            grid[i * 2] = '│';
        }
    }
    let left = ends.iter().map(|(c, _)| *c).min().unwrap_or(col).min(col);
    let right = ends.iter().map(|(c, _)| *c).max().unwrap_or(col).max(col);
    for cell in grid.iter_mut().take(right * 2).skip(left * 2 + 1) {
        *cell = if *cell == '│' { '┼' } else { '─' };
    }
    for (c, ch) in &ends {
        grid[c * 2] = *ch;
    }
    let has_left = ends.iter().any(|(c, _)| *c < col);
    let has_right = ends.iter().any(|(c, _)| *c > col);
    let down = lanes[col].is_some();
    grid[col * 2] = match (has_left, has_right, down) {
        (true, true, _) => '┼',
        (true, false, true) => '┤',
        (false, true, true) => '├',
        (true, false, false) => '╯',
        (false, true, false) => '╭',
        _ => grid[col * 2],
    };
    let mut spans: Vec<Span> = grid
        .into_iter()
        .enumerate()
        .map(|(x, ch)| Span::new(ch.to_string(), lane_style(x / 2)))
        .collect();
    spans.push(Span::plain(" "));
    spans
}

fn lane_style(col: usize) -> Style {
    match col % 5 {
        0 => Style::Branch,
        1 => Style::Added,
        2 => Style::Modified,
        3 => Style::Deleted,
        _ => Style::Hash,
    }
}

/// Build the commit-detail view: header fields, message, and the diff.
/// `" <email>"` for the author line, or empty when the commit has no email.
fn author_email(details: &CommitDetails) -> String {
    if details.email.is_empty() {
        String::new()
    } else {
        format!("  <{}>", details.email)
    }
}

pub fn build_commit(details: &CommitDetails) -> Vec<Section> {
    let mut sections = vec![
        Section::leaf(
            "commit/id",
            NodeKind::HeadField,
            vec![
                Span::new("Commit:  ", Style::FieldLabel),
                Span::new(details.full_id.clone(), Style::Hash),
            ],
        ),
        Section::leaf(
            "commit/author",
            NodeKind::HeadField,
            vec![
                Span::new("Author:  ", Style::FieldLabel),
                Span::plain(details.author.clone()),
                Span::new(author_email(details), Style::Dim),
                Span::new(format!("  · {}", details.when), Style::Dim),
            ],
        ),
    ];
    for (i, line) in details.message.lines().enumerate() {
        sections.push(Section::leaf(
            format!("commit/msg/{i}"),
            NodeKind::Info,
            vec![Span::plain(line.to_owned())],
        ));
    }
    for (fi, file) in details.files.iter().enumerate() {
        let file_id = format!("commit/file/{fi}");
        let hunks = file
            .hunks
            .iter()
            .enumerate()
            .map(|(hi, hunk)| hunk_node(&file_id, &file.path, hi, hunk, ext_of(&file.path), HunkCtx::Historical))
            .collect();
        sections.push(Section::branch(
            file_id,
            NodeKind::File,
            diff_file_header(file),
            hunks,
        ));
    }
    sections
}

/// A diff file header: `old -> new` when renamed or copied, else just the path.
fn diff_file_header(file: &FileDiff) -> Vec<Span> {
    match &file.old_path {
        Some(old) => vec![
            Span::new(format!("{old} -> "), Style::Dim),
            Span::new(file.path.clone(), Style::Modified),
        ],
        None => vec![Span::new(file.path.clone(), Style::Modified)],
    }
}

/// Build a read-only diff view between two revisions: a header plus one
/// foldable section per changed file.
pub fn build_diff(title: &str, files: &[FileDiff]) -> Vec<Section> {
    let mut sections = vec![Section::leaf(
        "diff/title",
        NodeKind::HeadField,
        vec![Span::new(title.to_owned(), Style::FieldLabel)],
    )];
    if files.is_empty() {
        sections.push(Section::leaf(
            "diff/empty",
            NodeKind::Info,
            vec![Span::new("no differences".to_owned(), Style::Dim)],
        ));
        return sections;
    }
    for (fi, file) in files.iter().enumerate() {
        let file_id = format!("diff/file/{fi}");
        let hunks = file
            .hunks
            .iter()
            .enumerate()
            .map(|(hi, hunk)| hunk_node(&file_id, &file.path, hi, hunk, ext_of(&file.path), HunkCtx::Historical))
            .collect();
        sections.push(Section::branch(
            file_id,
            NodeKind::File,
            diff_file_header(file),
            hunks,
        ));
    }
    sections
}

/// How a diff hunk can be acted on, chosen by the view that renders it. This is
/// what lets one renderer serve every diff view.
#[derive(Clone, Copy)]
enum HunkCtx {
    /// Working tree (status view): stageable; the editor opens the working file.
    Worktree { staged: bool },
    /// A commit, or a diff between revs: read-only, but still foldable and
    /// editor-openable (best effort - the file may have moved on).
    Historical,
}

impl HunkCtx {
    fn staged(self) -> Option<bool> {
        match self {
            HunkCtx::Worktree { staged } => Some(staged),
            HunkCtx::Historical => None,
        }
    }
}

/// Render one hunk as a foldable section: a header plus one leaf per diff line.
/// Every row carries a `Target::Hunk` with its own file line, so fold, Return,
/// click, and (for the working tree) staging behave identically in every view -
/// there is one diff component, not one per view.
fn hunk_node(
    file_id: &str,
    path: &str,
    index: usize,
    hunk: &Hunk,
    ext: Option<&str>,
    ctx: HunkCtx,
) -> Section {
    let hunk_id = format!("{file_id}#{index}");
    let target = |line: u32| Target::Hunk {
        path: path.to_owned(),
        new_start: hunk.new_start,
        line,
        staged: ctx.staged(),
    };

    // Side-by-side rows do not map 1:1 to source lines, so they resolve to the
    // hunk's start line; the default one-column layout gives each line exactly.
    // Only read-only views go side by side (staging needs the one-column form).
    let side_by_side = side_by_side() && matches!(ctx, HunkCtx::Historical);
    let lines = if side_by_side {
        side_by_side_lines(hunk)
            .into_iter()
            .enumerate()
            .map(|(j, spans)| {
                Section::leaf(format!("{hunk_id}/{j}"), NodeKind::DiffLine, spans)
                    .with_target(target(hunk.new_start))
            })
            .collect()
    } else {
        hunk_line_spans(hunk, ext)
            .into_iter()
            .enumerate()
            .map(|(j, spans)| {
                Section::leaf(format!("{hunk_id}/{j}"), NodeKind::DiffLine, spans)
                    .with_target(target(diff_line_file_line(hunk, j)))
            })
            .collect()
    };

    Section::branch(hunk_id, NodeKind::Hunk, hunk_header_spans(&hunk.header), lines)
        .with_target(target(hunk.new_start))
}

fn diff_line_spans(line: &DiffLine) -> Vec<Span> {
    let (prefix, style, bg) = match line.origin {
        LineOrigin::Added => ("+", Style::Added, Some(Style::AddedBg)),
        LineOrigin::Removed => ("-", Style::Deleted, Some(Style::DeletedBg)),
        LineOrigin::Context => (" ", Style::Plain, None),
        LineOrigin::Meta => (" ", Style::Dim, None),
    };
    let span = Span::new(format!("{prefix}{}", line.text), style);
    vec![match bg {
        Some(b) => span.with_bg(b),
        None => span,
    }]
}

/// The line-number gutter, washed with the add/remove background for changed
/// lines so the colored band starts at the left edge.
fn diff_gutter(old: Option<u32>, new: Option<u32>, origin: LineOrigin) -> Span {
    let g = gutter(old, new);
    match origin {
        LineOrigin::Added => g.with_bg(Style::AddedBg),
        LineOrigin::Removed => g.with_bg(Style::DeletedBg),
        _ => g,
    }
}

/// Per-line spans for a hunk: an old/new line-number gutter plus word-level
/// highlighting on a single removed line immediately followed by a single added
/// one. One output entry per source line, so callers can still index for
/// staging.
/// Above this many lines a hunk is left unhighlighted; per-line syntect is too
/// costly on huge diffs (e.g. lockfiles).
const MAX_SYNTAX_LINES: usize = 400;

/// The width of the diff line-number gutter (`{:>4} {:>4} `), so a caller can
/// map a caret column on a rendered diff line back to a file column.
pub const DIFF_GUTTER_COLS: usize = 10;

/// The new-side file line number (1-based) of the diff line at `source_index`
/// within `hunk` - what an editor should open at. A deleted line has no new-side
/// number, so it resolves to the following new-side line (where it was removed).
pub fn diff_line_file_line(hunk: &Hunk, source_index: usize) -> u32 {
    let mut old = hunk_old_start(&hunk.header);
    let mut new = hunk.new_start;
    for (i, line) in hunk.lines.iter().enumerate() {
        if i == source_index {
            return new.max(1);
        }
        advance(line.origin, &mut old, &mut new);
    }
    new.max(1)
}

fn hunk_line_spans(hunk: &Hunk, ext: Option<&str>) -> Vec<Vec<Span>> {
    let syntax = ext
        .filter(|_| hunk.lines.len() <= MAX_SYNTAX_LINES)
        .and_then(|e| syntax_set().find_syntax_by_extension(e));
    let lines = &hunk.lines;
    let mut old = hunk_old_start(&hunk.header);
    let mut new = hunk.new_start;
    let mut out = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let cur = &lines[i];
        // Syntax highlighting and word-diff both claim the foreground channel,
        // so when a syntax is known it takes over and the add/remove wash carries
        // the change signal instead.
        if let Some(syntax) = syntax {
            let (o, n) = line_numbers(cur.origin, old, new);
            let mut spans = vec![diff_gutter(o, n, cur.origin)];
            spans.extend(syntax_spans(cur, syntax));
            out.push(spans);
            advance(cur.origin, &mut old, &mut new);
            i += 1;
            continue;
        }
        let paired = lines
            .get(i + 1)
            .filter(|n| cur.origin == LineOrigin::Removed && n.origin == LineOrigin::Added);
        if let Some(added) = paired {
            let (p, s) = changed_span(&cur.text, &added.text);
            let mut removed = vec![diff_gutter(Some(old), None, LineOrigin::Removed)];
            removed.extend(
                worded_line("-", &cur.text, p, s, Style::Deleted, Style::WordDeleted)
                    .into_iter()
                    .map(|sp| sp.with_bg(Style::DeletedBg)),
            );
            let mut addition = vec![diff_gutter(None, Some(new), LineOrigin::Added)];
            addition.extend(
                worded_line("+", &added.text, p, s, Style::Added, Style::WordAdded)
                    .into_iter()
                    .map(|sp| sp.with_bg(Style::AddedBg)),
            );
            out.push(removed);
            out.push(addition);
            old += 1;
            new += 1;
            i += 2;
        } else {
            let (o, n) = match cur.origin {
                LineOrigin::Context => (Some(old), Some(new)),
                LineOrigin::Added => (None, Some(new)),
                LineOrigin::Removed => (Some(old), None),
                LineOrigin::Meta => (None, None),
            };
            let mut spans = vec![diff_gutter(o, n, cur.origin)];
            spans.extend(diff_line_spans(cur));
            out.push(spans);
            match cur.origin {
                LineOrigin::Context => {
                    old += 1;
                    new += 1;
                }
                LineOrigin::Added => new += 1,
                LineOrigin::Removed => old += 1,
                LineOrigin::Meta => {}
            }
            i += 1;
        }
    }
    out
}

/// Old/new line numbers to show for a line of the given origin.
fn line_numbers(origin: LineOrigin, old: u32, new: u32) -> (Option<u32>, Option<u32>) {
    match origin {
        LineOrigin::Context => (Some(old), Some(new)),
        LineOrigin::Added => (None, Some(new)),
        LineOrigin::Removed => (Some(old), None),
        LineOrigin::Meta => (None, None),
    }
}

/// Advance the running old/new counters past a line of the given origin.
fn advance(origin: LineOrigin, old: &mut u32, new: &mut u32) {
    match origin {
        LineOrigin::Context => {
            *old += 1;
            *new += 1;
        }
        LineOrigin::Added => *new += 1,
        LineOrigin::Removed => *old += 1,
        LineOrigin::Meta => {}
    }
}

/// A syntax-highlighted diff line: the marker, then per-token colors, all over
/// an added/removed background wash.
fn syntax_spans(line: &DiffLine, syntax: &SyntaxReference) -> Vec<Span> {
    let (marker, marker_style, bg) = match line.origin {
        LineOrigin::Added => ("+", Style::Added, Some(Style::AddedBg)),
        LineOrigin::Removed => ("-", Style::Deleted, Some(Style::DeletedBg)),
        LineOrigin::Context => (" ", Style::Dim, None),
        LineOrigin::Meta => (" ", Style::Dim, None),
    };
    let washed = |span: Span| match bg {
        Some(b) => span.with_bg(b),
        None => span,
    };
    let mut spans = vec![washed(Span::new(marker, marker_style))];

    let mut highlighter = HighlightLines::new(syntax, syn_theme());
    match highlighter.highlight_line(&line.text, syntax_set()) {
        Ok(ranges) => {
            for (token, text) in ranges {
                let c = token.foreground;
                spans.push(washed(Span::new(
                    text.to_owned(),
                    Style::Rgb(c.r, c.g, c.b),
                )));
            }
        }
        Err(_) => spans.push(washed(Span::plain(line.text.clone()))),
    }
    spans
}

/// The syntax for `path`, by extension, or plain text when unknown.
fn syntax_for_path(path: &str) -> &'static SyntaxReference {
    let ss = syntax_set();
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|e| ss.find_syntax_by_extension(e))
        .unwrap_or_else(|| ss.find_syntax_plain_text())
}

/// One line of highlighted spans from a `HighlightLines` result, using the host
/// syntax palette; a highlight failure falls back to a plain span.
fn highlighted_spans(h: &mut HighlightLines, line: &str) -> Vec<Span> {
    match h.highlight_line(line, syntax_set()) {
        Ok(ranges) => ranges
            .into_iter()
            .map(|(t, s)| {
                let c = t.foreground;
                Span::new(s.to_owned(), Style::Rgb(c.r, c.g, c.b))
            })
            .collect(),
        Err(_) => vec![Span::plain(line.to_owned())],
    }
}

/// Highlight one isolated line of code from `path`'s language (no cross-line
/// state), for per-line contexts like blame.
pub fn highlight_code(path: &str, line: &str) -> Vec<Span> {
    let mut h = HighlightLines::new(syntax_for_path(path), syn_theme());
    highlighted_spans(&mut h, line)
}

/// Highlight a whole file into one span-run per line. Highlighting from the top
/// keeps multi-line constructs (strings, block comments) correct even when the
/// caller only shows a window.
pub fn highlight_file(path: &str, text: &str) -> Vec<Vec<Span>> {
    let mut h = HighlightLines::new(syntax_for_path(path), syn_theme());
    text.lines().map(|line| highlighted_spans(&mut h, line)).collect()
}

/// The old-side start line parsed from a `@@ -old,c +new,c @@` header.
fn hunk_old_start(header: &str) -> u32 {
    header
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.strip_prefix('-'))
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
}

/// A dim old/new line-number gutter cell.
fn gutter(old: Option<u32>, new: Option<u32>) -> Span {
    let fmt = |n: Option<u32>| n.map(|n| n.to_string()).unwrap_or_default();
    Span::new(format!("{:>4} {:>4} ", fmt(old), fmt(new)), Style::Dim)
}

/// The common prefix and suffix lengths (in chars) shared by two lines; the
/// range between them is what changed.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn changed_span(old: &str, new: &str) -> (usize, usize) {
    let a: Vec<char> = old.chars().collect();
    let b: Vec<char> = new.chars().collect();
    let mut p = 0;
    while p < a.len() && p < b.len() && a[p] == b[p] {
        p += 1;
    }
    let mut s = 0;
    while s < a.len() - p && s < b.len() - p && a[a.len() - 1 - s] == b[b.len() - 1 - s] {
        s += 1;
    }
    // Snap the boundaries to whole words. When the common prefix ends inside a
    // word (a word char on both sides of the split), retract it to the word
    // start; likewise pull the suffix off a mid-word split. This highlights the
    // whole changed token instead of leaving a ragged one- or two-char remnant.
    while p > 0 && p < a.len() && is_word_char(a[p - 1]) && is_word_char(a[p]) {
        p -= 1;
    }
    let alen = a.len();
    while s > 0 && alen - s > p && is_word_char(a[alen - s]) && is_word_char(a[alen - s - 1]) {
        s -= 1;
    }
    (p, s)
}

/// Split `text` into unchanged prefix/suffix (`base` style) and a changed middle
/// (`word` style), prefixed by the diff marker.
fn worded_line(
    marker: &str,
    text: &str,
    prefix: usize,
    suffix: usize,
    base: Style,
    word: Style,
) -> Vec<Span> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let suffix = suffix.min(n - prefix.min(n));
    let mid_end = n - suffix;
    let pre: String = chars[..prefix.min(n)].iter().collect();
    let mid: String = chars[prefix.min(n)..mid_end].iter().collect();
    let suf: String = chars[mid_end..].iter().collect();
    let mut spans = vec![Span::new(format!("{marker}{pre}"), base)];
    if !mid.is_empty() {
        spans.push(Span::new(mid, word));
    }
    if !suf.is_empty() {
        spans.push(Span::new(suf, base));
    }
    spans
}

/// Build the status buffer's content tree from a repository snapshot. Section
/// ids are derived from the git model so fold state survives a refresh.
pub fn build(status: &RepoStatus) -> Vec<Section> {
    // A magit-style Head/Push header opens the buffer so branch, tip and remote
    // tracking state are visible at a glance; the title bar mirrors the branch.
    let mut sections: Vec<Section> = Vec::new();

    let untracked: Vec<&StatusEntry> = status.entries.iter().filter(|e| e.is_untracked()).collect();
    let unstaged: Vec<&StatusEntry> = status.entries.iter().filter(|e| e.is_unstaged()).collect();
    let staged: Vec<&StatusEntry> = status.entries.iter().filter(|e| e.is_staged()).collect();

    if !untracked.is_empty() {
        sections.push(file_section(
            "untracked",
            "Untracked",
            &untracked,
            |e| e.worktree,
            false,
            // Untracked files now carry an all-added diff in the unstaged list,
            // so they fold open to show their contents like any other change.
            |p| status.unstaged_diff(p),
        ));
    }
    if !unstaged.is_empty() {
        sections.push(file_section(
            "unstaged",
            "Unstaged",
            &unstaged,
            |e| e.worktree,
            false,
            |p| status.unstaged_diff(p),
        ));
    }
    if !staged.is_empty() {
        sections.push(file_section(
            "staged",
            "Staged",
            &staged,
            |e| e.index,
            true,
            |p| status.staged_diff(p),
        ));
    }

    if untracked.is_empty() && unstaged.is_empty() && staged.is_empty() {
        let check = if nerd_fonts() {
            "\u{f00c}  "
        } else {
            glyph("✓ ", "+ ")
        };
        sections.push(Section::leaf(
            "info",
            NodeKind::Info,
            vec![
                Span::new(check, Style::Added),
                Span::new("working tree clean", Style::Dim),
            ],
        ));
        sections.push(Section::leaf(
            "info/hint",
            NodeKind::Info,
            vec![Span::new("  nothing to commit", Style::Dim)],
        ));
    }

    if !status.stashes.is_empty() {
        let entries = status
            .stashes
            .iter()
            .map(|s| {
                let mut spans = Vec::new();
                spans.extend(icon_span("\u{f01c}", Style::Modified));
                spans.push(Span::new(format!("stash@{{{}}}", s.index), Style::Hash));
                spans.push(Span::plain(format!("  {}", s.message)));
                Section::leaf(format!("stash/{}", s.index), NodeKind::Stash, spans)
                    .with_target(Target::Stash { index: s.index })
            })
            .collect();
        sections.push(Section::branch(
            "stashes",
            NodeKind::Section,
            section_header("Stashes", status.stashes.len()),
            entries,
        ));
    }

    if !status.recent.is_empty() {
        let commits = status
            .recent
            .iter()
            .map(|c| {
                let mut spans = Vec::new();
                spans.extend(icon_span("\u{e729}", Style::Hash));
                spans.push(Span::new(c.short_id.clone(), Style::Hash));
                for span in ref_labels(&c.refs) {
                    spans.push(span);
                }
                spans.push(Span::plain(format!("  {}", c.summary)));
                spans.push(Span::new(format!("  · {}", c.when), Style::Dim));
                if c.unpushed {
                    spans.push(Span::new("  \u{2191}unpushed".to_owned(), Style::Added));
                }
                Section::leaf(format!("recent/{}", c.short_id), NodeKind::Commit, spans)
                    .with_target(Target::Commit {
                        id: c.short_id.clone(),
                    })
            })
            .collect();
        sections.push(Section::branch(
            "recent",
            NodeKind::Section,
            vec![Span::new("RECENT".to_owned(), Style::Dim)],
            commits,
        ));
    }

    // Header first (tightly packed), then a gap, then the spaced change sections.
    let mut out = head_header(&status.head);
    if !sections.is_empty() {
        out.push(Section::leaf(
            "spacer/head",
            NodeKind::Info,
            vec![Span::plain("")],
        ));
    }
    out.extend(space_sections(sections));
    out
}

/// The Head/Push header: current branch and tip, then remote tracking state,
/// so publish status ("no upstream", ahead/behind) reads at a glance.
fn head_header(head: &Head) -> Vec<Section> {
    // A branch glyph precedes the name when nerd fonts are on.
    let branch = if nerd_fonts() { "\u{e725} " } else { "" };
    let mut head_line = vec![
        Span::new("Head:   ", Style::FieldLabel),
        Span::new(format!("{branch}{}", head.describe()), Style::Branch),
    ];
    if let Some(oid) = &head.oid {
        head_line.push(Span::new(format!("  {oid}"), Style::Hash));
    }
    if let Some(summary) = &head.summary {
        head_line.push(Span::new(format!("  {summary}"), Style::Dim));
    }
    if let Some(when) = &head.when {
        head_line.push(Span::new(format!("  · {when}"), Style::Dim));
    }
    let mut push_line = vec![Span::new("Push:   ", Style::FieldLabel)];
    match &head.upstream {
        Some(upstream) => {
            push_line.push(Span::new(upstream.clone(), Style::Branch));
            let divergence = match (head.ahead, head.behind) {
                (0, 0) => (" up to date".to_owned(), Style::Dim),
                (a, 0) => (format!("  {}{a}", glyph("↑", "+")), Style::Added),
                (0, b) => (format!("  {}{b}", glyph("↓", "-")), Style::Deleted),
                (a, b) => (
                    format!("  {}{a} {}{b}", glyph("↑", "+"), glyph("↓", "-")),
                    Style::Modified,
                ),
            };
            push_line.push(Span::new(divergence.0, divergence.1));
        }
        // No tracking branch: the work here has never been published upstream.
        None => push_line.push(Span::new("not published", Style::Untracked)),
    }
    vec![
        Section::leaf("head/branch", NodeKind::HeadField, head_line),
        Section::leaf("head/push", NodeKind::HeadField, push_line),
    ]
}

/// Put a blank spacer row between top-level sections for breathing room.
fn space_sections(sections: Vec<Section>) -> Vec<Section> {
    let mut spaced = Vec::with_capacity(sections.len() * 2);
    for (i, section) in sections.into_iter().enumerate() {
        if i > 0 {
            spaced.push(Section::leaf(
                format!("spacer/{i}"),
                NodeKind::Info,
                vec![Span::plain("")],
            ));
        }
        spaced.push(section);
    }
    spaced
}

fn file_section<'a>(
    id: &str,
    title: &str,
    entries: &[&StatusEntry],
    code: impl Fn(&StatusEntry) -> StatusCode,
    staged: bool,
    diff_of: impl Fn(&str) -> Option<&'a FileDiff>,
) -> Section {
    let files = entries
        .iter()
        .map(|e| {
            let c = code(e);
            let diff = diff_of(&e.path);
            let mut spans = vec![Span::new(format!("{}  ", c.letter()), code_style(c))];
            if let Some((glyph, color)) = file_icon(&e.path) {
                spans.push(Span::new(format!("{glyph} "), color));
            }
            if let Some(orig) = &e.orig_path {
                spans.push(Span::new(format!("{orig} -> "), Style::Dim));
            }
            spans.push(Span::plain(e.path.clone()));
            if let Some(d) = diff {
                spans.extend(diffstat_spans(d));
            }

            let file_id = format!("{id}/{}", e.path);
            let hunks = diff
                .map(|d| hunk_nodes(&file_id, &e.path, staged, d))
                .unwrap_or_default();
            Section::branch(file_id, NodeKind::File, spans, hunks)
                .with_target(Target::File {
                    path: e.path.clone(),
                    staged,
                })
                .folded_by_default()
        })
        .collect();

    let paths = entries.iter().map(|e| e.path.clone()).collect();
    Section::branch(
        id,
        NodeKind::Section,
        section_header(title, entries.len()),
        files,
    )
    .with_target(Target::Section { staged, paths })
}

/// A dim uppercase section title with an accent count, e.g. `UNSTAGED  3`.
fn section_header(title: &str, count: usize) -> Vec<Span> {
    vec![
        Span::new(format!("{}  ", title.to_uppercase()), Style::Dim),
        Span::new(count.to_string(), Style::SectionHeader),
    ]
}

fn code_style(code: StatusCode) -> Style {
    match code {
        StatusCode::Added => Style::Added,
        StatusCode::Deleted | StatusCode::Unmerged => Style::Deleted,
        StatusCode::Untracked => Style::Dim,
        _ => Style::Modified,
    }
}

/// A trailing `+added -removed` diffstat for a file, from its hunks.
fn diffstat_spans(diff: &FileDiff) -> Vec<Span> {
    let mut added = 0usize;
    let mut removed = 0usize;
    for hunk in &diff.hunks {
        for line in &hunk.lines {
            match line.origin {
                LineOrigin::Added => added += 1,
                LineOrigin::Removed => removed += 1,
                _ => {}
            }
        }
    }
    let mut spans = Vec::new();
    if added > 0 {
        spans.push(Span::new(format!("  +{added}"), Style::Added));
    }
    if removed > 0 {
        spans.push(Span::new(format!(" -{removed}"), Style::Deleted));
    }
    // A compact five-square bar showing the add/remove proportion (GitHub-style).
    let total = added + removed;
    if total > 0 {
        const N: usize = 5;
        let g = if added == 0 {
            0
        } else {
            ((added * N).div_ceil(total)).clamp(1, N)
        };
        spans.push(Span::plain("  "));
        if g > 0 {
            spans.push(Span::new(glyph("▰", "+").repeat(g), Style::Added));
        }
        if N - g > 0 {
            spans.push(Span::new(glyph("▰", "-").repeat(N - g), Style::Deleted));
        }
    }
    spans
}

fn hunk_nodes(file_id: &str, path: &str, staged: bool, diff: &FileDiff) -> Vec<Section> {
    diff.hunks
        .iter()
        .enumerate()
        .map(|(i, hunk)| {
            hunk_node(
                file_id,
                path,
                i,
                hunk,
                ext_of(path),
                HunkCtx::Worktree { staged },
            )
        })
        .collect()
}

#[cfg(test)]
mod hunk_component_tests {
    use super::{build, build_commit, Target};
    use rgit_git::{
        CommitDetails, DiffLine, FileDiff, Head, Hunk, LineOrigin, RepoStatus, StatusCode,
        StatusEntry,
    };

    fn one_hunk_file(path: &str) -> FileDiff {
        FileDiff {
            path: path.into(),
            old_path: None,
            binary: false,
            hunks: vec![Hunk {
                header: "@@ -1,1 +1,2 @@".into(),
                new_start: 1,
                lines: vec![
                    DiffLine {
                        origin: LineOrigin::Context,
                        text: "a".into(),
                    },
                    DiffLine {
                        origin: LineOrigin::Added,
                        text: "b".into(),
                    },
                ],
            }],
        }
    }

    fn first_hunk_line_target(sections: &[super::Section]) -> Option<Target> {
        // Walk into the first file -> first hunk -> first diff line.
        fn dig(s: &super::Section) -> Option<Target> {
            if s.kind == super::NodeKind::DiffLine {
                return s.target.clone();
            }
            s.children.iter().find_map(dig)
        }
        sections.iter().find_map(dig)
    }

    #[test]
    fn commit_and_status_diff_lines_share_the_targeted_component() {
        // The commit view (read-only) and the status view (working tree) now go
        // through one renderer, so both attach a Target::Hunk with a file line;
        // they differ only in `staged` (None vs Some).
        let commit = CommitDetails {
            id: "abc".into(),
            full_id: "abc".into(),
            author: "t".into(),
            email: "t".into(),
            when: "now".into(),
            message: "m".into(),
            files: vec![one_hunk_file("a.rs")],
        };
        let sections = build_commit(&commit);
        match first_hunk_line_target(&sections) {
            Some(Target::Hunk {
                staged: None, line, ..
            }) => assert!(line >= 1, "commit diff line carries a file line"),
            other => panic!("commit diff line should be a read-only Hunk target: {other:?}"),
        }

        // The status view: same component, but stageable (staged = Some).
        let status = RepoStatus {
            head: Head::default(),
            entries: vec![StatusEntry {
                path: "a.rs".into(),
                orig_path: None,
                index: StatusCode::Unmodified,
                worktree: StatusCode::Modified,
            }],
            unstaged: vec![one_hunk_file("a.rs")],
            ..Default::default()
        };
        let sections = build(&status);
        assert!(matches!(
            first_hunk_line_target(&sections),
            Some(Target::Hunk {
                staged: Some(false),
                ..
            })
        ));
    }
}

#[cfg(test)]
mod diff_line_tests {
    use super::diff_line_file_line;
    use rgit_git::{DiffLine, Hunk, LineOrigin};

    #[test]
    fn maps_a_source_index_to_its_new_side_file_line() {
        // @@ -10,3 +20,4 @@ : context, removed, added, context.
        let line = |origin, text: &str| DiffLine {
            origin,
            text: text.into(),
        };
        let hunk = Hunk {
            header: "@@ -10,3 +20,4 @@".into(),
            new_start: 20,
            lines: vec![
                line(LineOrigin::Context, "a"), // new line 20
                line(LineOrigin::Removed, "b"), // old only
                line(LineOrigin::Added, "c"),   // new line 21
                line(LineOrigin::Context, "d"), // new line 22
            ],
        };
        assert_eq!(diff_line_file_line(&hunk, 0), 20, "context = new_start");
        // A removed line has no new-side number, so it resolves to the next
        // new-side line (where the deletion lands in the working file).
        assert_eq!(diff_line_file_line(&hunk, 1), 21, "removed -> next new line");
        assert_eq!(diff_line_file_line(&hunk, 2), 21, "the added line");
        assert_eq!(diff_line_file_line(&hunk, 3), 22, "context after");
    }
}

#[cfg(test)]
mod graph_tests {
    use super::*;

    fn entry(oid: &str, parents: &[&str]) -> LogEntry {
        LogEntry {
            short_id: oid.into(),
            summary: String::new(),
            author: String::new(),
            when: String::new(),
            oid: oid.into(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            refs: Vec::new(),
            unpushed: false,
        }
    }

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn log_ref_labels_collapse_shared_branch_name() {
        let refs = vec![
            CommitRef {
                name: "main".into(),
                kind: RefKind::Local,
                head: true,
            },
            CommitRef {
                name: "origin/main".into(),
                kind: RefKind::Remote,
                head: false,
            },
            CommitRef {
                name: "upstream/main".into(),
                kind: RefKind::Remote,
                head: false,
            },
            CommitRef {
                name: "v1".into(),
                kind: RefKind::Tag,
                head: false,
            },
        ];
        let spans = ref_labels(&refs);
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "  {local,origin,upstream}/main  v1");
        assert!(
            spans
                .iter()
                .any(|s| s.text == "local" && s.style == Style::Branch)
        );
        assert!(
            spans
                .iter()
                .any(|s| s.text == "  v1" && s.style == Style::Modified)
        );
    }

    #[test]
    fn log_ref_labels_keep_unique_names_separate() {
        let refs = vec![
            CommitRef {
                name: "feature".into(),
                kind: RefKind::Local,
                head: true,
            },
            CommitRef {
                name: "origin/main".into(),
                kind: RefKind::Remote,
                head: false,
            },
        ];
        let joined: String = ref_labels(&refs)
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(joined, "  feature  origin/main");
    }

    #[test]
    fn word_diff_snaps_to_word_boundaries() {
        // A shared trailing letter must not leave a ragged remnant: the change
        // covers the whole word, not "bet"/"gamm" with a dangling "a".
        let (p, s) = changed_span("alpha beta", "alpha gamma");
        assert_eq!(&"alpha beta"[p.."alpha beta".len() - s], "beta");
        assert_eq!(&"alpha gamma"[p.."alpha gamma".len() - s], "gamma");

        // A non-word boundary (punctuation) is respected without snapping.
        let (p, s) = changed_span("a.b", "a.c");
        assert_eq!(&"a.b"[p.."a.b".len() - s], "b");
        assert_eq!(&"a.c"[p.."a.c".len() - s], "c");
    }

    #[test]
    fn word_diff_isolates_the_changed_run() {
        let (p, s) = changed_span("timeout = 30;", "timeout = 60;");
        let removed = worded_line(
            "-",
            "timeout = 30;",
            p,
            s,
            Style::Deleted,
            Style::WordDeleted,
        );
        let added = worded_line("+", "timeout = 60;", p, s, Style::Added, Style::WordAdded);
        // the whole changed token is word-styled on each side (boundaries snap
        // to the word, so "30"/"60" highlight as a unit, not just the digit)
        assert!(
            removed
                .iter()
                .any(|sp| sp.text == "30" && sp.style == Style::WordDeleted)
        );
        assert!(
            added
                .iter()
                .any(|sp| sp.text == "60" && sp.style == Style::WordAdded)
        );
        // unchanged text keeps the base style
        assert!(removed[0].text.starts_with("-timeout = "));
    }

    fn head(upstream: Option<&str>, ahead: usize, behind: usize) -> Head {
        Head {
            branch: Some("main".into()),
            detached: false,
            oid: Some("abc1234".into()),
            summary: Some("do a thing".into()),
            when: Some("2 hours ago".into()),
            upstream: upstream.map(str::to_owned),
            ahead,
            behind,
            remotes: Vec::new(),
        }
    }

    #[test]
    fn head_header_shows_tracking_state() {
        let rows = head_header(&head(Some("origin/main"), 2, 0));
        let push = text(&rows[1].spans);
        assert!(push.contains("origin/main"), "names the upstream: {push}");
        assert!(push.contains("↑2"), "shows divergence: {push}");
    }

    #[test]
    fn head_header_flags_unpublished_branch() {
        let rows = head_header(&head(None, 0, 0));
        assert!(text(&rows[1].spans).contains("not published"));
    }

    #[test]
    fn linear_history_is_a_single_lane() {
        let log = [entry("c", &["b"]), entry("b", &["a"]), entry("a", &[])];
        let rows = log_graph(&log);
        // No forks or merges: one node row per commit, no link rows.
        assert_eq!(rows.len(), 3);
        for row in &rows {
            assert!(row.entry.is_some());
            assert_eq!(text(&row.spans), "●  ", "one lane, node each row");
        }
    }

    #[test]
    fn a_merge_draws_a_fork_connector() {
        // m merges l and r; then each side, then their shared root.
        let log = [
            entry("m", &["l", "r"]),
            entry("l", &["base"]),
            entry("r", &["base"]),
            entry("base", &[]),
        ];
        let rows = log_graph(&log);
        // The merge is a ◆ node followed by a link row fanning out a second lane.
        assert_eq!(rows[0].entry, Some(0));
        assert!(
            text(&rows[0].spans).starts_with('◆'),
            "merge node is a diamond"
        );
        assert!(rows[1].entry.is_none(), "a connector row follows the merge");
        let link = text(&rows[1].spans);
        assert!(
            link.contains('╮') && link.contains('─'),
            "fork connector routes to the new lane: {link:?}"
        );
    }

    #[test]
    fn a_branch_tip_folds_back_with_a_connector() {
        // Two branches off a shared base that merge at the tip `m`.
        let log = [
            entry("m", &["a", "b"]),
            entry("a", &["base"]),
            entry("b", &["base"]),
            entry("base", &[]),
        ];
        let rows: Vec<String> = log_graph(&log).iter().map(|r| text(&r.spans)).collect();
        // Somewhere the second lane folds back into the first (base reached).
        assert!(
            rows.iter()
                .any(|r| r.contains('╯') || r.contains('┴') || r.contains('┼')),
            "a fold-back connector is drawn: {rows:?}"
        );
    }
}
