//! git's whitespace rules (ws.c): core.whitespace, the `whitespace`
//! attribute, and checking and fixing one line against them.

use std::path::Path;

pub const BLANK_AT_EOL: u32 = 0o100;
pub const SPACE_BEFORE_TAB: u32 = 0o200;
pub const INDENT_WITH_NON_TAB: u32 = 0o400;
pub const CR_AT_EOL: u32 = 0o1000;
pub const BLANK_AT_EOF: u32 = 0o2000;
pub const TAB_IN_INDENT: u32 = 0o4000;
const TRAILING_SPACE: u32 = BLANK_AT_EOL | BLANK_AT_EOF;
const TAB_WIDTH_MASK: u32 = 0o77;
const DEFAULT_RULE: u32 = TRAILING_SPACE | SPACE_BEFORE_TAB | 8;

/// (name, bits, loosens an error, left out of `whitespace` set to true)
const NAMES: [(&str, u32, bool, bool); 7] = [
    ("trailing-space", TRAILING_SPACE, false, false),
    ("space-before-tab", SPACE_BEFORE_TAB, false, false),
    ("indent-with-non-tab", INDENT_WITH_NON_TAB, false, false),
    ("cr-at-eol", CR_AT_EOL, true, false),
    ("blank-at-eol", BLANK_AT_EOL, false, false),
    ("blank-at-eof", BLANK_AT_EOF, false, false),
    ("tab-in-indent", TAB_IN_INDENT, false, true),
];

/// parse_whitespace_rule: the default rule changed by a comma list.
pub fn parse(spec: &str) -> u32 {
    let mut rule = DEFAULT_RULE;
    for item in spec.split(',') {
        let item = item.trim_start_matches([' ', '\t', '\n', '\r']);
        let (negated, name) = match item.strip_prefix('-') {
            Some(n) => (true, n),
            None => (false, item),
        };
        if name.is_empty() {
            break;
        }
        if let Some((_, bits, ..)) = NAMES.iter().find(|n| n.0.starts_with(name)) {
            if negated {
                rule &= !bits;
            } else {
                rule |= bits;
            }
        }
        if let Some(w) = name.strip_prefix("tabwidth=") {
            let digits: String = w.chars().take_while(char::is_ascii_digit).collect();
            let w: u32 = digits.parse().unwrap_or(0);
            if (1..0o100).contains(&w) {
                rule = (rule & !TAB_WIDTH_MASK) | w;
            }
        }
    }
    rule
}

/// whitespace_rule: the rule for `path` from core.whitespace and its
/// `whitespace` attribute.
pub fn rule_for(git_dir: Option<&Path>, path: &str) -> u32 {
    let Some(git_dir) = git_dir else {
        return DEFAULT_RULE;
    };
    let cfg = git2::Repository::open(git_dir)
        .ok()
        .and_then(|r| r.config().ok()?.get_string("core.whitespace").ok())
        .map_or(DEFAULT_RULE, |s| parse(&s));
    let attr = crate::attr::check_attr(
        git_dir,
        &["whitespace".to_owned()],
        &[path.to_owned()],
        false,
    )
    .ok()
    .and_then(|rows| rows.into_iter().next())
    .map(|(_, _, v)| v);
    match attr.as_deref() {
        Some("set") => NAMES
            .iter()
            .filter(|n| !n.2 && !n.3)
            .fold(cfg & TAB_WIDTH_MASK, |r, n| r | n.1),
        Some("unset") => cfg & TAB_WIDTH_MASK,
        Some("unspecified") | None => cfg,
        Some(v) => parse(v),
    }
}

fn tab_width(rule: u32) -> usize {
    (rule & TAB_WIDTH_MASK) as usize
}

/// ws_check: the rules `line` (no diff prefix) breaks.
pub fn check(line: &[u8], rule: u32) -> u32 {
    let mut len = line.len();
    if len > 0 && line[len - 1] == b'\n' {
        len -= 1;
    }
    if rule & CR_AT_EOL != 0 && len > 0 && line[len - 1] == b'\r' {
        len -= 1;
    }
    let mut result = 0;
    let mut trailing = len;
    if rule & BLANK_AT_EOL != 0 {
        while trailing > 0 && line[trailing - 1].is_ascii_whitespace() {
            trailing -= 1;
            result |= BLANK_AT_EOL;
        }
    }
    let mut written = 0;
    let mut i = 0;
    while i < trailing {
        match line[i] {
            b' ' => {}
            b'\t' => {
                if rule & SPACE_BEFORE_TAB != 0 && written < i {
                    result |= SPACE_BEFORE_TAB;
                } else if rule & TAB_IN_INDENT != 0 {
                    result |= TAB_IN_INDENT;
                }
                written = i + 1;
            }
            _ => break,
        }
        i += 1;
    }
    if rule & INDENT_WITH_NON_TAB != 0 && i - written >= tab_width(rule) {
        result |= INDENT_WITH_NON_TAB;
    }
    result
}

