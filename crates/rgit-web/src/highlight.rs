//! Server-side syntax highlighting via syntect, emitted as CSS classes so the
//! colors follow the page theme. The class stylesheet is generated once for a
//! light and a dark theme; per-blob highlighting produces class-tagged spans.

use std::sync::OnceLock;

use syntect::highlighting::ThemeSet;
use syntect::html::{ClassStyle, css_for_theme_with_class_style, line_tokens_to_classed_spans};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

const STYLE: ClassStyle = ClassStyle::SpacedPrefixed { prefix: "hl-" };

fn syntaxes() -> &'static SyntaxSet {
    // two-face bundles bat's full syntax set (TOML, TSX, Dockerfile, and hundreds
    // more) rather than syntect's small default set - a bigger binary for broad
    // language coverage.
    static S: OnceLock<SyntaxSet> = OnceLock::new();
    S.get_or_init(two_face::syntax::extra_newlines)
}

/// Highlight a file's text into class-tagged HTML spans, choosing the syntax by
/// extension (then first line, then plain text). Falls back to escaped text if
/// highlighting fails, so a page never renders raw markup.
pub fn highlight(path: &str, text: &str) -> String {
    spans(pick_syntax(path, text), text)
}

/// Choose a syntax for a file. Detect the language with hyperpolyglot (GitHub
/// Linguist's data + content heuristics: `.h` -> C vs C++, `.pl` -> Perl vs
/// Prolog, ...), map that to a syntect syntax, then fall back to extension /
/// first line / plain text.
fn pick_syntax(path: &str, text: &str) -> &'static SyntaxReference {
    let ss = syntaxes();
    if let Some(syntax) = detect_language(path, text).and_then(|lang| syntax_for_language(ss, &lang))
    {
        return syntax;
    }
    let base = path.rsplit('/').next().unwrap_or(path);
    let ext = base.rsplit('.').next().unwrap_or("");
    ss.find_syntax_by_extension(base)
        .or_else(|| ss.find_syntax_by_extension(ext))
        .or_else(|| ss.find_syntax_by_first_line(text.lines().next().unwrap_or("")))
        .unwrap_or_else(|| ss.find_syntax_plain_text())
}

/// Detect a file's language name via hyperpolyglot. It needs a real file, so the
/// blob is written to a uniquely-named temp dir under its true basename (so
/// filename and content heuristics both apply), then removed.
fn detect_language(path: &str, text: &str) -> Option<String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let base = path.rsplit('/').next().unwrap_or(path);
    if base.is_empty() {
        return None;
    }
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rgit-detect-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let file = dir.join(base);
    let lang = std::fs::write(&file, text.as_bytes())
        .ok()
        .and_then(|_| hyperpolyglot::detect(&file).ok().flatten())
        .map(|d| d.language().to_owned());
    let _ = std::fs::remove_dir_all(&dir);
    lang
}

/// The file extensions a language uses, from the bundled syntax set (broad
/// coverage: Rust, Python, TOML, CMake, ...). Looks the language up by name then
/// by lowercase token; empty when it is unknown to the syntax set.
pub fn lang_extensions(lang: &str) -> Vec<String> {
    let ss = syntaxes();
    ss.find_syntax_by_name(lang)
        .or_else(|| ss.find_syntax_by_token(&lang.to_ascii_lowercase()))
        .map(|s| s.file_extensions.clone())
        .unwrap_or_default()
}

