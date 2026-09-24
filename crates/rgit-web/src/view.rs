//! Server-rendered pages (maud). Every view is a real URL; the JS layer only
//! adds keyboard motion on top of complete HTML.
//!
//! `base` is the per-repo URL prefix: empty in single-repo mode (`/log`), or
//! `/{repo}` when serving a managed root (`/{repo}/log`). Every internal link is
//! built from it so both modes share one set of views.

use std::collections::HashMap;

use maud::{DOCTYPE, Markup, PreEscaped, html};
use rgit_git::{
    BlameLine, Blob, CommitOverview, CommitRef, Deco, FileDiff, GrepMatch, Head, LanesState,
    LastCommit, LineOrigin, LogEntry, RefEntry, RefKind, RepoStatus, StatusEntry, TagInfo,
    TreeEntry, group_decorations,
};

use crate::{assets, highlight};

const TABS: [(&str, &str, &str); 5] = [
    ("summary", "1", ""),
    ("log", "2", "/log"),
    ("tree", "3", "/tree"),
    ("refs", "4", "/refs"),
    ("releases", "5", "/releases"),
];

/// Repo metadata for the sidebar, gathered once per request.
pub struct SideInfo {
    pub description: Option<String>,
    pub branches: usize,
    pub tags: usize,
    pub clone_url: Option<String>,
    /// URL of the source tarball for this repo (base-aware).
    pub archive: String,
    /// All remotes as (name, url), for the header clone popover.
    pub remotes: Vec<(String, String)>,
    /// Language breakdown as (label, percent, css-class), largest first.
    pub languages: Vec<(String, u32, String)>,
    /// Top contributors as (name, email, commit-count); empty except on summary.
    pub contributors: Vec<(String, String, usize)>,
    /// The latest release as (tag, age, message); set only on the summary.
    pub release: Option<(String, String, String)>,
    /// Local branch names, for the header ref switcher.
    pub branch_names: Vec<String>,
    /// Tag names, for the header ref switcher.
    pub tag_names: Vec<String>,
}

/// A link under the repo base. `rest` starts with `/`, or is empty for the
/// summary root.
fn href(base: &str, rest: &str) -> String {
    if rest.is_empty() {
        if base.is_empty() {
            "/".to_owned()
        } else {
            base.to_owned()
        }
    } else {
        format!("{base}{rest}")
    }
}

/// A navigation link that carries the browse ref (`?ref=`) when it is not HEAD,
/// so browsing a branch stays on that branch. `rest` may already have a query.
fn at(base: &str, rev: &str, rest: &str) -> String {
    let url = href(base, rest);
    if rev == "HEAD" {
        url
    } else {
        let sep = if url.contains('?') { '&' } else { '?' };
        format!("{url}{sep}ref={}", q_encode(rev))
    }
}

/// The page shell: head, top bar, keyed tab strip, content + sidebar, status line.
/// `ctx_line` is the status-line context (branch/path/sha); `ctx_card` an
/// optional "At point" card pinned above the sidebar.
#[allow(clippy::too_many_arguments)]
/// The header code-search box. `scope` renders a `repo:` pill (multi-repo repo
/// pages); the empty `.aclist` is filled by the autocomplete JS in `assets`.
fn search_box(action: &str, scope: Option<&str>, placeholder: &str) -> Markup {
    html! {
        form.hsearch method="get" action=(action) {
            div.sbox {
                @if let Some(s) = scope { span.scope { "repo:" (s) } }
                input type="search" name="q" placeholder=(placeholder) autocomplete="off";
                span.slash { "/" }
            }
            div.aclist {}
        }
    }
}

