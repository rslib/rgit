//! git's diff output past libgit2's patch text: colors with whitespace
//! errors and moved lines, `--check`, `--dirstat` and external diff tools.

use std::collections::HashMap;

use rgit_git::{FileDiff, LineOrigin};

use crate::diffopts::{
    MOVED_BLOCKS, MOVED_PLAIN, MOVED_WS_ALL, MOVED_WS_CHANGE, MOVED_WS_EOL, MOVED_WS_INDENT,
    MOVED_ZEBRA_DIM, Render, WS_BLANK_AT_EOF, WS_BLANK_AT_EOL, WS_CR_AT_EOL,
    WS_INDENT_WITH_NON_TAB, WS_SPACE_BEFORE_TAB, WS_TAB_IN_INDENT, WS_TAB_WIDTH_MASK,
};

const RESET: &str = "\x1b[m";

/// git's isspace: space, tab, newline and carriage return only.
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// The diff color slots, from color.diff.<slot> over git's defaults.
struct Colors {
    meta: String,
    frag: String,
    func: String,
    context: String,
    old: String,
    new: String,
    ws: String,
    moved: [String; 8],
}

fn colors() -> Colors {
    let backend = crate::diffopts::backend();
    let slot = |name: &str, default: &str| {
        backend
            .as_ref()
            .and_then(|b| b.config_get(&format!("color.diff.{name}")).ok().flatten())
            .and_then(|v| rgit_git::ansi_color(&v))
            .unwrap_or_else(|| default.to_owned())
    };
    Colors {
        meta: slot("meta", "\x1b[1m"),
        frag: slot("frag", "\x1b[36m"),
        func: slot("func", ""),
        context: backend
            .as_ref()
            .and_then(|b| b.config_get("color.diff.context").ok().flatten())
            .and_then(|v| rgit_git::ansi_color(&v))
            .unwrap_or_else(|| slot("plain", "")),
        old: slot("old", "\x1b[31m"),
        new: slot("new", "\x1b[32m"),
        ws: slot("whitespace", "\x1b[41m"),
        moved: [
            slot("oldMoved", "\x1b[1;35m"),
            slot("oldMovedAlternative", "\x1b[1;34m"),
            slot("oldMovedDimmed", "\x1b[2m"),
            slot("oldMovedAlternativeDimmed", "\x1b[2;3m"),
            slot("newMoved", "\x1b[1;36m"),
            slot("newMovedAlternative", "\x1b[1;33m"),
            slot("newMovedDimmed", "\x1b[2m"),
            slot("newMovedAlternativeDimmed", "\x1b[2;3m"),
        ],
    }
}

/// One side of a file: its full id and bytes.
type Side = Option<(String, Vec<u8>)>;

/// Both sides of `f`, read through the backend (the new one from the work
/// tree when the diff is against it).
fn sides(f: &FileDiff, r: &Render) -> (Side, Side) {
    let Some(backend) = crate::diffopts::backend() else {
        return (None, None);
    };
    let Some(ids) = f.header.lines().find_map(|l| l.strip_prefix("index ")) else {
        return (None, None);
    };
    let ids = ids.split(' ').next().unwrap_or("");
    let Some((a, b)) = ids.split_once("..") else {
        return (None, None);
    };
    let rel = rgit_git::xdiff::tweaks().relative.unwrap_or_default();
    let read = |id: &str, path: &str, worktree: bool| -> Side {
        if id.bytes().all(|c| c == b'0') {
            return None;
        }
        let file = || std::fs::read(backend.workdir().join(format!("{rel}{path}"))).ok();
        if worktree && let Some(data) = file() {
            // git names a work tree file by its index entry's id when the
            // file matches it; diff-files never does.
            let id = match backend.read_object(id) {
                Ok(o) if !r.index_worktree => o.id,
                _ => "0".repeat(40),
            };
            return Some((id, data));
        }
        match backend.read_object(id) {
            Ok(o) => Some((o.id, o.data)),
            Err(_) => file().map(|d| ("0".repeat(40), d)),
        }
    };
    let old = f.old_path.as_deref().unwrap_or(&f.path);
    (read(a, old, false), read(b, &f.path, r.worktree))
}

fn count_lines(data: &[u8]) -> usize {
    if data.is_empty() {
        return 0;
    }
    data.iter().filter(|&&b| b == b'\n').count() + usize::from(!data.ends_with(b"\n"))
}

fn ws_blank_line(line: &[u8]) -> bool {
    line.iter().all(|&b| is_space(b))
}

/// git's count_trailing_blank.
fn trailing_blank(data: &[u8]) -> usize {
    if data.is_empty() {
        return 0;
    }
    let mut end = data.len() - usize::from(data.ends_with(b"\n"));
    let mut cnt = 0;
    while end > 0 {
        let start = data[..end]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1);
        if !ws_blank_line(&data[start..end]) {
            break;
        }
        cnt += 1;
        if start == 0 {
            break;
        }
        end = start - 1;
    }
    cnt
}