/// Map a Linguist language name to a syntect syntax (by name, then token, then a
/// few name mismatches).
fn syntax_for_language(ss: &'static SyntaxSet, lang: &str) -> Option<&'static SyntaxReference> {
    ss.find_syntax_by_name(lang)
        .or_else(|| ss.find_syntax_by_token(&lang.to_ascii_lowercase()))
        .or_else(|| {
            let ext = match lang {
                "C++" => "cpp",
                "C" => "c",
                "Objective-C" => "m",
                "Objective-C++" => "mm",
                "Shell" => "sh",
                "Makefile" => "make",
                "reStructuredText" => "rst",
                "Markdown" => "md",
                "Jupyter Notebook" => "json",
                _ => return None,
            };
            ss.find_syntax_by_extension(ext)
        })
}

/// Highlight `code` as a fenced code block for a rendered Markdown preview,
/// choosing the syntax by the fence's language token. Wrapped in `<pre><code>`,
/// class-tagged like the blob view; plain-escaped when the language is unknown.
pub fn code_block_html(lang: &str, code: &str) -> String {
    let ss = syntaxes();
    let syntax = (!lang.is_empty())
        .then(|| ss.find_syntax_by_token(lang))
        .flatten();
    match syntax {
        Some(syntax) => format!("<pre><code>{}</code></pre>", spans(syntax, code)),
        None => format!("<pre><code>{}</code></pre>", escape(code)),
    }
}

/// One-syntax highlighting of multi-line text: emit each line's classed spans,
/// one line per source line. The two-face `extra_newlines` syntaxes keep the
/// source newline inside the trailing span (not at the very end of the string),
/// so drop that embedded newline and add a single plain separator - otherwise
/// every line renders as two under `white-space:pre`.
fn spans(syntax: &SyntaxReference, text: &str) -> String {
    let ss = syntaxes();
    let mut parse = ParseState::new(syntax);
    let mut scope = ScopeStack::new();
    let mut out = String::new();
    for line in LinesWithEndings::from(text) {
        let mut html = parse
            .parse_line(line, ss)
            .ok()
            .and_then(|ops| line_tokens_to_classed_spans(line, &ops, STYLE, &mut scope).ok())
            .map(|(html, _)| html)
            .unwrap_or_else(|| escape(line));
        if let Some(pos) = html.rfind('\n') {
            html.remove(pos);
        }
        out.push_str(&html);
        out.push('\n');
    }
    out
}

/// Pick a syntax for a diff by extension only. Diffs highlight many files, so
/// this skips the hyperpolyglot content detection (a temp-file write per file)
/// that the blob view can afford; extension lookup is enough for a diff.
pub fn syntax_for_path(path: &str) -> &'static SyntaxReference {
    let ss = syntaxes();
    let base = path.rsplit('/').next().unwrap_or(path);
    let ext = base.rsplit('.').next().unwrap_or("");
    ss.find_syntax_by_extension(base)
        .or_else(|| ss.find_syntax_by_extension(ext))
        .unwrap_or_else(|| ss.find_syntax_plain_text())
}

/// Highlight one line in isolation for a diff view. Hunks skip lines, so there is
/// no reliable cross-line parse state; a multi-line string or comment is only
/// approximate, but keywords, strings, and comments within a line are colored.
pub fn highlight_fragment(syntax: &SyntaxReference, text: &str) -> String {
    let ss = syntaxes();
    let mut parse = ParseState::new(syntax);
    let mut scope = ScopeStack::new();
    let Ok(ops) = parse.parse_line(text, ss) else {
        return escape(text);
    };
    match line_tokens_to_classed_spans(text, &ops, STYLE, &mut scope) {
        // Close any spans still open at the line's end so the fragment is
        // self-contained; otherwise the unclosed spans nest each diff line inside
        // the previous one and the left padding compounds into a staircase.
        Ok((mut html, open)) => {
            for _ in 0..open.max(0) {
                html.push_str("</span>");
            }
            html
        }
        Err(_) => escape(text),
    }
}

/// The class stylesheet for both themes: a light default, and a dark set applied
/// under the OS dark scheme and the explicit dark toggle (mirrors the page's own
/// token pattern via CSS nesting).
pub fn css() -> &'static str {
    static C: OnceLock<String> = OnceLock::new();
    C.get_or_init(|| {
        let ts = ThemeSet::load_defaults();
        let light = ts
            .themes
            .get("InspiredGitHub")
            .map(|t| css_for_theme_with_class_style(t, STYLE).unwrap_or_default())
            .unwrap_or_default();
        let dark = ts
            .themes
            .get("base16-ocean.dark")
            .map(|t| css_for_theme_with_class_style(t, STYLE).unwrap_or_default())
            .unwrap_or_default();
        format!(
            "{light}\n@media (prefers-color-scheme:dark){{:root:not([data-theme=\"light\"]){{\n{dark}\n}}}}\n:root[data-theme=\"dark\"]{{\n{dark}\n}}\n"
        )
    })
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