/// whitespace_error_string.
pub fn error_string(ws: u32) -> String {
    let mut err: Vec<&str> = Vec::new();
    if ws & TRAILING_SPACE == TRAILING_SPACE {
        err.push("trailing whitespace");
    } else {
        if ws & BLANK_AT_EOL != 0 {
            err.push("trailing whitespace");
        }
        if ws & BLANK_AT_EOF != 0 {
            err.push("new blank line at EOF");
        }
    }
    for (bit, what) in [
        (SPACE_BEFORE_TAB, "space before tab in indent"),
        (INDENT_WITH_NON_TAB, "indent with spaces"),
        (TAB_IN_INDENT, "tab in indent"),
    ] {
        if ws & bit != 0 {
            err.push(what);
        }
    }
    err.join(", ")
}

/// ws_fix_copy: `src` with its whitespace errors fixed, and whether any
/// were.
pub fn fix(src: &[u8], rule: u32) -> (Vec<u8>, bool) {
    let mut dst = Vec::with_capacity(src.len());
    let mut len = src.len();
    let (mut nl, mut cr, mut fixed) = (false, false, false);
    if rule & BLANK_AT_EOL != 0 {
        if len > 0 && src[len - 1] == b'\n' {
            nl = true;
            len -= 1;
            if len > 0 && src[len - 1] == b'\r' {
                cr = rule & CR_AT_EOL != 0;
                len -= 1;
            }
        }
        if len > 0 && src[len - 1].is_ascii_whitespace() {
            while len > 0 && src[len - 1].is_ascii_whitespace() {
                len -= 1;
            }
            fixed = true;
        }
    }
    let (mut last_tab, mut last_space) = (-1isize, -1isize);
    let mut need_fix = false;
    for (i, &ch) in src[..len].iter().enumerate() {
        let i = i as isize;
        match ch {
            b'\t' => {
                last_tab = i;
                if rule & SPACE_BEFORE_TAB != 0 && last_space >= 0 {
                    need_fix = true;
                }
            }
            b' ' => {
                last_space = i;
                if rule & INDENT_WITH_NON_TAB != 0 && tab_width(rule) as isize <= i - last_tab {
                    need_fix = true;
                }
            }
            _ => break,
        }
    }
    let mut start = 0;
    if need_fix {
        let last = if rule & INDENT_WITH_NON_TAB != 0 {
            last_tab.max(last_space) + 1
        } else {
            last_tab + 1
        } as usize;
        let mut spaces = 0;
        for &ch in &src[..last] {
            if ch != b' ' {
                spaces = 0;
                dst.push(ch);
            } else {
                spaces += 1;
                if spaces == tab_width(rule) {
                    dst.push(b'\t');
                    spaces = 0;
                }
            }
        }
        dst.extend(std::iter::repeat_n(b' ', spaces));
        start = last;
        fixed = true;
    } else if rule & TAB_IN_INDENT != 0 && last_tab >= 0 {
        let last = (last_tab + 1) as usize;
        for &ch in &src[..last] {
            if ch == b'\t' {
                dst.push(b' ');
                while dst.len() % tab_width(rule) != 0 {
                    dst.push(b' ');
                }
            } else {
                dst.push(ch);
            }
        }
        start = last;
        fixed = true;
    }
    dst.extend_from_slice(&src[start..len]);
    if cr {
        dst.push(b'\r');
    }
    if nl {
        dst.push(b'\n');
    }
    (dst, fixed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_and_fixes_like_ws_c() {
        let rule = parse("");
        assert_eq!(check(b"a  \n", rule), BLANK_AT_EOL);
        assert_eq!(check(b"  \tb\n", rule), SPACE_BEFORE_TAB);
        assert_eq!(fix(b"  \tb  \n", rule), (b"\tb\n".to_vec(), true));
        assert_eq!(fix(b"ok\n", rule), (b"ok\n".to_vec(), false));
        let tabs = parse("tab-in-indent,tabwidth=4");
        assert_eq!(fix(b"\tx\n", tabs).0, b"    x\n");
        assert_eq!(
            error_string(BLANK_AT_EOL | SPACE_BEFORE_TAB),
            "trailing whitespace, space before tab in indent"
        );
    }
}