/// git's check_blank_at_eof: where new blank lines at the end start, in
/// the pre- and postimage (0 for none).
fn blank_at_eof(f: &FileDiff, r: &Render) -> (usize, usize) {
    if r.ws_rule & WS_BLANK_AT_EOF == 0 || f.binary {
        return (0, 0);
    }
    let (old, new) = sides(f, r);
    let (old, new) = (
        old.map(|o| o.1).unwrap_or_default(),
        new.map(|n| n.1).unwrap_or_default(),
    );
    let (l1, l2) = (trailing_blank(&old), trailing_blank(&new));
    if l2 <= l1 {
        return (0, 0);
    }
    (count_lines(&old) - l1 + 1, count_lines(&new) - l2 + 1)
}

/// git's ws_check_emit_1: the whitespace errors of `line` (no newline), and
/// the line painted with them when `paint` gives (set, ws).
fn ws_check(line: &[u8], rule: u32, paint: Option<(&str, &str)>) -> (u32, Vec<u8>) {
    let mut out = Vec::new();
    let mut result = 0;
    let mut len = line.len();
    let cr = rule & WS_CR_AT_EOL != 0 && len > 0 && line[len - 1] == b'\r';
    if cr {
        len -= 1;
    }
    let mut trailing = None;
    if rule & WS_BLANK_AT_EOL != 0 {
        for i in (0..len).rev() {
            if is_space(line[i]) {
                trailing = Some(i);
                result |= WS_BLANK_AT_EOL;
            } else {
                break;
            }
        }
    }
    let trailing = trailing.unwrap_or(len);
    let (set, ws) = paint.unwrap_or(("", ""));
    let color = paint.is_some();
    let mut written = 0;
    let mut i = 0;
    while i < trailing {
        if line[i] == b' ' {
            i += 1;
            continue;
        }
        if line[i] != b'\t' {
            break;
        }
        if rule & WS_SPACE_BEFORE_TAB != 0 && written < i {
            result |= WS_SPACE_BEFORE_TAB;
            if color {
                out.extend_from_slice(ws.as_bytes());
                out.extend_from_slice(&line[written..i]);
                out.extend_from_slice(RESET.as_bytes());
                out.push(line[i]);
            } else {
                out.extend_from_slice(&line[written..=i]);
            }
        } else if rule & WS_TAB_IN_INDENT != 0 {
            result |= WS_TAB_IN_INDENT;
            out.extend_from_slice(&line[written..i]);
            if color {
                out.extend_from_slice(ws.as_bytes());
                out.push(line[i]);
                out.extend_from_slice(RESET.as_bytes());
            } else {
                out.push(line[i]);
            }
        } else {
            out.extend_from_slice(&line[written..=i]);
        }
        written = i + 1;
        i += 1;
    }
    let tab_width = (rule & WS_TAB_WIDTH_MASK).max(1) as usize;
    if rule & WS_INDENT_WITH_NON_TAB != 0 && i - written >= tab_width {
        result |= WS_INDENT_WITH_NON_TAB;
        if color {
            out.extend_from_slice(ws.as_bytes());
        }
        out.extend_from_slice(&line[written..i]);
        if color {
            out.extend_from_slice(RESET.as_bytes());
        }
        written = i;
    }
    if trailing > written {
        if color {
            out.extend_from_slice(set.as_bytes());
        }
        out.extend_from_slice(&line[written..trailing]);
        if color {
            out.extend_from_slice(RESET.as_bytes());
        }
    }
    if trailing != len {
        if color {
            out.extend_from_slice(ws.as_bytes());
        }
        out.extend_from_slice(&line[trailing..len]);
        if color {
            out.extend_from_slice(RESET.as_bytes());
        }
    }
    if cr {
        out.push(b'\r');
    }
    (result, out)
}

fn ws_error_string(ws: u32) -> String {
    let mut err: Vec<&str> = Vec::new();
    if ws & WS_BLANK_AT_EOL != 0 {
        err.push("trailing whitespace");
    }
    if ws & WS_BLANK_AT_EOF != 0 {
        err.push("new blank line at EOF");
    }
    if ws & WS_SPACE_BEFORE_TAB != 0 {
        err.push("space before tab in indent");
    }
    if ws & WS_INDENT_WITH_NON_TAB != 0 {
        err.push("indent with spaces");
    }
    if ws & WS_TAB_IN_INDENT != 0 {
        err.push("tab in indent");
    }
    err.join(", ")
}

fn conflict_marker(line: &[u8]) -> bool {
    const SIZE: usize = 7;
    // `line` has its newline, which counts as the space after the marker.
    line.len() > SIZE
        && matches!(line[0], b'=' | b'>' | b'<' | b'|')
        && line[1..SIZE].iter().all(|&c| c == line[0])
        && is_space(line[SIZE])
}

/// The new-side start line of a hunk header.
fn new_start(header: &str) -> usize {
    header
        .split_once('+')
        .and_then(|(_, r)| r.split([',', ' ']).next()?.parse().ok())
        .unwrap_or(0)
}

