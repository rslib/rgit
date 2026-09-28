//! git's text filters that need no repository: `stripspace`, `column`,
//! `check-ref-format` and `patch-id`, byte for byte as git does them.

use std::fmt::Write as _;

use sha1::{Digest, Sha1};

/// `git stripspace`: trailing whitespace off each line, runs of blank lines
/// squeezed to one, none at either end, and lines starting with `comment`
/// dropped when given.
pub fn stripspace(input: &[u8], comment: Option<&str>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut empties = 0;
    for line in input.split_inclusive(|&b| b == b'\n') {
        if let Some(c) = comment
            && line.starts_with(c.as_bytes())
        {
            continue;
        }
        let end = line
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
            .map_or(0, |i| i + 1);
        if end == 0 {
            empties += 1;
            continue;
        }
        if empties > 0 && !out.is_empty() {
            out.push(b'\n');
        }
        empties = 0;
        out.extend_from_slice(&line[..end]);
        out.push(b'\n');
    }
    out
}

/// `git stripspace -c`: each line prefixed with `comment` and a space (no
/// space before an empty or tab-led line).
pub fn comment_lines(input: &[u8], comment: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for line in input.split_inclusive(|&b| b == b'\n') {
        out.extend_from_slice(comment.as_bytes());
        if !matches!(line[0], b'\n' | b'\t') {
            out.push(b' ');
        }
        out.extend_from_slice(line);
    }
    if out.last().is_some_and(|&b| b != b'\n') {
        out.push(b'\n');
    }
    out
}

const COL_ENABLE_MASK: u32 = 0x30;
const COL_ENABLED: u32 = 0x10;
const COL_AUTO: u32 = 0x20;
const COL_DENSE: u32 = 0x80;
const COL_ROW: u32 = 1;
const COL_PLAIN: u32 = 15;

/// Apply a column.* / `--column=` setting (`always,column,dense`, ...) to
/// git's option bits.
pub fn column_mode(opts: &mut u32, spec: &str) -> Result<(), String> {
    for word in spec.split([' ', ',']).filter(|w| !w.is_empty()) {
        let (neg, name) = match word.strip_prefix("no") {
            Some(rest) if rest == "dense" => (true, rest),
            _ => (false, word),
        };
        match name {
            "always" => *opts = *opts & !COL_ENABLE_MASK | COL_ENABLED,
            "never" => *opts &= !COL_ENABLE_MASK,
            "auto" => *opts = *opts & !COL_ENABLE_MASK | COL_AUTO,
            "plain" => *opts = *opts & !0xf | COL_PLAIN,
            "column" => *opts &= !0xf,
            "row" => *opts = *opts & !0xf | COL_ROW,
            "dense" if neg => *opts &= !COL_DENSE,
            "dense" => *opts |= COL_DENSE,
            _ => return Err(format!("unsupported option '{word}'")),
        }
    }
    Ok(())
}

/// Resolve `auto` against whether stdout is a terminal.
pub fn column_finalize(opts: &mut u32, tty: bool) {
    if *opts & COL_ENABLE_MASK == COL_AUTO {
        *opts &= !COL_ENABLE_MASK;
        if tty {
            *opts |= COL_ENABLED;
        }
    }
}

/// `git column`'s layout options.
pub struct ColumnOpts<'a> {
    pub width: usize,
    pub indent: &'a str,
    pub nl: &'a str,
    pub padding: usize,
}

/// Display width without ANSI color sequences.
fn width_of(s: &str) -> usize {
    let mut n = 0;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            n += 1;
        }
    }
    n
}