/// The searchable branch/tag dropdown in the tab bar. A styled button opens a
/// filterable list (Commit context when at a bare rev, then HEAD, branches, and
/// tags); the JS in `assets` drives the open/filter/keyboard behavior.
fn ref_switcher(base: &str, rev: &str, side: &SideInfo) -> Markup {
    let is_named = rev == "HEAD"
        || side.branch_names.iter().any(|b| b == rev)
        || side.tag_names.iter().any(|t| t == rev);
    html! {
        div.refsw id="refsw" {
            button.refbtn type="button" id="refbtn" title="Browse a branch or tag" {
                span.rbi { "\u{2387}" }
                span.rbl { (rev) }
                span.rbc { "\u{25be}" }
            }
            div.refpop id="refpop" {
                div.rpf { input id="ref-input" placeholder="find a branch or tag\u{2026}" autocomplete="off"; }
                div.rpk id="ref-kinds" {
                    button type="button" class="rpk-pill on" data-kind="all" { "All" }
                    button type="button" class="rpk-pill" data-kind="branch" { "Branches" span.rpk-n { (side.branch_names.len()) } }
                    button type="button" class="rpk-pill" data-kind="tag" { "Tags" span.rpk-n { (side.tag_names.len()) } }
                }
                div.rpl id="ref-list" {
                    @if !is_named {
                        div.rpg data-kind="commit" { "Commit" }
                        a class="refitem on" data-kind="commit" data-ref=(rev) href=(at(base, rev, "/tree")) { (rev) }
                    }
                    a class=(if rev == "HEAD" { "refitem on" } else { "refitem" }) data-kind="branch" data-ref="HEAD" href=(href(base, "/tree")) { "HEAD" }
                    @if !side.branch_names.is_empty() {
                        div.rpg data-kind="branch" { "Branches" }
                        @for b in &side.branch_names {
                            a class=(if rev == *b { "refitem on" } else { "refitem" }) data-kind="branch" data-ref=(b) href=(at(base, b, "/tree")) { (b) }
                        }
                    }
                    @if !side.tag_names.is_empty() {
                        div.rpg data-kind="tag" { "Tags" }
                        @for t in &side.tag_names {
                            a class=(if rev == *t { "refitem on" } else { "refitem" }) data-kind="tag" data-ref=(t) href=(at(base, t, "/tree")) { (t) }
                        }
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn layout(
    repo: &str,
    base: &str,
    active: &str,
    perma: &str,
    side: &SideInfo,
    ctx_line: &str,
    ctx_card: Option<Markup>,
    rev: &str,
    body: Markup,
) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { "rgit / " (repo) }
                link rel="preconnect" href="https://fonts.googleapis.com";
                link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@400;500;700&family=Hanken+Grotesk:wght@400;500;600;700&display=swap";
                style { (PreEscaped(assets::CSS)) }
                style { (PreEscaped(highlight::css())) }
            }
            body data-base=(base) data-permalink=(perma) {
                header {
                    a.logo.m href="/" { "r" b { "git" } " " span style="color:var(--dim);font-weight:400" { "serve" } }
                    span.path { a.m href="/" { "repos" } " / " span.m style="color:var(--acc)" { (repo) } }
                    (search_box(&href(base, "/search"), (!base.is_empty()).then_some(repo), "search code\u{2026}"))
                    div.cw {
                        button.btn.pri id="cloneBtn" { "\u{2913} Clone" }
                        div.cpop id="cpop" {
                            div.ch2 { "Clone this repository" }
                            @for (name, url) in &side.remotes {
                                div.copyrow { span.rn { (name) } code { (url) } button data-copy-text=(url) title="copy" { "\u{29c9}" } }
                            }
                            @if side.remotes.is_empty() { p.hint { "no remotes configured" } }
                            div.dl2 { a.dl href=(side.archive) { "\u{2193} source.tar.gz" } }
                        }
                    }
                    a.chip-mcp href="/agent" {
                        span.dot {}
                        "MCP"
                        span.chip-tip {
                            b { "Connect an agent (MCP)" }
                            span.tiprow { span.tipk { "HTTP" } code.mcpchipurl { "/mcp" } span.tipro { "read-only" } }
                            span.tiprow { span.tipk { "stdio" } code { "rgit mcp" } span.tipro { "full" } }
                            span.tipmore { "Click for all tools and setup \u{2192}" }
                        }
                    }
                    button.ib id="theme" title="Theme" { "\u{25d1}" }
                }
                nav.tabs {
                    (ref_switcher(base, rev, side))
                    span.tabsep {}
                    @for (name, key, path) in TABS {
                        @let url = if name == "log" || name == "tree" { at(base, rev, path) } else { href(base, path) };
                        a href=(url) class=(if name == active { "on" } else { "" }) {
                            span.k { (key) } (name)
                        }
                    }
                }
                main.shell {
                    div.content data-pane="main" { (body) }
                    aside.side data-pane="side" {
                        @if let Some(c) = ctx_card { (c) }
                        (sidebar(base, side))
                    }
                }
                div.statusline {
                    span.mode { (active.to_uppercase()) }
                    @if !ctx_line.is_empty() { span.ctxl.m { (ctx_line) } }
                    span.sp {}
                    span.keys.m { b { "j/k" } " move  " b { "h/l" } " pane  " b { "t" } " find  " b { "y" } " link  " b { "?" } " keys" }
                }
                div.scrim id="scrim" {}
                div id="whichkey" {
                    h3 { "Keys" }
                    div.wk {
                        div { kbd { "j" } kbd { "k" } span.d { "move cursor" } }
                        div { kbd { "h" } kbd { "l" } span.d { "switch pane" } }
                        div { kbd { "RET" } span.d { "open at point" } }
                        div { kbd { "1" } "-" kbd { "6" } span.d { "switch view" } }
                        div { kbd { ":" } span.d { "command palette" } }
                        div { kbd { "t" } span.d { "find file" } }
                        div { kbd { "c" } span.d { "clone menu" } }
                        div { kbd { "y" } span.d { "copy permalink" } }
                        div { kbd { "Esc" } span.d { "close" } }
                    }
                }
                div.toast id="toast" { "copied" }
                div id="finder" {
                    div.finput { span.pfx { "\u{1f50d}" } input id="find-input" placeholder="find file\u{2026}" autocomplete="off"; }
                    div id="find-list" {}
                }
                div id="palette" {
                    div.finput { span.pfx { ":" } input id="pal-input" placeholder="jump to a view or action\u{2026}" autocomplete="off"; }
                    div id="pal-list" {
                        a.pitem href=(href(base, "")) { span.pic { "\u{25a4}" } "summary" }
                        a.pitem href=(href(base, "/log")) { span.pic { "\u{2630}" } "log" }
                        a.pitem href=(href(base, "/tree")) { span.pic { "\u{1f4c1}" } "tree" }
                        a.pitem href=(href(base, "/refs")) { span.pic { "\u{2325}" } "refs" }
                        a.pitem href=(href(base, "/releases")) { span.pic { "\u{2691}" } "releases" }
                        a.pitem data-act="finder" { span.pic { "\u{1f50d}" } "find file" span.phint { "t" } }
                        a.pitem href=(side.archive) { span.pic { "\u{2193}" } "download source" }
                        @if let Some(u) = &side.clone_url { a.pitem data-copy-text=(u) { span.pic { "\u{29c9}" } "copy clone URL" } }
                    }
                }
                script { (PreEscaped(assets::JS)) }
            }
        }
    }
}

/// A small "At point" card: a title over key/value rows.
fn ctx_card(title: &str, rows: &[(&str, String)]) -> Markup {
    html! {
        div.card {
            div.ch { (title) }
            div.cb {
                @for (k, v) in rows {
                    div.kvrow { span.k { (k) } span.v { (v) } }
                }
            }
        }
    }
}

/// A segmented control: one `(label, href, active)` per tab.
fn seg(tabs: &[(&str, String, bool)]) -> Markup {
    html! {
        div.seg {
            @for (label, url, on) in tabs {
                a class=(if *on { "on" } else { "" }) href=(url) { (label) }
            }
        }
    }
}

/// The header shared by the blob, blame, and working-tree diff views: the file
/// path, optional meta (size or state), a segmented control of related views,
/// and an optional trailing action.
fn file_header(
    path: &str,
    meta: Option<&str>,
    tabs: &[(&str, String, bool)],
    trailing: Option<Markup>,
) -> Markup {
    html! {
        div.filehead {
            span.p { (path) }
            @if let Some(m) = meta { span.fsz { (m) } }
            span.sp {}
            (seg(tabs))
            @if let Some(t) = trailing { (t) }
        }
    }
}

/// The `code | blame | raw` tabs for a file at `rev`, with one marked active.
fn file_tabs(base: &str, path: &str, rev: &str, active: &str) -> Vec<(&'static str, String, bool)> {
    vec![
        (
            "code",
            at(base, rev, &format!("/blob/{path}?src=1")),
            active == "code",
        ),
        (
            "blame",
            href(base, &format!("/blame/{path}")),
            active == "blame",
        ),
        (
            "raw",
            at(base, rev, &format!("/blob/{path}?raw=1")),
            active == "raw",
        ),
    ]
}

/// Percent-encode a value for a URL query (keeps unreserved chars).
fn q_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The metadata rail: description, stats, and a clone URL.
fn sidebar(base: &str, side: &SideInfo) -> Markup {
    html! {
        @if let Some(d) = &side.description {
            div.card { div.ch { "About" } div.cb { p.desc { (d) } } }
        }
        div.card {
            div.ch { "Stats" }
            div.cb {
                div.stat { span.l { "Branches" } span.v { (side.branches) } }
                div.stat { span.l { "Tags" } span.v { (side.tags) } }
                @if !side.languages.is_empty() {
                    div.langbar {
                        @for (_, pct, cls) in &side.languages {
                            i class=(format!("lb {cls}")) style=(format!("width:{pct}%")) {}
                        }
                    }
                    div.langkey {
                        @for (name, pct, cls) in &side.languages {
                            span { b class=(format!("lb {cls}")) {} (name) " " (pct) "%" }
                        }
                    }
                }
            }
        }
        @if let Some(u) = &side.clone_url {
            div.card {
                div.ch { "Clone" }
                div.cb {
                    div.miniclone {
                        code id="clone-url" { (u) }
                        button data-copy="#clone-url" title="copy" { "\u{29c9}" }
                    }
                }
            }
        }
        @if let Some((tag, when, msg)) = &side.release {
            div.card {
                div.ch { "Latest release" }
                div.cb {
                    div.rel { span.rt { (tag) } span.rw { (when) } }
                    @if !msg.is_empty() { p.relmsg { (msg) } }
                }
            }
        }
        @if !side.contributors.is_empty() {
            div.card {
                div.ch { "Contributors" }
                div.cb {
                    div.avatars {
                        @for (name, email, count) in &side.contributors {
                            @let tip = if email.is_empty() { format!("{name} \u{b7} {count} commits") } else { format!("{name} <{email}> \u{b7} {count} commits") };
                            a class="av" href=(href(base, &format!("/log?author={}", q_encode(name)))) data-tip=(tip) style=(format!("background:{}", av_color(name))) { (initials(name)) }
                        }
                    }
                }
            }
        }
        div.card {
            div.ch { "Download" }
            div.cb { a.dl href=(side.archive) { "\u{2193} source.tar.gz" } }
        }
    }
}

/// Up to two initials from a contributor's name.
fn initials(name: &str) -> String {
    let mut it = name.split_whitespace();
    let first = it.next().and_then(|w| w.chars().next());
    let second = it.next().and_then(|w| w.chars().next());
    match (first, second) {
        (Some(a), Some(b)) => format!("{a}{b}").to_uppercase(),
        (Some(a), None) => a.to_uppercase().to_string(),
        _ => "?".to_owned(),
    }
}

/// A stable avatar color for a name, from a small fixed palette.
fn av_color(name: &str) -> &'static str {
    const PALETTE: [&str; 6] = [
        "#c25a26", "#356390", "#3c8646", "#8557a6", "#b47f22", "#b23d3a",
    ];
    let sum: u32 = name.bytes().map(u32::from).sum();
    PALETTE[(sum as usize) % PALETTE.len()]
}

/// A shell for the repo index (no per-repo tabs), with a metadata sidebar.
fn plain_layout(title: &str, content: Markup, sidebar: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) }
                link rel="preconnect" href="https://fonts.googleapis.com";
                link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@400;500;700&family=Hanken+Grotesk:wght@400;500;600;700&display=swap";
                style { (PreEscaped(assets::CSS)) }
                style { (PreEscaped(highlight::css())) }
            }
            body {
                header {
                    a.logo.m href="/" { "r" b { "git" } " " span style="color:var(--dim);font-weight:400" { "serve" } }
                    (search_box("/search", None, "search all repos\u{2026}"))
                    a.chip-mcp href="/agent" {
                        span.dot {}
                        "MCP"
                        span.chip-tip {
                            b { "Connect an agent (MCP)" }
                            span.tiprow { span.tipk { "HTTP" } code.mcpchipurl { "/mcp" } span.tipro { "read-only" } }
                            span.tiprow { span.tipk { "stdio" } code { "rgit mcp" } span.tipro { "full" } }
                            span.tipmore { "Click for all tools and setup \u{2192}" }
                        }
                    }
                    button.ib id="theme" title="Theme" { "\u{25d1}" }
                }
                main.shell {
                    div.content data-pane="main" { (content) }
                    aside.side data-pane="side" { (sidebar) }
                }
                script { (PreEscaped(assets::JS)) }
            }
        }
    }
}