/// Whether the text line at `i` of a hunk ends without a newline.
fn no_newline(lines: &[rgit_git::DiffLine], i: usize) -> bool {
    lines
        .get(i + 1)
        .is_some_and(|l| l.origin == LineOrigin::Meta && l.text.starts_with('\\'))
}

/// `--check`: each whitespace error and conflict marker on added lines.
/// Returns the report and whether anything was found.
pub fn check(files: &[FileDiff], r: &Render, color: bool) -> (String, bool) {
    let c = colors();
    let paint = color.then_some((c.new.as_str(), c.ws.as_str()));
    let mut out = Vec::new();
    let mut failed = false;
    for f in files.iter().filter(|f| !f.binary) {
        for h in &f.hunks {
            let mut lineno = new_start(&h.header).saturating_sub(1);
            for (i, l) in h.lines.iter().enumerate() {
                match l.origin {
                    LineOrigin::Added => {
                        lineno += 1;
                        let mut with_nl = l.text.as_bytes().to_vec();
                        if !no_newline(&h.lines, i) {
                            with_nl.push(b'\n');
                        }
                        if conflict_marker(&with_nl) {
                            failed = true;
                            out.extend_from_slice(
                                format!("{}:{lineno}: leftover conflict marker\n", f.path)
                                    .as_bytes(),
                            );
                        }
                        let (bad, text) = ws_check(l.text.as_bytes(), r.ws_rule, paint);
                        if bad == 0 {
                            continue;
                        }
                        failed = true;
                        out.extend_from_slice(
                            format!("{}:{lineno}: {}.\n", f.path, ws_error_string(bad)).as_bytes(),
                        );
                        if color {
                            out.extend_from_slice(format!("{}+{RESET}", c.new).as_bytes());
                        } else {
                            out.push(b'+');
                        }
                        out.extend_from_slice(&text);
                        out.push(b'\n');
                    }
                    LineOrigin::Context => lineno += 1,
                    _ => {}
                }
            }
        }
        let (_, post) = blank_at_eof(f, r);
        if post > 0 {
            failed = true;
            out.extend_from_slice(
                format!("{}:{post}: {}.\n", f.path, ws_error_string(WS_BLANK_AT_EOF)).as_bytes(),
            );
        }
    }
    (String::from_utf8_lossy(&out).into_owned(), failed)
}

const MOVED: u8 = 1;
const MOVED_ALT: u8 = 2;
const MOVED_DIM: u8 = 4;

/// One line of the patch being colored.
struct Sym {
    kind: LineOrigin,
    /// The content, with its newline when it has one.
    text: Vec<u8>,
    flags: u8,
    blank_eof: bool,
    id: usize,
    indent_width: i64,
    indent_off: usize,
}

const BLANK: i64 = i64::MIN;

/// git's fill_es_indent_data.
fn indent_data(s: &[u8], tab: usize) -> (i64, usize) {
    let mut off = 0;
    while off < s.len()
        && (s[off] == 0x0c || s[off] == 0x0b || (off + 1 < s.len() && s[off] == b'\r'))
    {
        off += 1;
    }
    let mut width: i64 = 0;
    loop {
        match s.get(off) {
            Some(b' ') => {
                width += 1;
                off += 1;
            }
            Some(b'\t') => {
                width += tab as i64 - width % tab as i64;
                off += 1;
                while s.get(off) == Some(&b'\t') {
                    width += tab as i64;
                    off += 1;
                }
            }
            _ => break,
        }
    }
    if s[off..].iter().all(|&b| is_space(b)) {
        (BLANK, s.len())
    } else {
        (width, off)
    }
}

/// The text moved lines are compared by, as xdiff_compare_lines with the
/// --color-moved-ws flags does.
fn moved_key(text: &[u8], ws: u32) -> Vec<u8> {
    if ws & MOVED_WS_ALL != 0 {
        return text.iter().copied().filter(|&b| !is_space(b)).collect();
    }
    if ws & MOVED_WS_CHANGE != 0 {
        let mut out = Vec::new();
        let mut i = 0;
        while i < text.len() {
            if is_space(text[i]) {
                while i < text.len() && is_space(text[i]) {
                    i += 1;
                }
                if i < text.len() {
                    out.push(b' ');
                }
                continue;
            }
            out.push(text[i]);
            i += 1;
        }
        return out;
    }
    if ws & MOVED_WS_EOL != 0 {
        let end = text
            .iter()
            .rposition(|&b| !is_space(b))
            .map_or(0, |i| i + 1);
        return text[..end].to_vec();
    }
    text.to_vec()
}

struct Entry {
    sym: usize,
    next_line: Option<usize>,
}

