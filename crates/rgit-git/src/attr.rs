//! `git check-attr`: git's attribute stack (attr.c) - the files, their
//! priority, macros and "first seen" attribute order for `--all`.

use std::path::Path;

use git2::Repository;
use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::GitError;

#[derive(Clone, PartialEq)]
enum Value {
    /// `!attr`: unspecified, but decided.
    Unset,
    True,
    False,
    Str(String),
}

struct Line {
    /// A macro's name, or the pattern's matcher.
    macro_name: Option<String>,
    pattern: Option<Gitignore>,
    states: Vec<(String, Value)>,
}

/// One attributes file and the folder (`""` or `dir/`) its patterns are under.
struct Frame {
    base: String,
    lines: Vec<Line>,
}

/// git's `unquote_c_style`, for a quoted pattern; returns it and the rest.
fn unquote(s: &str) -> Option<(String, &str)> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => return Some((String::from_utf8_lossy(&out).into_owned(), &s[i + 1..])),
            b'\\' => {
                i += 1;
                let c = *b.get(i)?;
                out.push(match c {
                    b'a' => 7,
                    b'b' => 8,
                    b't' => b'\t',
                    b'n' => b'\n',
                    b'v' => 11,
                    b'f' => 12,
                    b'r' => b'\r',
                    b'0'..=b'7' => {
                        let oct = s.get(i..i + 3)?;
                        i += 2;
                        u8::from_str_radix(oct, 8).ok()?
                    }
                    c => c,
                });
            }
            c => out.push(c),
        }
        i += 1;
    }
    None
}

fn parse(text: &str, base: &str, macros_ok: bool, order: &mut Vec<String>) -> Frame {
    let mut lines = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_start_matches([' ', '\t', '\r', '\n']);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (pat, rest) = if let Some(q) = line.strip_prefix('"') {
            match unquote(q) {
                Some(x) => x,
                None => continue,
            }
        } else {
            let end = line.find([' ', '\t', '\r', '\n']).unwrap_or(line.len());
            (line[..end].to_owned(), &line[end..])
        };
        let macro_name = pat.strip_prefix("[attr]").map(str::to_owned);
        if let Some(m) = &macro_name {
            if !macros_ok {
                continue;
            }
            add(order, m);
        } else if pat.starts_with('!') {
            continue;
        }
        let mut states = Vec::new();
        for s in rest.split_ascii_whitespace() {
            let (name, value) = if let Some(n) = s.strip_prefix('-') {
                (n, Value::False)
            } else if let Some(n) = s.strip_prefix('!') {
                (n, Value::Unset)
            } else if let Some((n, v)) = s.split_once('=') {
                (n, Value::Str(v.to_owned()))
            } else {
                (s, Value::True)
            };
            add(order, name);
            states.push((name.to_owned(), value));
        }
        let pattern = if macro_name.is_some() {
            None
        } else {
            let mut b = GitignoreBuilder::new(format!("/{base}"));
            if b.add_line(None, &pat).is_err() {
                continue;
            }
            match b.build() {
                Ok(g) => Some(g),
                Err(_) => continue,
            }
        };
        lines.push(Line {
            macro_name,
            pattern,
            states,
        });
    }
    Frame {
        base: base.to_owned(),
        lines,
    }
}

fn add(order: &mut Vec<String>, name: &str) {
    if !name.is_empty() && !order.iter().any(|n| n == name) {
        order.push(name.to_owned());
    }
}

/// git's `fill_one`: a line's states, last first, each only if still
/// undecided, a set macro expanding in place.
fn fill_one(
    states: &[(String, Value)],
    macros: &[(String, Vec<(String, Value)>)],
    got: &mut Vec<(String, Value)>,
) {
    for (name, value) in states.iter().rev() {
        if got.iter().any(|(n, _)| n == name) {
            continue;
        }
        got.push((name.clone(), value.clone()));
        if *value == Value::True
            && let Some((_, m)) = macros.iter().find(|(n, _)| n == name)
        {
            fill_one(m, macros, got);
        }
    }
}