/// One repository on the index, with light metadata.
#[derive(Clone)]
pub struct RepoCard {
    pub name: String,
    pub description: Option<String>,
    /// The latest commit as (short id, summary, age), if any.
    pub last: Option<(String, String, String)>,
    pub branches: usize,
    pub tags: usize,
    /// The dominant language, if any.
    pub language: Option<String>,
}

/// The repo index: every repository under the managed root, as cards.
pub fn index(repos: &[RepoCard]) -> Markup {
    let total_branches: usize = repos.iter().map(|r| r.branches).sum();
    let total_tags: usize = repos.iter().map(|r| r.tags).sum();
    let mut lang_counts: std::collections::BTreeMap<&str, usize> =
        std::collections::BTreeMap::new();
    for r in repos {
        if let Some(l) = &r.language {
            *lang_counts.entry(l.as_str()).or_default() += 1;
        }
    }
    let mut langs: Vec<(&str, usize)> = lang_counts.into_iter().collect();
    langs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));

    let recent: Vec<&RepoCard> = repos.iter().filter(|r| r.last.is_some()).take(8).collect();
    let sidebar = html! {
        div.card {
            div.ch { "Overview" }
            div.cb {
                div.stat { span.l { "Repositories" } span.v { (repos.len()) } }
                div.stat { span.l { "Branches" } span.v { (total_branches) } }
                div.stat { span.l { "Tags" } span.v { (total_tags) } }
            }
        }
        @if !recent.is_empty() {
            div.card {
                div.ch { "Recent activity" }
                div.cb {
                    div.feed {
                        @for r in &recent {
                            @if let Some((id, summary, when)) = &r.last {
                                a href=(format!("/{}", r.name)) {
                                    div.fr { span.frn { (r.name) } span.fhh { (id) } span.ft { (when) } }
                                    div.fs { (summary) }
                                }
                            }
                        }
                    }
                }
            }
        }
        @if !langs.is_empty() {
            div.card {
                div.ch { "Languages" }
                div.cb {
                    @for (l, n) in &langs {
                        div.langrow {
                            span.langdot style=(format!("background:{}", av_color(l))) {}
                            span.langl { (l) }
                            span.langn { (n) }
                        }
                    }
                }
            }
        }
        div.card {
            div.ch { "Drive with an agent (MCP)" }
            div.cb {
                p.mcpsub { "Any MCP client can operate these repos. Add rgit to its config (stdio, runs in the repo):" }
                pre.mcpcmd { code { "{ \"mcpServers\": { \"rgit\": {\n  \"command\": \"rgit\", \"args\": [\"mcp\"] } } }" } }
                p.mcpsub { "Serving a single repo (" code { "rgit serve" } " in it) also exposes a " code { "POST /mcp" } " HTTP endpoint - open that repo's page for the URL and config." }
            }
        }
    };
    let content = html! {
        div.idxhead {
            h1 { "Repositories" }
            span.idxcount { (repos.len()) }
            span.sp {}
            input.repofilter id="repo-filter" placeholder="filter\u{2026}" autocomplete="off";
        }
        @if repos.is_empty() {
            div.sec { div.body { div.binary { "no git repositories in the managed root" } } }
        }
        div.repogrid {
            @for r in repos {
                a.repocard href=(format!("/{}", r.name)) data-name=(r.name) data-point {
                    div.rctop { span.rcicon { "\u{1f4e6}" } span.rcname.m { (r.name) } }
                    @if let Some(d) = &r.description { p.rcdesc { (d) } }
                    div.rcstats {
                        @if let Some(l) = &r.language {
                            span.rclang { span.langdot style=(format!("background:{}", av_color(l))) {} (l) }
                        }
                        span { (r.branches) " " (if r.branches == 1 { "branch" } else { "branches" }) }
                        span { (r.tags) " " (if r.tags == 1 { "tag" } else { "tags" }) }
                    }
                    @if let Some((id, summary, when)) = &r.last {
                        div.rclast { span.rchash.m { (id) } span.rcmsg { (summary) } span.rcwhen { (when) } }
                    } @else {
                        div.rclast { span.rcwhen { "empty repository" } }
                    }
                }
            }
        }
    };
    plain_layout("rgit / repositories", content, sidebar)
}