/// git's mark_color_as_moved (and dim_moved_lines) over the lines of one diff.
fn mark_moved(syms: &mut [Sym], mode: u8, ws: u32, tab: usize) {
    let indent = ws & MOVED_WS_INDENT != 0;
    let mut ids: HashMap<Vec<u8>, usize> = HashMap::new();
    let mut entries: Vec<Entry> = Vec::new();
    // Per id: (added, deleted) entries, newest first.
    let mut lists: Vec<(Vec<usize>, Vec<usize>)> = Vec::new();
    let mut prev: Option<usize> = None;
    for n in 0..syms.len() {
        let kind = syms[n].kind;
        if !matches!(kind, LineOrigin::Added | LineOrigin::Removed) {
            prev = None;
            continue;
        }
        if indent {
            let (w, off) = indent_data(&syms[n].text, tab);
            syms[n].indent_width = w;
            syms[n].indent_off = off;
        }
        let key = moved_key(&syms[n].text[syms[n].indent_off..], ws);
        let next = ids.len();
        let id = *ids.entry(key).or_insert(next);
        if id == lists.len() {
            lists.push((Vec::new(), Vec::new()));
        }
        syms[n].id = id;
        let e = entries.len();
        entries.push(Entry {
            sym: n,
            next_line: None,
        });
        if let Some(p) = prev
            && syms[entries[p].sym].kind == kind
        {
            entries[p].next_line = Some(e);
        }
        prev = Some(e);
        if kind == LineOrigin::Added {
            lists[id].0.insert(0, e);
        } else {
            lists[id].1.insert(0, e);
        }
    }
    let alnum_enough = |syms: &mut [Sym], n: usize, len: usize| -> bool {
        if mode == MOVED_PLAIN {
            return len > 0;
        }
        let mut count = 0;
        for i in 1..=len {
            for &c in &syms[n - i].text {
                if c.is_ascii_alphanumeric() {
                    count += 1;
                    if count >= 20 {
                        return true;
                    }
                }
            }
        }
        for i in 1..=len {
            syms[n - i].flags &= !MOVED;
        }
        false
    };
    // Potential blocks: (entry, whitespace delta).
    let mut pmb: Vec<(usize, i64)> = Vec::new();
    let (mut flipped, mut block_len) = (false, 0usize);
    let mut moved_kind: Option<LineOrigin> = None;
    let mut n: isize = 0;
    while (n as usize) < syms.len() {
        let i = n as usize;
        let kind = syms[i].kind;
        let mut matched: Option<Vec<usize>> = match kind {
            LineOrigin::Added => Some(lists[syms[i].id].1.clone()),
            LineOrigin::Removed => Some(lists[syms[i].id].0.clone()),
            _ => {
                flipped = false;
                None
            }
        }
        .filter(|m| !m.is_empty());
        if !pmb.is_empty() && (matched.is_none() || Some(kind) != moved_kind) {
            if !alnum_enough(syms, i, block_len) && block_len > 1 {
                matched = None;
                n -= block_len as isize;
            }
            pmb.clear();
            block_len = 0;
            flipped = false;
        }
        let Some(matched) = matched else {
            moved_kind = None;
            n += 1;
            continue;
        };
        if mode == MOVED_PLAIN {
            syms[i].flags |= MOVED;
            n += 1;
            continue;
        }
        // pmb_advance_or_null
        let mut kept = Vec::new();
        for &(e, mut wsd) in &pmb {
            let Some(cur) = entries[e].next_line else {
                continue;
            };
            let cs = &syms[entries[cur].sym];
            let ok = cs.id == syms[i].id
                && (!indent || cs.indent_width == BLANK || {
                    let delta = syms[i].indent_width - cs.indent_width;
                    if wsd == BLANK {
                        wsd = delta;
                    }
                    delta == wsd
                });
            if ok {
                kept.push((cur, wsd));
            }
        }
        pmb = kept;
        if pmb.is_empty() {
            let contiguous = alnum_enough(syms, i, block_len);
            if !contiguous && block_len > 1 {
                n -= block_len as isize;
            } else {
                for &e in &matched {
                    let wsd = if indent {
                        let (a, b) = (syms[i].indent_width, syms[entries[e].sym].indent_width);
                        if a == BLANK && b == BLANK {
                            BLANK
                        } else {
                            a - b
                        }
                    } else {
                        0
                    };
                    pmb.push((e, wsd));
                }
            }
            flipped = contiguous && !pmb.is_empty() && moved_kind == Some(kind) && !flipped;
            moved_kind = (!pmb.is_empty()).then_some(kind);
            block_len = 0;
        }
        if !pmb.is_empty() {
            block_len += 1;
            syms[i].flags |= MOVED;
            if flipped && mode != MOVED_BLOCKS {
                syms[i].flags |= MOVED_ALT;
            }
        }
        n += 1;
    }
    let end = syms.len();
    alnum_enough(syms, end, block_len);
    if mode == MOVED_ZEBRA_DIM {
        dim_moved(syms);
    }
}