/// `git check-attr`'s answers as (path index, attribute, value) rows, the
/// value being `set`, `unset`, `unspecified` or the value. `paths` are
/// relative to the top level; with no `attrs`, every attribute that is
/// decided, in git's order.
pub fn check_attr(
    git_dir: &Path,
    attrs: &[String],
    paths: &[String],
    cached: bool,
) -> Result<Vec<(usize, String, String)>, GitError> {
    let repo = Repository::open(git_dir)?;
    let top = repo.workdir().map(Path::to_path_buf);
    let index = repo.index().ok();
    let read = |rel: &str| -> Option<String> {
        if !cached && let Some(t) = &top {
            return std::fs::read_to_string(t.join(rel)).ok();
        }
        let e = index.as_ref()?.get_path(Path::new(rel), 0)?;
        let blob = repo.find_blob(e.id).ok()?;
        Some(String::from_utf8_lossy(blob.content()).into_owned())
    };
    let mut order: Vec<String> = attrs.to_vec();
    // Lowest priority first: builtins, global, top level, then (per path)
    // folders, and info/attributes above all.
    let mut low = vec![parse(
        "[attr]binary -diff -merge -text",
        "",
        true,
        &mut order,
    )];
    let config = repo.config()?;
    let global = config.get_path("core.attributesFile").ok().or_else(|| {
        let xdg = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|x| !x.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))?;
        Some(xdg.join("git/attributes"))
    });
    if let Some(text) = global.and_then(|g| std::fs::read_to_string(g).ok()) {
        low.push(parse(&text, "", true, &mut order));
    }
    if let Some(text) = read(".gitattributes") {
        low.push(parse(&text, "", true, &mut order));
    }
    let info = std::fs::read_to_string(repo.commondir().join("info/attributes"))
        .map(|t| parse(&t, "", true, &mut order))
        .ok();
    let mut dirs: Vec<Frame> = Vec::new();
    let mut rows = Vec::new();
    for (i, path) in paths.iter().enumerate() {
        let mut stack: Vec<&Frame> = low.iter().collect();
        let mut dir = String::new();
        let parts: Vec<&str> = path.split('/').collect();
        for part in &parts[..parts.len().saturating_sub(1)] {
            dir.push_str(part);
            dir.push('/');
            if !dirs.iter().any(|f| f.base == dir)
                && let Some(text) = read(&format!("{dir}.gitattributes"))
            {
                dirs.push(parse(&text, &dir, false, &mut order));
            }
        }
        let mut chain: Vec<&Frame> = dirs.iter().filter(|f| path.starts_with(&f.base)).collect();
        chain.sort_by_key(|f| f.base.len());
        stack.extend(chain);
        stack.extend(info.iter());
        let mut macros: Vec<(String, Vec<(String, Value)>)> = Vec::new();
        for f in stack.iter().rev() {
            for l in f.lines.iter().rev() {
                if let Some(m) = &l.macro_name
                    && !macros.iter().any(|(n, _)| n == m)
                {
                    macros.push((m.clone(), l.states.clone()));
                }
            }
        }
        let is_dir = path.ends_with('/');
        let abs = format!("/{}", path.trim_end_matches('/'));
        let mut got = Vec::new();
        for f in stack.iter().rev() {
            if !path.starts_with(&f.base) {
                continue;
            }
            for l in f.lines.iter().rev() {
                if let Some(g) = &l.pattern
                    && g.matched(&abs, is_dir).is_ignore()
                {
                    fill_one(&l.states, &macros, &mut got);
                }
            }
        }
        let names = if attrs.is_empty() { &order } else { attrs };
        for name in names {
            let value = match got.iter().find(|(n, _)| n == name).map(|(_, v)| v) {
                Some(Value::True) => "set".to_owned(),
                Some(Value::False) => "unset".to_owned(),
                Some(Value::Str(s)) => s.clone(),
                Some(Value::Unset) | None if attrs.is_empty() => continue,
                Some(Value::Unset) | None => "unspecified".to_owned(),
            };
            rows.push((i, name.clone(), value));
        }
    }
    Ok(rows)
}