/// The `/agent` page: the full MCP tool catalog with per-tool instructions, and
/// the connection setup in the sidebar. Each row shows the tool name, its args,
/// and its description; hovering shows the full argument signature.
pub fn agent_tools(tools: &[crate::McpTool]) -> Markup {
    let content = html! {
        div.idxhead {
            h1 { "MCP tools" }
            span.idxcount { (tools.len()) }
            span.sp {}
            input.repofilter id="tool-filter" placeholder="filter tools\u{2026}" autocomplete="off";
        }
        p.mcpsub style="margin:0 0 14px !important" { "Every tool an agent can call over MCP. Each also takes an optional " code { "repo" } " to target another repository. The " b { "write" } " tools (marked below) mutate the repo: they run over local stdio (" code { "rgit mcp" } ") but are " b { "disabled on this server's read-only HTTP endpoint" } ". Writes are auto-snapshotted (" code { "git_undo" } " reverses the last one)." }
        @if tools.is_empty() {
            div.sec { div.body { div.binary { "tool catalog unavailable" } } }
        }
        div.toollist {
            @for t in tools {
                div class=(if t.writes { "toolrow writes" } else { "toolrow" }) data-name=(format!("{} {}", t.name, t.description).to_lowercase()) title=(tool_hover(t)) {
                    div.toolhd {
                        code.tooln { (t.name) }
                        @if t.writes { span.twrite title="mutates the repo; stdio only, not on the read-only HTTP endpoint" { "write" } }
                        @for a in &t.args {
                            span class=(if a.required { "targ req" } else { "targ" }) title=(format!("{}{}", a.ty, if a.required { ", required" } else { ", optional" })) { (a.name) }
                        }
                    }
                    p.toold { (t.description) }
                }
            }
        }
        script { (PreEscaped(r#"(function(){var f=document.getElementById('tool-filter');if(!f)return;f.addEventListener('input',function(){var q=f.value.toLowerCase();Array.prototype.forEach.call(document.querySelectorAll('.toolrow'),function(c){c.style.display=(c.getAttribute('data-name')||'').indexOf(q)>=0?'':'none';});});})();"#)) }
    };
    let sidebar = html! {
        div.card {
            div.ch { "Connect an agent" }
            (agent_body())
        }
    };
    plain_layout("rgit / mcp tools", content, sidebar)
}

/// Hover text for a tool row: its description plus the full argument signature.
fn tool_hover(t: &crate::McpTool) -> String {
    let mut sig: Vec<String> = t
        .args
        .iter()
        .map(|a| {
            format!(
                "{}: {}{}",
                a.name,
                a.ty,
                if a.required { " (required)" } else { "" }
            )
        })
        .collect();
    sig.push("repo: string (optional; target another repo)".to_owned());
    format!("{}\nargs: {}", t.description, sig.join(", "))
}

/// The refs decorating one commit, collapsed via [`group_decorations`].
fn refs_markup(refs: &[CommitRef]) -> Markup {
    html! {
        @for d in group_decorations(refs) {
            @match d {
                Deco::Local(n) => span class="ref local m" { " " (n) }
                Deco::Remote { remote, branch } => span class="ref remote m" { " " (remote) "/" (branch) }
                Deco::Tag(n) => span class="ref tag m" { " " (n) }
                Deco::Group { branch, local, remotes } => {
                    @let members = {
                        let mut v: Vec<String> = Vec::new();
                        if local { v.push("local".to_owned()); }
                        v.extend(remotes.iter().cloned());
                        v.join(",")
                    };
                    span class=(if local { "ref local m" } else { "ref remote m" }) {
                        " {" (members) "}/" (branch)
                    }
                }
            }
        }
    }
}

/// One commit row: mono graph node, hash, refs, subject, author/age.
fn commit_row(
    base: &str,
    hash: &str,
    refs: &[CommitRef],
    subject: &str,
    meta: &str,
    head: bool,
    unpushed: bool,
) -> Markup {
    html! {
        div class=(if head { "row commit-row head" } else { "row commit-row" }) data-point {
            span.node { "\u{25cf}" }
            span.hash.m { a href=(href(base, &format!("/commit/{hash}"))) { (hash) } }
            span.subj {
                a data-go href=(href(base, &format!("/commit/{hash}"))) { (subject) }
                (refs_markup(refs))
                @if unpushed { span.up { "\u{2191}" } }
            }
            span.meta { (meta) }
        }
    }
}

pub fn summary(
    repo: &str,
    base: &str,
    side: &SideInfo,
    status: &RepoStatus,
    lanes: Option<&LanesState>,
) -> Markup {
    let head = &status.head;
    let staged: Vec<&StatusEntry> = status.entries.iter().filter(|e| e.is_staged()).collect();
    let unstaged: Vec<&StatusEntry> = status
        .entries
        .iter()
        .filter(|e| e.is_unstaged() && !e.is_untracked())
        .collect();
    let untracked: Vec<&StatusEntry> = status.entries.iter().filter(|e| e.is_untracked()).collect();
    let body = html! {
        (fold_sec("Head", None, None, head_body(head)))
        @if !staged.is_empty() {
            (fold_sec("Staged", Some(staged.len()), None, change_rows(base, &staged, Change::Staged)))
        }
        @if !unstaged.is_empty() {
            (fold_sec("Unstaged", Some(unstaged.len()), None, change_rows(base, &unstaged, Change::Unstaged)))
        }
        @if !untracked.is_empty() {
            (fold_sec("Untracked", Some(untracked.len()), None, change_rows(base, &untracked, Change::Untracked)))
        }
        (fold_sec("Recent", Some(status.recent.len()),
            Some(html! { a.aside href=(href(base, "/log")) { "log \u{2192}" } }),
            html! { @for c in &status.recent { (commit_row(base, &c.short_id, &c.refs, &c.summary, &c.when, false, c.unpushed)) } }))
        @if !head.remotes.is_empty() {
            (fold_sec("Remote", Some(head.remotes.len()),
                Some(html! { span id="pr-badges" data-prs=(href(base, "/prs")) {} }), remote_body(head)))
        }
        @if let Some(l) = lanes {
            @if !l.lanes.is_empty() {
                (fold_sec("Lanes", Some(l.lanes.len()),
                    Some(html! { span.aside { "split working tree" } }), lanes_body(l)))
            }
        }
        (fold_sec("Drive with an agent", None, None, agent_body()))
    };
    let ctx_line = format!(
        "{} {}",
        head.branch.as_deref().unwrap_or("detached"),
        head.oid.as_deref().unwrap_or("")
    );
    let card = ctx_card(
        "Head",
        &[
            (
                "branch",
                head.branch.clone().unwrap_or_else(|| "detached".into()),
            ),
            ("tip", head.oid.clone().unwrap_or_default()),
            (
                "upstream",
                head.upstream.clone().unwrap_or_else(|| "-".into()),
            ),
        ],
    );
    layout(
        repo,
        base,
        "summary",
        "",
        side,
        &ctx_line,
        Some(card),
        "HEAD",
        body,
    )
}

/// A collapsible magit-style section: a caret + title header over a body.
fn fold_sec(title: &str, count: Option<usize>, aside: Option<Markup>, body: Markup) -> Markup {
    html! {
        div.sec {
            div.h {
                span.caret { "\u{25be}" }
                span.title { (title) }
                @if let Some(c) = count { span.count { (c) } }
                span.sp {}
                @if let Some(a) = aside { (a) }
            }
            div.body { (body) }
        }
    }
}

fn head_body(head: &Head) -> Markup {
    html! {
        div.headline {
            span.b { (head.branch.as_deref().unwrap_or("detached")) }
            @if let Some(oid) = &head.oid { span.m style="color:var(--acc)" { (oid) } }
            @if let Some(s) = &head.summary { span.kv { (s) } }
            @if let Some(up) = &head.upstream {
                span.pill.br {
                    span.m { (up) }
                    " " span.ah { "\u{2191}" (head.ahead) }
                    " " span.bh { "\u{2193}" (head.behind) }
                }
            } @else {
                span.pill { "not published" }
            }
        }
    }
}

fn remote_body(head: &Head) -> Markup {
    html! {
        @for (name, ahead, behind) in &head.remotes {
            div.row style="grid-template-columns:1fr auto" {
                span.m style="color:var(--br)" { (name) }
                span.m { span.ah { "\u{2191}" (ahead) } " " span.bh { "\u{2193}" (behind) } }
            }
        }
    }
}

fn lanes_body(l: &LanesState) -> Markup {
    html! {
        @for lane in &l.lanes {
            div.lane {
                div.top {
                    span.nm { (lane.name) }
                    span.bn { "[" (lane.branch) "]" }
                    @if let Some(p) = &lane.parent { span.onp { "on " (p) } }
                }
                @if !lane.paths.is_empty() {
                    div.files { @for p in &lane.paths { span.fp { (p) } " " } span.cm { "\u{b7} pending" } }
                } @else if !lane.commits.is_empty() {
                    div.files { span.cm { (lane.commits.len()) " commits \u{b7} nothing pending" } }
                }
            }
        }
    }
}

fn agent_body() -> Markup {
    // Generic MCP client config. Nearly every client (Claude, Cursor, Windsurf,
    // Zed, Continue, VS Code, ...) reads this `mcpServers` shape; a few want a
    // `type`/`transport` field or a plain URL box in their UI.
    let stdio_json = "{\n  \"mcpServers\": {\n    \"rgit\": { \"command\": \"rgit\", \"args\": [\"mcp\"] }\n  }\n}";
    let http_json = "{\n  \"mcpServers\": {\n    \"rgit\": { \"url\": \"<URL>/mcp\" }\n  }\n}";
    html! {
        div.agent {
            p { "Point any MCP client at this repository - it sees exactly what the browser sees. Reads are open; every write is auto-snapshotted, so " code { "git_undo" } " reverses the last operation." }

            div.mcpopt {
                div.mcphd { span.mcpn { "A" } "Local spawn (stdio)" }
                p.mcpsub { "The client runs the binary in the repo; no server needed. Works in any repo, any MCP client. Drop this in the client's config:" }
                pre.mcpcmd { code { (stdio_json) } }
            }

            div.mcpopt {
                div.mcphd { span.mcpn { "B" } "This running server (HTTP)" }
                p.mcpsub { "Streamable HTTP at this site's " code { "/mcp" } " - browser and agent share one process. Endpoint:" }
                div.ep { span.k { "POST" } " " code id="mcp-url" { "/mcp" } }
                p.mcpsub { "Config for a URL-based client:" }
                pre.mcpcmd { code id="mcp-http-json" { (http_json) } }
            }

            p.mcpsub { "CLI shortcuts, if your client has one: Claude Code " code { "claude mcp add rgit -- rgit mcp" } " (stdio) or " code { "claude mcp add --transport http rgit <URL>/mcp" } "; others take the JSON above." }
            p.mcpsub { "Full tool set either way (status, log, diff, blame, stage, commit, branch, stash, rebase, remotes, ...). Inspect first (" code { "git_status" } ", " code { "git_log" } "), then act." }

            // Fill the HTTP endpoint from the page's own origin so it is copy-paste ready.
            script { (maud::PreEscaped(r#"(function(){var o=location.origin;var u=document.getElementById('mcp-url');if(u)u.textContent=o+'/mcp';var j=document.getElementById('mcp-http-json');if(j)j.textContent=j.textContent.replace('<URL>/mcp',o+'/mcp');})();"#)) }
        }
    }
}

#[derive(Clone, Copy)]
enum Change {
    Staged,
    Unstaged,
    Untracked,
}

/// Working-tree rows: a status letter and the path, colored by change kind.
fn change_rows(base: &str, entries: &[&StatusEntry], kind: Change) -> Markup {
    let (cls, get): (&str, fn(&StatusEntry) -> &str) = match kind {
        Change::Staged => ("g", |e| e.index.letter()),
        Change::Unstaged => ("y", |e| e.worktree.letter()),
        Change::Untracked => ("r", |_| "?"),
    };
    let staged = matches!(kind, Change::Staged);
    html! {
        @for e in entries {
            div class="row change" data-point {
                span class=(format!("stc {cls}")) { (get(e)) }
                span.stp.m {
                    a data-go href=(href(base, &format!("/diff/{}{}", e.path, if staged { "?staged=1" } else { "" }))) { (e.path) }
                    @if let Some(o) = &e.orig_path { span.storig { " \u{2190} " (o) } }
                }
            }
        }
    }
}

/// Just the commit rows, for the initial page and the load-more fragment.
pub fn log_rows(base: &str, entries: &[LogEntry]) -> Markup {
    html! {
        @for e in entries {
            (commit_row(base, &e.short_id, &e.refs, &e.summary, &format!("{} \u{b7} {}", e.author, e.when), false, e.unpushed))
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn log(
    repo: &str,
    base: &str,
    side: &SideInfo,
    entries: &[LogEntry],
    offset: usize,
    limit: usize,
    author: Option<&str>,
    rev: &str,
) -> Markup {
    let more = entries.len() == limit;
    let aq = author
        .map(|a| format!("&author={}", q_encode(a)))
        .unwrap_or_default();
    let body = html! {
        div.sec {
            div.body {
                @if let Some(a) = author {
                    div.filterbar {
                        "commits by " span.fauthor { (a) }
                        " " a.fclear href=(at(base, rev, "/log")) { "clear \u{2717}" }
                    }
                }
                div id="log-rows" { (log_rows(base, entries)) }
                div.pager {
                    @if offset > 0 {
                        a.btn href=(at(base, rev, &format!("/log?offset={}{aq}", offset.saturating_sub(limit)))) { "\u{2190} newer" }
                    } @else {
                        span.btn.off { "\u{2190} newer" }
                    }
                    span.m style="color:var(--faint)" { "from " (offset) }
                    @if more {
                        a.btn id="more" href=(at(base, rev, &format!("/log?offset={}{aq}", offset + limit))) data-next=(offset + limit) data-author=(author.unwrap_or("")) data-ref=(rev) { "load more \u{2193}" }
                    } @else {
                        span.btn.off { "end" }
                    }
                }
            }
        }
    };
    layout(repo, base, "log", "", side, "", None, rev, body)
}

/// A lazily-expanded file tree: only folders on the open path carry children,
/// so the explorer renders the current path plus siblings, not the whole repo.
#[derive(Default)]
struct FileNode {
    dirs: std::collections::BTreeMap<String, FileNode>,
    files: Vec<String>,
}

fn build_file_tree(files: &[String]) -> FileNode {
    let mut root = FileNode::default();
    for f in files {
        let parts: Vec<&str> = f.split('/').collect();
        let mut node = &mut root;
        for (i, part) in parts.iter().enumerate() {
            if i + 1 == parts.len() {
                node.files.push((*part).to_owned());
            } else {
                node = node.dirs.entry((*part).to_owned()).or_default();
            }
        }
    }
    root
}

fn render_explorer(
    node: &FileNode,
    prefix: &str,
    open: &[&str],
    base: &str,
    rev: &str,
    current: &str,
) -> Markup {
    let mut files = node.files.clone();
    files.sort();
    html! {
        @for (name, child) in &node.dirs {
            @let full = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            @let is_open = open.first() == Some(&name.as_str());
            @if is_open {
                div class="tfolder open" {
                    div class="tnode dir" { span.caret2 { "\u{25be}" } span.eic { "\u{1f4c1}" } a href=(at(base, rev, &format!("/tree/{full}"))) { (name) } }
                    div.tkids { (render_explorer(child, &full, &open[1..], base, rev, current)) }
                }
            } @else {
                div class="tnode dir" { span.caret2 { "\u{25b8}" } span.eic { "\u{1f4c1}" } a href=(at(base, rev, &format!("/tree/{full}"))) { (name) } }
            }
        }
        @for f in &files {
            @let full = if prefix.is_empty() { f.clone() } else { format!("{prefix}/{f}") };
            div class=(if full == current { "tnode file cur" } else { "tnode file" }) {
                span.caret2 {} span.eic { "\u{1f4c4}" }
                a data-go href=(at(base, rev, &format!("/blob/{full}"))) { (f) }
            }
        }
    }
}

/// The left file explorer, expanded along `path`, with `current` (a file path)
/// highlighted. Shared by the tree and blob views.
fn explorer_aside(base: &str, rev: &str, files: &[String], path: &str, current: &str) -> Markup {
    let root = build_file_tree(files);
    let open: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    html! {
        aside.explorer data-pane="explorer" {
            div.eh { "Files" }
            div.etree { (render_explorer(&root, "", &open, base, rev, current)) }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn tree(
    repo: &str,
    base: &str,
    side: &SideInfo,
    path: &str,
    entries: &[TreeEntry],
    last: &HashMap<String, LastCommit>,
    files: &[String],
    rev: &str,
) -> Markup {
    let body = html! {
      div.treegrid {
        (explorer_aside(base, rev, files, path, ""))
        div.treemain data-pane="main" {
        div.sec {
            div.body {
                (crumb(base, rev, path))
                @for e in entries {
                    div class="row tree-row" data-point {
                        @if e.is_dir {
                            span.ic.d { "\u{1f4c1}" }
                            span.nm { a data-go href=(at(base, rev, &format!("/tree/{}", e.path))) { (e.name) } }
                        } @else {
                            span.ic.f { "\u{1f4c4}" }
                            span.nm { a data-go href=(at(base, rev, &format!("/blob/{}", e.path))) { (e.name) } }
                        }
                        span.lc {
                            @if let Some(c) = last.get(&e.path) {
                                a href=(href(base, &format!("/commit/{}", c.short_id))) { (c.summary) }
                            }
                        }
                        span.meta {
                            @if let Some(c) = last.get(&e.path) { (c.when) }
                        }
                    }
                }
            }
        }
        }
      }
    };
    let disp = if path.is_empty() {
        "/".to_owned()
    } else {
        format!("/{path}")
    };
    let card = ctx_card(
        "This dir",
        &[
            ("path", disp.clone()),
            ("entries", entries.len().to_string()),
        ],
    );
    layout(repo, base, "tree", "", side, &disp, Some(card), rev, body)
}

#[allow(clippy::too_many_arguments)]
pub fn blob(
    repo: &str,
    base: &str,
    side: &SideInfo,
    blob: &Blob,
    rev: &str,
    perma: &str,
    files: &[String],
    rendered: Option<&str>,
    previewable: bool,
) -> Markup {
    let raw = at(base, rev, &format!("/blob/{}?raw=1", blob.path));
    let dir = blob.path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let size = human_size(blob.size);
    let mut tabs: Vec<(&str, String, bool)> = Vec::new();
    if previewable {
        tabs.push((
            "preview",
            at(base, rev, &format!("/blob/{}", blob.path)),
            rendered.is_some(),
        ));
        tabs.push((
            "code",
            at(base, rev, &format!("/blob/{}?src=1", blob.path)),
            rendered.is_none(),
        ));
    } else {
        tabs.push(("code", at(base, rev, &format!("/blob/{}", blob.path)), true));
    }
    tabs.push(("blame", href(base, &format!("/blame/{}", blob.path)), false));
    tabs.push(("raw", raw.clone(), false));
    let permalink =
        html! { a data-copy="#permalink" href="#" title="copy permalink" { "permalink" } };
    let body = html! {
      div.treegrid {
        (explorer_aside(base, rev, files, dir, &blob.path))
        div.treemain data-pane="main" {
        div.sec {
            div.body {
                (file_header(&blob.path, Some(&size), &tabs, Some(permalink)))
                span id="permalink" style="display:none" { (perma) }
                @if let Some(html) = rendered {
                    div class="markdown-body" { (PreEscaped(html)) }
                } @else {
                    @match &blob.text {
                        Some(text) => (code_block(&blob.path, text)),
                        None => div.binary { "binary file (" (human_size(blob.size)) ")" }
                    }
                }
            }
        }
        }
      }
    };
    let lang = std::path::Path::new(&blob.path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("-");
    let lines = blob.text.as_ref().map(|t| t.lines().count()).unwrap_or(0);
    let card = ctx_card(
        "This file",
        &[
            ("path", blob.path.clone()),
            ("lang", lang.to_owned()),
            ("size", human_size(blob.size)),
            ("lines", lines.to_string()),
        ],
    );
    layout(
        repo,
        base,
        "tree",
        perma,
        side,
        &blob.path,
        Some(card),
        rev,
        body,
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn code_block(path: &str, text: &str) -> Markup {
    let n = text.lines().count().max(1);
    // syntect tokenizes every line (~0.5ms+/line), so skip highlighting for very
    // large files - they render instantly as plain text, like GitHub.
    let content = if text.len() > 120_000 || n > 2500 {
        html_escape(text)
    } else {
        highlight::highlight(path, text)
    };
    html! {
        div.code {
            div.g {
                @for i in 1..=n {
                    a id=(format!("L{i}")) href=(format!("#L{i}")) { (i) }
                }
            }
            div.src { pre { (PreEscaped(content)) } }
        }
    }
}

pub fn commit(repo: &str, base: &str, side: &SideInfo, d: &CommitOverview) -> Markup {
    let perma = href(base, &format!("/commit/{}", d.full_id));
    let (add, del) = d.files.iter().fold((0usize, 0usize), |(a, r), f| {
        (a + f.additions, r + f.deletions)
    });
    let files_side = html! {
        aside.difffiles data-pane="files" {
            div.dfh { span.dft { "Changed files" } span.dfn { (d.files.len()) } }
            div.dfmode {
                a.dfm.on data-point data-mode="one" { "one" }
                a.dfm data-point data-mode="list" { "list" }
            }
            div.dflist {
                @for (i, f) in d.files.iter().enumerate() {
                    a class=(if i == 0 { "dfitem on" } else { "dfitem" })
                        data-point
                        data-file=(i)
                        data-diff=(href(base, &format!("/commit/{}/diff/{}", d.id, f.path))) {
                        span.dfp {
                            @if let Some(o) = &f.old_path { span.storig { (o) " \u{2192} " } }
                            (f.path)
                        }
                        span.dfs { b.p { "+" (f.additions) } " " b.m { "\u{2212}" (f.deletions) } }
                    }
                }
            }
        }
    };
    let body = html! {
        div.difflayout {
            (files_side)
            div.diffmain data-pane="main" {
                div.sec { div.body {
                    div.cmeta {
                        p.subj { (d.message.lines().next().unwrap_or_default()) }
                        div.full { (d.full_id) }
                        div.by {
                            b { (d.author) } " " (d.email) " \u{b7} " (d.when)
                            " \u{b7} " span.diffstat { (d.files.len()) " files " b.p { "+" (add) } " " b.m { "\u{2212}" (del) } }
                        }
                        div.cactions {
                            a.cbtn href=(at(base, &d.id, "/tree")) { "\u{25a4} Browse files at this commit" }
                        }
                    }
                } }
                // Diffs load on demand: "one" fetches a single file, "list"
                // fetches them all in one request. Either way the commit page
                // itself never renders every hunk up front.
                div id="diffbox" data-loading="loading diff\u{2026}" data-diffs=(href(base, &format!("/commit/{}/diffs", d.id))) {}
            }
        }
    };
    let card = ctx_card(
        "This commit",
        &[
            ("sha", d.id.clone()),
            ("files", d.files.len().to_string()),
            ("author", d.author.clone()),
        ],
    );
    layout(
        repo,
        base,
        "log",
        &perma,
        side,
        &d.id,
        Some(card),
        "HEAD",
        body,
    )
}

/// The rendered diff for one file, returned as a fragment for on-demand loading.
pub fn diff_fragment(diff: Option<&FileDiff>) -> Markup {
    match diff {
        Some(f) => diff_file(0, f),
        None => html! { div.sec { div.body { div.binary { "no changes for this file" } } } },
    }
}

/// Every file's diff for a commit, each wrapped with its index, for the "list"
/// mode's single bulk request.
pub fn commit_diffs_fragment(files: &[FileDiff]) -> Markup {
    html! {
        @for (i, f) in files.iter().enumerate() {
            div.dfwrap data-file=(i) { (diff_file(0, f)) }
        }
    }
}

/// One file's uncommitted diff (staged or unstaged), reached from the summary.
pub fn worktree_diff(
    repo: &str,
    base: &str,
    side: &SideInfo,
    path: &str,
    staged: bool,
    diff: Option<&FileDiff>,
) -> Markup {
    let label = if staged { "staged" } else { "unstaged" };
    let meta = format!("{label} changes");
    let staged_q = if staged { "?staged=1" } else { "" };
    let tabs: Vec<(&str, String, bool)> = vec![
        ("diff", href(base, &format!("/diff/{path}{staged_q}")), true),
        ("code", href(base, &format!("/blob/{path}?src=1")), false),
        ("blame", href(base, &format!("/blame/{path}")), false),
    ];
    let body = html! {
        div.sec { div.body {
            (file_header(path, Some(&meta), &tabs, None))
        } }
        @match diff {
            Some(f) => (diff_file(0, f)),
            None => div.sec { div.body { div.binary { "no " (label) " changes for this file" } } }
        }
    };
    let card = ctx_card(
        "Working change",
        &[("file", path.to_owned()), ("state", label.to_owned())],
    );
    layout(
        repo,
        base,
        "summary",
        "",
        side,
        path,
        Some(card),
        "HEAD",
        body,
    )
}

fn diff_counts(f: &FileDiff) -> (usize, usize) {
    let mut add = 0;
    let mut del = 0;
    for h in &f.hunks {
        for l in &h.lines {
            match l.origin {
                LineOrigin::Added => add += 1,
                LineOrigin::Removed => del += 1,
                _ => {}
            }
        }
    }
    (add, del)
}

fn diff_file(i: usize, f: &FileDiff) -> Markup {
    let (add, del) = diff_counts(f);
    html! {
        div.filediff id=(format!("fd{i}")) data-file=(i) {
            div.fh {
                span.p { (f.path) }
                span.diffstat { b.p { "+" (add) } " " b.m { "\u{2212}" (del) } }
            }
            @if f.binary {
                div.binary { "binary file" }
            } @else {
                // Syntax-highlight each line by the file's syntax (picked by
                // extension, so no per-file content detection). Very large diffs
                // fall back to plain text so the page stays fast; the +/- coloring
                // and background still carry the change.
                @let total: usize = f.hunks.iter().map(|h| h.lines.len()).sum();
                @let syntax = highlight::syntax_for_path(&f.path);
                @let hl = total <= 3000;
                div.hunk { pre {
                    @for h in &f.hunks {
                        span.ln.h { (h.header) "\n" }
                        @for l in &h.lines {
                            @let (cls, mark) = match l.origin {
                                LineOrigin::Added => ("ln a", "+"),
                                LineOrigin::Removed => ("ln d", "-"),
                                LineOrigin::Meta => ("ln h", "\\"),
                                LineOrigin::Context => ("ln", " "),
                            };
                            span class=(cls) {
                                (mark)
                                @if hl { (PreEscaped(highlight::highlight_fragment(syntax, &l.text))) }
                                @else { (l.text) }
                                "\n"
                            }
                        }
                    }
                } }
            }
        }
    }
}

pub fn refs(repo: &str, base: &str, side: &SideInfo, entries: &[RefEntry]) -> Markup {
    let table = |title: &str, kind: RefKind, glyph: &str, tag: bool| -> Markup {
        let rows: Vec<&RefEntry> = entries.iter().filter(|r| r.kind == kind).collect();
        html! {
            @if !rows.is_empty() {
                div.sec {
                    div.h { span.title { (title) } span.count { (rows.len()) } }
                    div.body { table.tbl {
                        @for r in rows {
                            tr data-point {
                                td {
                                    span class=(if tag { "gl t" } else { "gl" }) { (glyph) }
                                    span.nm { a data-go href=(at(base, &r.name, "/tree")) { (r.name) } }
                                    @if r.is_head { " " span.badge { "HEAD" } }
                                }
                            }
                        }
                    } }
                }
            }
        }
    };
    let body = html! {
        (table("Branches", RefKind::Local, "\u{25cf}", false))
        (table("Remotes", RefKind::Remote, "\u{25cf}", false))
        (table("Tags", RefKind::Tag, "\u{2691}", true))
    };
    layout(repo, base, "refs", "", side, "", None, "HEAD", body)
}

/// The releases page: every tag newest-first, each with its message and age, and
/// a link to browse that tag's tree. The newest is marked "latest".
pub fn releases(repo: &str, base: &str, side: &SideInfo, tags: &[TagInfo]) -> Markup {
    let body = html! {
        div.sec {
            div.h { span.title { "Releases" } span.count { (tags.len()) } }
            div.body {
                @if tags.is_empty() {
                    div.binary { "no tags yet; tag a commit to cut a release" }
                }
                @for (i, t) in tags.iter().enumerate() {
                    div class="row rel-row" data-point {
                        span.rel-tag.m {
                            span.gl.t { "\u{2691}" }
                            a data-go href=(at(base, &t.name, "/tree")) { (t.name) }
                            @if i == 0 { span.rel-latest { "latest" } }
                        }
                        span.rel-msg { (t.message) }
                        span.meta { (t.when) }
                    }
                }
            }
        }
    };
    layout(repo, base, "releases", "", side, "", None, "HEAD", body)
}

pub fn blame(repo: &str, base: &str, side: &SideInfo, path: &str, lines: &[BlameLine]) -> Markup {
    let syntax = highlight::syntax_for_path(path);
    let hl = lines.len() <= 5000;
    let body = html! {
        div.sec { div.body {
            (file_header(path, None, &file_tabs(base, path, "HEAD", "blame"), None))
            div {
                @for (i, l) in lines.iter().enumerate() {
                    div.blameline {
                        span.who {
                            @if !l.short_id.is_empty() {
                                a.bh href=(href(base, &format!("/commit/{}", l.short_id))) { (l.short_id) }
                                " "
                                a.ba href=(href(base, &format!("/log?author={}", q_encode(&l.author)))) { (l.author) }
                            }
                        }
                        span.no { (i + 1) }
                        span.bt {
                            @if hl { (PreEscaped(highlight::highlight_fragment(syntax, &l.line))) }
                            @else { (l.line) }
                        }
                    }
                }
            }
        } }
    };
    layout(repo, base, "tree", "", side, path, None, "HEAD", body)
}

/// Group consecutive path-sorted matches into (path, matches) runs.
fn group_by_file(matches: &[GrepMatch]) -> Vec<(&str, Vec<&GrepMatch>)> {
    let mut groups: Vec<(&str, Vec<&GrepMatch>)> = Vec::new();
    for m in matches {
        match groups.last_mut() {
            Some((p, v)) if *p == m.path => v.push(m),
            _ => groups.push((&m.path, vec![m])),
        }
    }
    groups
}

/// One search-result run of files, with each line syntax-highlighted and the
/// match marked. `mark` is the pattern to emphasize; `link` builds the blob href.
fn result_files(
    groups: &[(&str, Vec<&GrepMatch>)],
    mark: &str,
    link: impl Fn(&str) -> String,
) -> Markup {
    html! {
        @for (path, ms) in groups {
            div.srfile data-point {
                a.srpath.m data-go href=(link(path)) { (path) }
                span.srn { (ms.len()) }
            }
            @for m in ms {
                a.srline data-point href=(format!("{}#L{}", link(path), m.line)) {
                    span.srlno.m { (m.line) }
                    span.srtext.m { (highlight_match(path, &m.text, mark)) }
                }
            }
        }
    }
}

/// The text/meaning toggle shown on the search and semantic pages.
fn search_modes(base: &str, query: &str, active: &str) -> Markup {
    let q = q_encode(query);
    html! {
        div.modetabs {
            a class=(if active == "text" { "on" } else { "" }) href=(href(base, &format!("/search?q={q}"))) { "text" }
            a class=(if active == "meaning" { "on" } else { "" }) href=(href(base, &format!("/semantic?q={q}"))) { "meaning" }
        }
    }
}

pub fn search(
    repo: &str,
    base: &str,
    side: &SideInfo,
    query: &str,
    pattern: &str,
    matches: &[GrepMatch],
) -> Markup {
    let groups = group_by_file(matches);
    let body = html! {
        (search_modes(base, query, "text"))
        div.sec { div.body {
            @if query.is_empty() {
                div.binary { "type a query in the search box" }
            } @else if matches.is_empty() {
                div.binary { "no matches for \u{201c}" (query) "\u{201d}" }
            } @else {
                div.srhead { (matches.len()) " matches in " (groups.len()) " files for \u{201c}" span.srq { (query) } "\u{201d}" }
                (result_files(&groups, pattern, |p| href(base, &format!("/blob/{p}"))))
            }
        } }
    };
    layout(repo, base, "search", "", side, query, None, "HEAD", body)
}

/// The cross-repo search page: results grouped by repo, then file.
pub fn global_search(query: &str, pattern: &str, groups: &[(String, Vec<GrepMatch>)]) -> Markup {
    let total: usize = groups.iter().map(|(_, m)| m.len()).sum();
    let content = html! {
        div.sec { div.body {
            @if query.is_empty() {
                div.binary { "search every repository from here" }
            } @else if groups.is_empty() {
                div.binary { "no matches for \u{201c}" (query) "\u{201d}" }
            } @else {
                div.srhead { (total) " matches in " (groups.len()) " repositories for \u{201c}" span.srq { (query) } "\u{201d}" }
                @for (repo, ms) in groups {
                    div.grepo { span.grepon.m { (repo) } span.srn { (ms.len()) } }
                    (result_files(&group_by_file(ms), pattern, |p| format!("/{repo}/blob/{p}")))
                }
            }
        } }
    };
    let sidebar = html! {
        @if !groups.is_empty() {
            div.card {
                div.ch { "By repository" }
                div.cb {
                    @for (repo, ms) in groups {
                        div.langrow { span.langl { (repo) } span.langn { (ms.len()) } }
                    }
                }
            }
        }
        div.card {
            div.ch { "Scope" }
            div.cb {
                p.desc { "Add " span.m style="color:var(--acc-ink)" { "repo:" } " to search one repo, "
                    span.m style="color:var(--acc-ink)" { "lang:" } " / " span.m style="color:var(--acc-ink)" { "path:" }
                    " to narrow, or " span.m style="color:var(--acc-ink)" { "/re/" } " for a regex." }
            }
        }
    };
    plain_layout("rgit / search", content, sidebar)
}

/// One semantic hit: score, path with line range, and a code preview linking to
/// the blob at that line. `repo` prefixes the path for cross-repo results.
fn sem_hit(hit: &rgit_index::SearchHit, repo: Option<&str>) -> Markup {
    let href = match repo {
        Some(r) => format!("/{r}/blob/{}#L{}", hit.path, hit.start_line),
        None => format!("/blob/{}#L{}", hit.path, hit.start_line),
    };
    let shown = match repo {
        Some(r) => format!("{r}/{}", hit.path),
        None => hit.path.clone(),
    };
    let syntax = highlight::syntax_for_path(&hit.path);
    html! {
        a.semhit data-point href=(href) {
            div.semtop {
                span.sempath.m { (shown) }
                span.semloc.m { "L" (hit.start_line) "-" (hit.end_line) }
                span.semscore.m { (format!("{:.2}", hit.score)) }
            }
            pre.semprev { (PreEscaped(highlight::highlight_fragment(syntax, hit.preview.lines().next().unwrap_or("")))) }
        }
    }
}

/// Meaning-based search of one repo's semantic index.
pub fn semantic(
    repo: &str,
    base: &str,
    side: &SideInfo,
    query: &str,
    indexed: bool,
    hits: &[rgit_index::SearchHit],
) -> Markup {
    let body = html! {
        (search_modes(base, query, "meaning"))
        div.sec { div.body {
            @if !indexed {
                div.binary { "no semantic index. run " code.m { "rgit index build" } " to enable meaning-based search." }
            } @else if query.is_empty() {
                div.binary { "search the code by meaning (embeddings), not exact text" }
            } @else if hits.is_empty() {
                div.binary { "no semantic matches for \u{201c}" (query) "\u{201d}" }
            } @else {
                div.srhead { (hits.len()) " results by meaning for \u{201c}" span.srq { (query) } "\u{201d}" }
                @for h in hits { (sem_hit(h, None)) }
            }
        } }
    };
    layout(repo, base, "search", "", side, query, None, "HEAD", body)
}

/// Cross-repo meaning-based search, grouped by repo.
pub fn global_semantic(query: &str, groups: &[(String, Vec<rgit_index::SearchHit>)]) -> Markup {
    let total: usize = groups.iter().map(|(_, h)| h.len()).sum();
    let content = html! {
        div.modetabs {
            a href=(&format!("/search?q={}", q_encode(query))) { "text" }
            a.on href=(&format!("/semantic?q={}", q_encode(query))) { "meaning" }
        }
        div.sec { div.body {
            @if query.is_empty() {
                div.binary { "search every repository by meaning" }
            } @else if groups.is_empty() {
                div.binary { "no semantic matches (or no indexes built) for \u{201c}" (query) "\u{201d}" }
            } @else {
                div.srhead { (total) " results by meaning in " (groups.len()) " repositories for \u{201c}" span.srq { (query) } "\u{201d}" }
                @for (repo, hits) in groups {
                    div.grepo { span.grepon.m { (repo) } span.srn { (hits.len()) } }
                    @for h in hits { (sem_hit(h, Some(repo))) }
                }
            }
        } }
    };
    let sidebar = html! {
        @if !groups.is_empty() {
            div.card {
                div.ch { "By repository" }
                div.cb {
                    @for (repo, hits) in groups {
                        div.langrow { span.langl { (repo) } span.langn { (hits.len()) } }
                    }
                }
            }
        }
        div.card {
            div.ch { "Meaning search" }
            div.cb { p.desc { "Ranks code by embedding similarity, not exact text. Build indexes with "
                span.m style="color:var(--acc-ink)" { "rgit index build --root <dir>" } "." } }
        }
    };
    plain_layout("rgit / semantic", content, sidebar)
}

/// Syntax-highlight one result line and mark the matched span. The syntax comes
/// from the file extension; the match is the first ASCII-case-insensitive
/// occurrence of `query` (best effort - a scoped query may not appear verbatim,
/// in which case the line is highlighted without a mark).
fn highlight_match(path: &str, line: &str, query: &str) -> Markup {
    let syntax = highlight::syntax_for_path(path);
    let html = highlight::highlight_fragment(syntax, line);
    let needle = query.to_ascii_lowercase();
    let range = (!needle.is_empty())
        .then(|| line.to_ascii_lowercase().find(&needle))
        .flatten()
        .map(|b| {
            let start = line[..b].chars().count();
            (start, start + query.chars().count())
        });
    match range {
        Some((s, e)) => PreEscaped(mark_in_html(&html, s, e)),
        None => PreEscaped(html),
    }
}

/// Wrap the visible characters `[start, end)` of already-highlighted HTML in
/// `<mark>`, splitting at tag boundaries so the markup stays well-formed.
fn mark_in_html(html: &str, start: usize, end: usize) -> String {
    if start >= end {
        return html.to_owned();
    }
    let mut out = String::with_capacity(html.len() + 16);
    let mut vis = 0usize;
    let mut in_mark = false;
    let mut it = html.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '<' => {
                if in_mark {
                    out.push_str("</mark>");
                }
                out.push('<');
                for t in it.by_ref() {
                    out.push(t);
                    if t == '>' {
                        break;
                    }
                }
                if in_mark {
                    out.push_str("<mark>");
                }
            }
            '&' => {
                if vis == start {
                    out.push_str("<mark>");
                    in_mark = true;
                }
                out.push('&');
                for t in it.by_ref() {
                    out.push(t);
                    if t == ';' {
                        break;
                    }
                }
                vis += 1;
                if vis == end {
                    out.push_str("</mark>");
                    in_mark = false;
                }
            }
            _ => {
                if vis == start {
                    out.push_str("<mark>");
                    in_mark = true;
                }
                out.push(c);
                vis += 1;
                if vis == end {
                    out.push_str("</mark>");
                    in_mark = false;
                }
            }
        }
    }
    if in_mark {
        out.push_str("</mark>");
    }
    out
}

fn crumb(base: &str, rev: &str, path: &str) -> Markup {
    let mut acc = String::new();
    let parts: Vec<(String, String)> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|seg| {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(seg);
            (seg.to_owned(), acc.clone())
        })
        .collect();
    html! {
        div.crumb {
            a href=(at(base, rev, "/tree")) { "root" }
            @for (seg, full) in &parts {
                span.s { "/" }
                a href=(at(base, rev, &format!("/tree/{full}"))) { (seg) }
            }
        }
    }
}

fn human_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