fn dim_moved(syms: &mut [Sym]) {
    let pm = |s: &Sym| matches!(s.kind, LineOrigin::Added | LineOrigin::Removed);
    let zebra = MOVED | MOVED_ALT;
    for n in 0..syms.len() {
        if !pm(&syms[n]) || syms[n].flags & MOVED == 0 {
            continue;
        }
        let prev = (n > 0 && pm(&syms[n - 1])).then(|| syms[n - 1].flags);
        let next = syms.get(n + 1).filter(|s| pm(s)).map(|s| s.flags);
        let me = syms[n].flags;
        if prev.is_some_and(|p| p & zebra == me & zebra)
            && next.is_some_and(|x| x & zebra == me & zebra)
        {
            syms[n].flags |= MOVED_DIM;
            continue;
        }
        if prev.is_some_and(|p| p & MOVED != 0 && p & MOVED_ALT != me & MOVED_ALT) {
            continue;
        }
        if next.is_some_and(|x| x & MOVED != 0 && x & MOVED_ALT != me & MOVED_ALT) {
            continue;
        }
        syms[n].flags |= MOVED_DIM;
    }
}

/// git's emit_line_0 for a line without its newline.
fn line0(set_sign: Option<&str>, set: Option<&str>, sign: Option<u8>, line: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut len = line.len();
    let cr = len > 0 && line[len - 1] == b'\r';
    if cr {
        len -= 1;
    }
    let mut reset = false;
    if len > 0 || sign.is_some() {
        if let Some(s) = set_sign {
            out.extend_from_slice(s.as_bytes());
            reset = true;
        }
        if let Some(c) = sign {
            out.push(c);
        }
        if len > 0 {
            if let Some(s) = set {
                if set_sign.is_some_and(|ss| ss != s) {
                    out.extend_from_slice(RESET.as_bytes());
                }
                out.extend_from_slice(s.as_bytes());
            }
            out.extend_from_slice(&line[..len]);
            reset = true;
        }
    }
    if reset {
        out.extend_from_slice(RESET.as_bytes());
    }
    if cr {
        out.push(b'\r');
    }
    out
}

/// git's emit_hunk_header in color.
fn hunk_header(header: &str, c: &Colors) -> String {
    let Some(at) = header.get(2..).and_then(|h| h.find("@@")).map(|i| i + 4) else {
        return header.to_owned();
    };
    let (frag, rest) = header.split_at(at);
    let mut out = format!("{}{frag}{RESET}", c.frag);
    let func = rest.trim_start_matches([' ', '\t']);
    let blank = &rest[..rest.len() - func.len()];
    if !blank.is_empty() {
        out.push_str(&format!("{}{blank}{RESET}", c.context));
    }
    if !func.is_empty() {
        out.push_str(&format!("{}{func}{RESET}", c.func));
    }
    out
}

/// Header lines in the meta color, a binary patch's body left plain.
fn header(text: &str, c: &Colors, color: bool) -> String {
    let mut out = String::new();
    let mut plain = false;
    for line in text.lines() {
        plain |= line == "GIT binary patch";
        if !color || plain || line.starts_with("Binary files ") || line.is_empty() {
            out.push_str(line);
        } else {
            out.push_str(&format!("{}{line}{RESET}", c.meta));
        }
        out.push('\n');
    }
    out
}