/// git's column.c `print_columns`.
pub fn columns(items: &[String], colopts: u32, o: &ColumnOpts) -> String {
    let mut out = String::new();
    if items.is_empty() {
        return out;
    }
    if colopts & COL_ENABLE_MASK == 0 {
        for s in items {
            let _ = writeln!(out, "{s}");
        }
        return out;
    }
    if colopts & 0xf == COL_PLAIN {
        for s in items {
            out.push_str(o.indent);
            out.push_str(s);
            out.push_str(o.nl);
        }
        return out;
    }
    let by_col = colopts & 0xf == 0;
    let n = items.len();
    let len: Vec<usize> = items.iter().map(|s| width_of(s)).collect();
    let initial = len.iter().copied().max().unwrap_or(0) + o.padding;
    let mut cols = (o.width.saturating_sub(o.indent.len()) / initial.max(1)).max(1);
    let mut rows = n.div_ceil(cols);
    let at = |x: usize, y: usize, rows: usize, cols: usize| {
        if by_col { x * rows + y } else { y * cols + x }
    };
    let widths = |rows: usize, cols: usize| -> Vec<usize> {
        (0..cols)
            .map(|x| {
                let mut w = at(x, 0, rows, cols);
                for y in 0..rows {
                    let i = at(x, y, rows, cols);
                    if i < n && len.get(w).is_some_and(|&l| l < len[i]) {
                        w = i;
                    }
                }
                w
            })
            .collect()
    };
    let mut width = None;
    if colopts & COL_DENSE != 0 {
        while rows > 1 {
            let (r, c) = (rows, cols);
            rows -= 1;
            cols = n.div_ceil(rows);
            let w = widths(rows, cols);
            let total = o.indent.len()
                + w.iter()
                    .map(|&i| len.get(i).copied().unwrap_or(0) + o.padding)
                    .sum::<usize>();
            if total > o.width {
                (rows, cols) = (r, c);
                break;
            }
        }
        width = Some(widths(rows, cols));
    }
    for y in 0..rows {
        for x in 0..cols {
            let i = at(x, y, rows, cols);
            if i >= n {
                break;
            }
            let mut l = len[i];
            if let Some(w) = &width {
                let cw = len.get(w[x]).copied().unwrap_or(0);
                if cw < initial {
                    l = l + initial - cw - o.padding;
                }
            }
            let newline = if by_col {
                i + rows >= n
            } else {
                x == cols - 1 || i == n - 1
            };
            if x == 0 {
                out.push_str(o.indent);
            }
            out.push_str(&items[i]);
            if newline {
                out.push_str(o.nl);
            } else {
                out.push_str(&" ".repeat(initial.saturating_sub(l)));
            }
        }
    }
    out
}

/// git's `check_refname_format`; `onelevel` and `pattern` are
/// REFNAME_ALLOW_ONELEVEL and REFNAME_REFSPEC_PATTERN.
pub fn check_ref_format(name: &str, onelevel: bool, pattern: bool) -> bool {
    if name == "@" || name.is_empty() {
        return false;
    }
    let mut star = pattern;
    let mut count = 0;
    for comp in name.split('/') {
        count += 1;
        if comp.is_empty() || comp.starts_with('.') || comp.ends_with(".lock") {
            return false;
        }
        let b = comp.as_bytes();
        for (i, &c) in b.iter().enumerate() {
            match c {
                b'*' if star => star = false,
                0..=0x20 | 0x7f | b'~' | b'^' | b':' | b'?' | b'[' | b'\\' | b'*' => return false,
                b'.' if i > 0 && b[i - 1] == b'.' => return false,
                b'{' if i > 0 && b[i - 1] == b'@' => return false,
                _ => {}
            }
        }
    }
    !name.ends_with('.') && (count >= 2 || onelevel)
}

/// Slashes collapsed and a leading one dropped, as `--normalize` does.
pub fn collapse_slashes(name: &str) -> String {
    let mut out = String::new();
    let mut prev = '/';
    for c in name.chars() {
        if !(prev == '/' && c == '/') {
            out.push(c);
        }
        prev = c;
    }
    out
}