/// A patch in git's colors, with whitespace errors and (under
/// --color-moved) moved lines marked; or an external tool's output for
/// the files one applies to.
pub fn patch(files: &[FileDiff], r: &Render, color: bool) -> String {
    let c = colors();
    let mut out: Vec<u8> = Vec::new();
    // Each output piece: raw bytes, or a line to color at the end.
    enum Piece {
        Raw(Vec<u8>),
        Line(usize),
    }
    let mut pieces = Vec::new();
    let mut syms: Vec<Sym> = Vec::new();
    let total = files.len();
    let mut counter = 0;
    for f in files {
        if let Some(text) = submodule(f, r, color) {
            pieces.push(Piece::Raw(text));
            continue;
        }
        if let Some(text) = external(f, r, counter + 1, total) {
            counter += 1;
            pieces.push(Piece::Raw(text));
            continue;
        }
        pieces.push(Piece::Raw(header(&f.header, &c, color).into_bytes()));
        if !color {
            let mut body = String::new();
            for h in &f.hunks {
                body.push_str(&h.header);
                body.push('\n');
                for l in &h.lines {
                    let sign = match l.origin {
                        LineOrigin::Added => "+",
                        LineOrigin::Removed => "-",
                        LineOrigin::Context => " ",
                        LineOrigin::Meta => "",
                    };
                    body.push_str(sign);
                    body.push_str(&l.text);
                    body.push('\n');
                }
            }
            pieces.push(Piece::Raw(body.into_bytes()));
            continue;
        }
        let (_, post_eof) = blank_at_eof(f, r);
        for h in &f.hunks {
            pieces.push(Piece::Raw(
                format!("{}\n", hunk_header(&h.header, &c)).into_bytes(),
            ));
            let mut post = new_start(&h.header).saturating_sub(1);
            for (i, l) in h.lines.iter().enumerate() {
                let mut text = l.text.as_bytes().to_vec();
                if l.origin != LineOrigin::Meta && !no_newline(&h.lines, i) {
                    text.push(b'\n');
                }
                if matches!(l.origin, LineOrigin::Added | LineOrigin::Context) {
                    post += 1;
                }
                let blank_eof = l.origin == LineOrigin::Added
                    && post_eof > 0
                    && post >= post_eof
                    && ws_blank_line(l.text.as_bytes());
                pieces.push(Piece::Line(syms.len()));
                syms.push(Sym {
                    kind: l.origin,
                    text,
                    flags: 0,
                    blank_eof,
                    id: 0,
                    indent_width: 0,
                    indent_off: 0,
                });
            }
        }
    }
    if color && r.color_moved != 0 {
        let tab = (r.ws_rule & WS_TAB_WIDTH_MASK).max(1) as usize;
        mark_moved(&mut syms, r.color_moved, r.color_moved_ws, tab);
    }
    for p in pieces {
        match p {
            Piece::Raw(b) => out.extend_from_slice(&b),
            Piece::Line(i) => {
                out.extend_from_slice(&emit(&syms[i], r, &c));
                out.push(b'\n');
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One colored patch line, as git's emit_diff_symbol_from_struct.
fn emit(s: &Sym, r: &Render, c: &Colors) -> Vec<u8> {
    let line = s.text.strip_suffix(b"\n").unwrap_or(&s.text);
    let (sign, wseh, base) = match s.kind {
        LineOrigin::Added => (b'+', 1, 4),
        LineOrigin::Removed => (b'-', 4, 0),
        LineOrigin::Context => (b' ', 2, 0),
        LineOrigin::Meta => return line0(Some(&c.context), None, None, line),
    };
    let set: &str = if s.flags & MOVED != 0 && s.kind != LineOrigin::Context {
        let alt = usize::from(s.flags & MOVED_ALT != 0);
        let dim = usize::from(s.flags & MOVED_DIM != 0);
        &c.moved[base + alt + 2 * dim]
    } else {
        match s.kind {
            LineOrigin::Added => &c.new,
            LineOrigin::Removed => &c.old,
            _ => &c.context,
        }
    };
    let ws = (r.ws_highlight & wseh != 0 && !c.ws.is_empty()).then_some(c.ws.as_str());
    match ws {
        None => line0(Some(set), None, Some(sign), line),
        Some(ws) if s.blank_eof => line0(Some(ws), None, Some(sign), line),
        Some(ws) => {
            let mut out = line0(Some(set), None, Some(sign), b"");
            out.extend_from_slice(&ws_check(line, r.ws_rule, Some((set, ws))).1);
            out
        }
    }
}

/// `diff.<driver>.command` for `path`, or the external diff every file
/// takes.
fn external_cmd(path: &str, r: &Render) -> Option<String> {
    if !r.allow_external {
        return None;
    }
    let backend = crate::diffopts::backend()?;
    let rel = rgit_git::xdiff::tweaks().relative.unwrap_or_default();
    let driver = rgit_git::check_attr(
        &backend.git_dir(),
        &["diff".to_owned()],
        &[format!("{rel}{path}")],
        false,
    )
    .ok()
    .and_then(|rows| rows.into_iter().next())
    .map(|(_, _, v)| v)
    .filter(|v| !matches!(v.as_str(), "set" | "unset" | "unspecified"));
    driver
        .and_then(|d| {
            backend
                .config_get(&format!("diff.{d}.command"))
                .ok()
                .flatten()
        })
        .or_else(|| r.external.clone())
}

/// Run an external diff tool for `f` as git's run_external_diff does.
fn external(f: &FileDiff, r: &Render, counter: usize, total: usize) -> Option<Vec<u8>> {
    let pgm = external_cmd(&f.path, r)?;
    let backend = crate::diffopts::backend()?;
    let (old, new) = sides(f, r);
    let mode = |key: &str| {
        f.header
            .lines()
            .find_map(|l| l.strip_prefix(key).map(|m| m.trim().to_owned()))
    };
    let index_mode = f
        .header
        .lines()
        .find_map(|l| l.strip_prefix("index "))
        .and_then(|l| l.split(' ').nth(1))
        .map(str::to_owned);
    let old_mode = mode("old mode ")
        .or_else(|| mode("deleted file mode "))
        .or_else(|| index_mode.clone())
        .unwrap_or_else(|| "100644".to_owned());
    let new_mode = mode("new mode ")
        .or_else(|| mode("new file mode "))
        .or(index_mode)
        .unwrap_or_else(|| "100644".to_owned());
    let rel = rgit_git::xdiff::tweaks().relative.unwrap_or_default();
    let dir = std::env::temp_dir();
    let mut temps = Vec::new();
    let mut side = |s: Side, path: &str, mode: String, worktree: bool| -> [String; 3] {
        match s {
            None => ["/dev/null".to_owned(), ".".to_owned(), ".".to_owned()],
            Some((id, _)) if worktree => [format!("{rel}{path}"), id, mode],
            Some((id, data)) => {
                let base = path.rsplit('/').next().unwrap_or(path);
                let name = dir.join(format!("{}_{base}", &id[..6.min(id.len())]));
                let _ = std::fs::write(&name, data);
                temps.push(name.clone());
                [name.to_string_lossy().into_owned(), id, mode]
            }
        }
    };
    let old_path = f.old_path.clone().unwrap_or_else(|| f.path.clone());
    let a = side(old, &old_path, old_mode, false);
    let b = side(new, &f.path, new_mode, r.worktree);
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c")
        .arg(format!("{pgm} \"$@\""))
        .arg(&pgm)
        .arg(&old_path)
        .args(&a)
        .args(&b)
        .current_dir(backend.workdir())
        .env("GIT_DIFF_PATH_COUNTER", counter.to_string())
        .env("GIT_DIFF_PATH_TOTAL", total.to_string());
    if f.old_path.is_some() {
        let meta: String = f
            .header
            .lines()
            .skip(1)
            .take_while(|l| !l.starts_with("--- ") && !l.starts_with("Binary files "))
            .map(|l| format!("{l}\n"))
            .collect();
        cmd.arg(&f.path).arg(meta);
    }
    let out = cmd.stderr(std::process::Stdio::inherit()).output();
    for t in temps {
        let _ = std::fs::remove_file(t);
    }
    match out {
        Ok(o) if o.status.success() => Some(o.stdout),
        // git dies here, before anything it buffered reaches stdout.
        _ => {
            eprintln!("fatal: external diff died, stopping at {old_path}");
            std::process::exit(128);
        }
    }
}

/// Bytes the new side shares with the old and bytes it adds, by git's
/// span hashing (diffcore_count_changes).
fn count_changes(old: &[u8], new: &[u8]) -> (u64, u64) {
    rgit_git::xdiff::count_changes(old, new)
}

/// `--dirstat`: each folder's share of the damage, as git's show_dirstat.
pub fn dirstat(
    files: &[FileDiff],
    r: &Render,
    (by, permille, cumulative): (u8, usize, bool),
) -> String {
    let mut rows: Vec<(String, u64)> = Vec::new();
    for f in files {
        let damage = if by == 1 {
            let (a, d) = crate::axi::line_counts(f);
            let d = (a + d) as u64;
            if f.binary { d.div_ceil(64) } else { d }
        } else {
            let (old, new) = sides(f, r);
            let same =
                matches!((&old, &new), (Some(a), Some(b)) if a.0 == b.0 && a.0 != "0".repeat(40));
            if same || (old.is_none() && new.is_none() && !f.header.contains("index ")) {
                0
            } else if by == 2 {
                1
            } else {
                let d = match (&old, &new) {
                    (Some(a), Some(b)) => {
                        let (copied, added) = count_changes(&a.1, &b.1);
                        (a.1.len() as u64).saturating_sub(copied) + added
                    }
                    (Some(a), None) => a.1.len() as u64,
                    (None, Some(b)) => b.1.len() as u64,
                    (None, None) => 0,
                };
                d.max(1)
            }
        };
        rows.push((f.path.clone(), damage));
    }
    let changed: u64 = rows.iter().map(|r| r.1).sum();
    if changed == 0 {
        return String::new();
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = String::new();
    let mut at = 0;
    gather(&rows, &mut at, changed, "", permille, cumulative, &mut out);
    out
}

fn gather(
    rows: &[(String, u64)],
    at: &mut usize,
    changed: u64,
    base: &str,
    permille: usize,
    cumulative: bool,
    out: &mut String,
) -> u64 {
    let mut sum = 0;
    let mut sources = 0;
    while *at < rows.len() {
        let name = &rows[*at].0;
        if name.len() < base.len() || !name.starts_with(base) {
            break;
        }
        let changes = match name[base.len()..].find('/') {
            Some(i) => {
                sources += 1;
                let sub = name[..base.len() + i + 1].to_owned();
                gather(rows, at, changed, &sub, permille, cumulative, out)
            }
            None => {
                sources += 2;
                *at += 1;
                rows[*at - 1].1
            }
        };
        sum += changes;
    }
    if !base.is_empty() && sources != 1 && sum > 0 {
        let p = (sum * 1000 / changed) as usize;
        if p >= permille {
            out.push_str(&format!("{:4}.{}% {base}\n", p / 10, p % 10));
            if !cumulative {
                return 0;
            }
        }
    }
    sum
}

/// The submodule a gitlink change names, with its old and new commits
/// (all zeros for a side that is absent).
fn gitlink(f: &FileDiff) -> Option<(String, String)> {
    if !f.header.lines().any(|l| l.ends_with(" 160000")) {
        return None;
    }
    let zero = "0".repeat(40);
    let (mut old, mut new) = (zero.clone(), zero);
    for l in f.hunks.iter().flat_map(|h| &h.lines) {
        let Some(id) = l.text.strip_prefix("Subproject commit ") else {
            continue;
        };
        let id = id.trim_end_matches("-dirty").to_owned();
        match l.origin {
            LineOrigin::Removed => old = id,
            LineOrigin::Added => new = id,
            _ => {}
        }
    }
    Some((old, new))
}

type Backend = std::sync::Arc<dyn rgit_git::GitBackend>;

/// The repository checked out at `dir`, when it is one of its own.
fn open_sub(dir: &std::path::Path) -> Option<Backend> {
    let b = rgit_git::Git2Backend::discover(dir).ok()?;
    let b = std::sync::Arc::new(b) as Backend;
    (b.workdir().canonicalize().ok() == dir.canonicalize().ok()).then_some(b)
}

/// A submodule's change as `--submodule=log` or `=diff` shows it, as git's
/// show_submodule_header and its summary or inline diff.
fn submodule(f: &FileDiff, r: &Render, color: bool) -> Option<Vec<u8>> {
    let format = r.submodule.as_deref().filter(|s| *s != "short")?;
    let (old, new) = gitlink(f)?;
    let top = crate::diffopts::backend()?;
    let rel = rgit_git::xdiff::tweaks().relative.unwrap_or_default();
    let dir = top.workdir().join(format!("{rel}{}", f.path));
    let sub = open_sub(&dir);
    let null = |id: &str| id.bytes().all(|c| c == b'0');
    let mut message = if null(&old) {
        Some("(new submodule)")
    } else if null(&new) {
        Some("(submodule deleted)")
    } else {
        None
    };
    let (mut left, mut right) = (false, false);
    let mut bases = Vec::new();
    if let Some(s) = &sub {
        left = !null(&old) && s.read_object(&old).is_ok();
        right = !null(&new) && s.read_object(&new).is_ok();
        if (!null(&old) && !left) || (!null(&new) && !right) {
            message = Some("(commits not present)");
        }
        if left && right {
            if old == new {
                return Some(Vec::new());
            }
            bases = s.merge_bases(&old, &new, true).unwrap_or_default();
        }
    } else if message.is_none() {
        message = Some("(commits not present)");
    }
    let forward = bases.first().is_some_and(|b| *b == old);
    let backward = bases.first().is_some_and(|b| *b == new);
    let abbrev = |id: &str| match &sub {
        Some(s) if !null(id) => s.abbrev_id(id, 7).unwrap_or_else(|_| id[..7].to_owned()),
        _ => id[..7.min(id.len())].to_owned(),
    };
    let mut out = format!(
        "Submodule {} {}{}{}",
        f.path,
        abbrev(&old),
        if forward || backward { ".." } else { "..." },
        abbrev(&new)
    );
    match message {
        Some(m) => out.push_str(&format!(" {m}\n")),
        None => out.push_str(&format!("{}:\n", if backward { " (rewind)" } else { "" })),
    }
    if format == "diff" {
        if let Some(s) = &sub
            && (left || null(&old))
        {
            out.push_str(&inline(s, &old, &new, &f.path, r));
        }
        return Some(out.into_bytes());
    }
    let Some(s) = sub.filter(|_| left && right) else {
        return Some(out.into_bytes());
    };
    let entries = s
        .log(&rgit_git::LogOptions {
            limit: usize::MAX,
            revs: vec![format!("{old}...{new}")],
            first_parent: true,
            ..Default::default()
        })
        .unwrap_or_default();
    let c = colors();
    for e in entries {
        let mark = e.mark.unwrap_or('>');
        let line = format!("  {mark} {}", e.summary);
        let set = if mark == '<' { &c.old } else { &c.new };
        if color {
            out.push_str(&format!("{set}{line}{RESET}\n"));
        } else {
            out.push_str(&line);
            out.push('\n');
        }
    }
    Some(out.into_bytes())
}

/// `--submodule=diff`: the submodule's own patch between the two commits
/// (or against its work tree), named under its path.
fn inline(s: &Backend, old: &str, new: &str, path: &str, r: &Render) -> String {
    let null = |id: &str| id.bytes().all(|c| c == b'0');
    let (saved, tweaks) = (crate::diffopts::current(), rgit_git::xdiff::tweaks());
    let top = crate::diffopts::backend();
    let opts = crate::diffopts::DiffOptArgs {
        src_prefix: Some(format!("{}{path}/", r.src_prefix)),
        dst_prefix: Some(format!("{}{path}/", r.dst_prefix)),
        submodule: Some("diff".to_owned()),
        ..Default::default()
    };
    let sides = if null(new) {
        crate::diffopts::Sides::CommitWorktree
    } else {
        crate::diffopts::Sides::Commits
    };
    let text = opts
        .apply(s, sides, true, true)
        .ok()
        .and_then(|()| {
            s.diff(&rgit_git::DiffSpec {
                from: (!null(old)).then(|| old.to_owned()),
                to: (!null(new)).then(|| new.to_owned()),
                ..Default::default()
            })
            .ok()
        })
        .map(|files| crate::render::patch(&files))
        .unwrap_or_default();
    crate::diffopts::set(saved);
    rgit_git::xdiff::set_tweaks(tweaks);
    crate::diffopts::set_backend(top);
    text
}