/// A path as git prints it with core.quotePath on: in double quotes with C
/// escapes when it has a quote, backslash, control or non-ASCII byte.
pub fn quote_path(path: &str) -> String {
    let needs = path
        .bytes()
        .any(|b| !(0x20..0x7f).contains(&b) || b == b'"' || b == b'\\');
    if !needs {
        return path.to_owned();
    }
    let mut out = String::from("\"");
    for b in path.bytes() {
        match b {
            b'\x07' => out.push_str("\\a"),
            b'\x08' => out.push_str("\\b"),
            b'\t' => out.push_str("\\t"),
            b'\n' => out.push_str("\\n"),
            b'\x0b' => out.push_str("\\v"),
            b'\x0c' => out.push_str("\\f"),
            b'\r' => out.push_str("\\r"),
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b if !(0x20..0x7f).contains(&b) => {
                let _ = write!(out, "\\{b:03o}");
            }
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}

/// `git patch-id`: each patch on `input` (from `log -p`, `format-patch` or
/// `diff`) as `<patch id> <commit id>` lines. `stable` sums per-file hashes
/// so file order does not matter; `verbatim` keeps whitespace.
pub fn patch_ids(input: &[u8], stable: bool, verbatim: bool) -> String {
    let mut out = String::new();
    let mut lines = input.split_inclusive(|&b| b == b'\n').peekable();
    let zero = "0".repeat(40);
    let mut commit = zero.clone();
    while lines.peek().is_some() {
        let (next, id, len) = one_patch_id(&mut lines, stable, verbatim);
        if len > 0 {
            let _ = writeln!(out, "{} {commit}", hex(&id));
        }
        commit = next.unwrap_or_else(|| zero.clone());
    }
    out
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

fn flush(sum: &mut [u8; 20], h: &mut Sha1) {
    let mut carry = 0u16;
    for (s, d) in sum.iter_mut().zip(std::mem::take(h).finalize()) {
        carry += u16::from(*s) + u16::from(d);
        *s = carry as u8;
        carry >>= 8;
    }
}

/// `@@ -a,b +c,d`'s line counts b and d.
fn hunk_counts(line: &[u8]) -> Option<(i64, i64)> {
    let s = std::str::from_utf8(line.get(4..)?).ok()?;
    let count = |s: &str| -> Option<(i64, usize)> {
        let n = s.bytes().take_while(u8::is_ascii_digit).count();
        match s[n..].strip_prefix(',') {
            Some(rest) => {
                let m = rest.bytes().take_while(u8::is_ascii_digit).count();
                Some((rest[..m].parse().unwrap_or(0), n + 1 + m))
            }
            None if n > 0 => Some((1, n)),
            None => None,
        }
    };
    let (before, used) = count(s)?;
    let rest = s[used..].strip_prefix(" +")?;
    let (after, _) = count(rest)?;
    Some((before, after))
}

fn one_patch_id<'a>(
    lines: &mut impl Iterator<Item = &'a [u8]>,
    stable: bool,
    verbatim: bool,
) -> (Option<String>, [u8; 20], usize) {
    let mut h = Sha1::new();
    let mut sum = [0u8; 20];
    let (mut before, mut after) = (-1i64, -1i64);
    let mut len = 0;
    let mut binary = false;
    let (mut pre, mut post) = (Vec::new(), Vec::new());
    let mut next = None;
    for line in lines {
        let rest = line
            .strip_prefix(b"commit ")
            .or_else(|| line.strip_prefix(b"From "));
        if rest.is_none() && line.starts_with(b"\\ ") && line.len() > 12 {
            if verbatim {
                h.update(line);
            }
            continue;
        }
        let p = rest.unwrap_or(line);
        if p.len() >= 40 && p[..40].iter().all(u8::is_ascii_hexdigit) {
            next = Some(String::from_utf8_lossy(&p[..40]).to_ascii_lowercase());
            break;
        }
        if len == 0 && !line.starts_with(b"diff ") {
            continue;
        }
        if before == -1 {
            if line.starts_with(b"GIT binary patch") || line.starts_with(b"Binary files") {
                binary = true;
                before = 0;
                h.update(&pre);
                h.update(&post);
                if stable {
                    flush(&mut sum, &mut h);
                }
                continue;
            } else if let Some(rest) = line.strip_prefix(b"index ") {
                if let Some(dots) = rest.windows(2).position(|w| w == b"..") {
                    let tail = &rest[dots + 2..];
                    let end = tail
                        .iter()
                        .position(|&b| b == b' ')
                        .unwrap_or(tail.len().saturating_sub(1));
                    pre = rest[..dots].to_vec();
                    post = tail[..end].to_vec();
                }
                continue;
            } else if line.starts_with(b"--- ") {
                (before, after) = (1, 1);
            } else if !line.first().is_some_and(u8::is_ascii_alphabetic) {
                break;
            }
        }
        if binary {
            if line.starts_with(b"diff ") {
                binary = false;
                before = -1;
            }
            continue;
        }
        if before == 0 && after == 0 {
            if line.starts_with(b"@@ -") {
                if let Some((b, a)) = hunk_counts(line) {
                    (before, after) = (b, a);
                }
                continue;
            }
            if !line.starts_with(b"diff ") {
                break;
            }
            if stable {
                flush(&mut sum, &mut h);
            }
            (before, after) = (-1, -1);
        }
        match line.first() {
            Some(b'-') => before -= 1,
            Some(b'+') => after -= 1,
            Some(b' ') => {
                before -= 1;
                after -= 1;
            }
            _ => {}
        }
        if verbatim {
            h.update(line);
            len += line.len();
        } else {
            let squeezed: Vec<u8> = line
                .iter()
                .copied()
                .filter(|b| !b.is_ascii_whitespace())
                .collect();
            h.update(&squeezed);
            len += squeezed.len();
        }
    }
    flush(&mut sum, &mut h);
    (next, sum, len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stripspace_squeezes_and_trims() {
        assert_eq!(
            stripspace(b"\n\na  \n\n\n#c\nb", Some("#")),
            b"a\n\nb\n".to_vec()
        );
        assert_eq!(comment_lines(b"a\n\tb\n\nc", "#"), b"# a\n#\tb\n#\n# c\n");
    }

    #[test]
    fn ref_format_rules() {
        assert!(check_ref_format("a/b", false, false));
        assert!(!check_ref_format("a", false, false));
        assert!(check_ref_format("a", true, false));
        for bad in [
            "a/.b", "a..b", "a/b.lock", "a/@{b", "@", "a//b", "a/b/", "a/b.",
        ] {
            assert!(!check_ref_format(bad, true, false), "{bad}");
        }
        assert!(check_ref_format("a/*", false, true));
        assert!(!check_ref_format("a/*/*", false, true));
        assert_eq!(collapse_slashes("//a//b"), "a/b");
    }
}
