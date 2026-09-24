//! rgit serve: a server-rendered, keyboard-driven web viewer over the same
//! GitBackend the CLI and MCP use. Every view is a real URL; a thin JS layer
//! adds motion on top of complete HTML. Progressive enhancement, no SPA.
//!
//! Two modes: a single repo (the current directory), served at `/...`; or a
//! managed root of repositories addressed by name, served at `/{repo}/...` with
//! an index at `/`. Names are single path segments, so a client can never reach
//! outside the root.

mod assets;
mod highlight;
mod preview;
mod view;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use maud::Markup;
use rgit_git::{Git2Backend, GitBackend, GitError, GrepQuery, LogOptions, RefKind};

const LOG_PAGE: usize = 50;

/// The server's public clone base (e.g. `https://git.example.dev`), set once at
/// startup. When present, the shown clone URL is `{base}/{repo}.git` - the way a
/// client clones from THIS server - rather than the repo's own remotes.
static CLONE_BASE: OnceLock<Option<String>> = OnceLock::new();

fn clone_base() -> Option<&'static str> {
    CLONE_BASE.get().and_then(|o| o.as_deref())
}

/// One MCP tool for the human-readable `/agent` reference page. Populated by the
/// CLI (which owns the tool catalog) via [`set_mcp_tools`] before serving.
#[derive(Debug, Clone)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    /// True for a mutating tool: available over stdio, disabled on the read-only
    /// HTTP endpoint.
    pub writes: bool,
    pub args: Vec<McpArg>,
}

/// One argument of an [`McpTool`].
#[derive(Debug, Clone)]
pub struct McpArg {
    pub name: String,
    pub ty: String,
    pub required: bool,
}

static MCP_TOOLS: OnceLock<Vec<McpTool>> = OnceLock::new();

/// Register the MCP tool catalog rendered on `/agent`. Call once before serving.
pub fn set_mcp_tools(tools: Vec<McpTool>) {
    let _ = MCP_TOOLS.set(tools);
}

fn mcp_tools() -> &'static [McpTool] {
    MCP_TOOLS.get().map(Vec::as_slice).unwrap_or(&[])
}

async fn agent_page() -> Markup {
    view::agent_tools(mcp_tools())
}

/// Repo metadata for the sidebar: branch/tag counts, a clone URL, and a
/// description from `.git/description` if the repo set one.
fn side_info(b: &dyn GitBackend, base: &str, repo: &str) -> view::SideInfo {
    let refs = b.refs().unwrap_or_default();
    let branch_names: Vec<String> = refs
        .iter()
        .filter(|r| r.kind == RefKind::Local)
        .map(|r| r.name.clone())
        .collect();
    let tag_names: Vec<String> = refs
        .iter()
        .filter(|r| r.kind == RefKind::Tag)
        .map(|r| r.name.clone())
        .collect();
    let branches = branch_names.len();
    let tags = tag_names.len();
    // On a hosting server (clone-base set), clone from the server; otherwise show
    // the repo's own remotes (the local-dev case).
    let (clone_url, remotes) = match clone_base() {
        Some(cb) => {
            let url = format!("{}/{repo}.git", cb.trim_end_matches('/'));
            (Some(url.clone()), vec![("origin".to_owned(), url)])
        }
        None => {
            let remotes: Vec<(String, String)> = b
                .remotes()
                .unwrap_or_default()
                .into_iter()
                .map(|r| (r.name, r.url))
                .collect();
            let clone_url = remotes.first().map(|(_, u)| u.clone());
            (clone_url, remotes)
        }
    };
    let description = std::fs::read_to_string(b.workdir().join(".git/description"))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty() && !s.starts_with("Unnamed repository"));
    view::SideInfo {
        description,
        branches,
        tags,
        clone_url,
        archive: format!("{base}/archive"),
        remotes,
        // languages/contributors/release are summary-only: each needs a tree or
        // history walk, too heavy to run on every page's sidebar.
        languages: Vec::new(),
        contributors: Vec::new(),
        release: None,
        branch_names,
        tag_names,
    }
}

/// A rough language breakdown by file count, top 4 plus Other, as
/// (label, percent, css-class l0..l4).
fn languages(files: &[String]) -> Vec<(String, u32, String)> {
    fn label(path: &str) -> &'static str {
        let ext = path.rsplit('.').next().unwrap_or("");
        match ext {
            "rs" => "Rust",
            "py" => "Python",
            "js" | "mjs" | "cjs" => "JavaScript",
            "ts" | "tsx" => "TypeScript",
            "go" => "Go",
            "c" => "C",
            "h" | "hpp" | "hh" => "Headers",
            "cpp" | "cc" | "cxx" => "C++",
            "java" => "Java",
            "rb" => "Ruby",
            "sh" | "bash" => "Shell",
            "md" | "markdown" => "Markdown",
            "html" | "htm" => "HTML",
            "css" | "scss" => "CSS",
            "toml" => "TOML",
            "yaml" | "yml" => "YAML",
            "json" => "JSON",
            "cmake" | "txt" if path.ends_with("CMakeLists.txt") => "CMake",
            _ => "Other",
        }
    }
    if files.is_empty() {
        return Vec::new();
    }
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for f in files {
        *counts.entry(label(f)).or_default() += 1;
    }
    let total = files.len() as f64;
    let mut sorted: Vec<(&str, usize)> = counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    sorted
        .into_iter()
        .take(5)
        .enumerate()
        .map(|(i, (name, n))| {
            let pct = ((n as f64 / total) * 100.0).round() as u32;
            (name.to_owned(), pct, format!("l{i}"))
        })
        .collect()
}

/// A backend error becomes a 404 with its message (a missing rev/path is the
/// common case); a bad repo name is a 400.
struct AppError(StatusCode, String);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.0, self.1).into_response()
    }
}

impl From<GitError> for AppError {
    fn from(e: GitError) -> Self {
        AppError(StatusCode::NOT_FOUND, e.to_string())
    }
}

/// Resolves a repo name to a backend within a managed root, caching handles.
struct Registry {
    root: PathBuf,
    cache: Mutex<HashMap<String, Arc<dyn GitBackend>>>,
    /// Cached index cards (metadata is a walk per repo), refreshed on a TTL.
    cards: Mutex<HashMap<String, (Instant, view::RepoCard)>>,
}

impl Registry {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            cache: Mutex::new(HashMap::new()),
            cards: Mutex::new(HashMap::new()),
        }
    }

    /// An index card for one repo, from cache when fresh (the metadata needs a
    /// tree walk, too heavy to recompute on every index load).
    fn card(&self, name: &str) -> view::RepoCard {
        const TTL: Duration = Duration::from_secs(60);
        if let Some((at, card)) = self.cards.lock().expect("cards").get(name) {
            if at.elapsed() < TTL {
                return card.clone();
            }
        }
        let card = match self.resolve(name) {
            Ok(b) => repo_card(b.as_ref(), name),
            Err(_) => view::RepoCard {
                name: name.to_owned(),
                description: None,
                last: None,
                branches: 0,
                tags: 0,
                language: None,
            },
        };
        self.cards
            .lock()
            .expect("cards")
            .insert(name.to_owned(), (Instant::now(), card.clone()));
        card
    }

    /// A repo name must be a single, non-hidden path segment: no separators, no
    /// `.`/`..`, so it can only ever name a directory directly under the root.
    fn valid(name: &str) -> bool {
        !name.is_empty() && !name.starts_with('.') && !name.contains('/') && !name.contains('\\')
    }

    fn resolve(&self, name: &str) -> Result<Arc<dyn GitBackend>, AppError> {
        if !Self::valid(name) {
            return Err(AppError(
                StatusCode::BAD_REQUEST,
                format!("invalid repo name: {name}"),
            ));
        }
        if let Some(b) = self.cache.lock().expect("registry").get(name) {
            return Ok(b.clone());
        }
        let backend = Git2Backend::discover(self.root.join(name))
            .map_err(|e| AppError(StatusCode::NOT_FOUND, format!("no repo '{name}' ({e})")))?;
        let arc: Arc<dyn GitBackend> = Arc::new(backend);
        self.cache
            .lock()
            .expect("registry")
            .insert(name.to_owned(), arc.clone());
        Ok(arc)
    }

    /// Names of the git repositories directly under the root, sorted.
    fn list(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.root) {
            for e in entries.flatten() {
                if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let name = e.file_name().to_string_lossy().into_owned();
                if Self::valid(&name) && Git2Backend::discover(e.path()).is_ok() {
                    out.push(name);
                }
            }
        }
        out.sort();
        out
    }
}

enum Repos {
    Single {
        backend: Arc<dyn GitBackend>,
        name: String,
    },
    Multi(Registry),
}

struct AppState {
    repos: Repos,
}

type Shared = Arc<AppState>;

/// Resolve a request to (backend, repo display name, URL base). In single mode
/// the base is empty; in multi mode it is `/{repo}`.
fn ctx(
    state: &Shared,
    repo: Option<&str>,
) -> Result<(Arc<dyn GitBackend>, String, String), AppError> {
    match &state.repos {
        Repos::Single { backend, name } => Ok((backend.clone(), name.clone(), String::new())),
        Repos::Multi(reg) => {
            let name =
                repo.ok_or_else(|| AppError(StatusCode::NOT_FOUND, "repo required".into()))?;
            let backend = reg.resolve(name)?;
            Ok((backend, name.to_owned(), format!("/{name}")))
        }
    }
}

// --- page builders, shared by both route shapes ---

fn summary_page(b: &dyn GitBackend, repo: &str, base: &str) -> Result<Markup, AppError> {
    let lanes = if b.lanes_active() {
        b.lanes_state().ok()
    } else {
        None
    };
    // The contributors/release cards are summary-only: they need a history walk
    // and a tag scan, too heavy to run on every page's sidebar.
    let mut side = side_info(b, base, repo);
    side.languages = languages(&b.list_files("HEAD").unwrap_or_default());
    side.contributors = b.contributors().unwrap_or_default();
    side.release = b
        .latest_tag()
        .ok()
        .flatten()
        .map(|t| (t.name, t.when, t.message));
    Ok(view::summary(
        repo,
        base,
        &side,
        &b.status()?,
        lanes.as_ref(),
    ))
}

fn commit_page(b: &dyn GitBackend, repo: &str, base: &str, rev: &str) -> Result<Markup, AppError> {
    Ok(view::commit(
        repo,
        base,
        &side_info(b, base, repo),
        &b.commit_overview(rev)?,
    ))
}

fn commit_diff_fragment(b: &dyn GitBackend, rev: &str, path: &str) -> Result<Markup, AppError> {
    Ok(view::diff_fragment(b.commit_file_diff(rev, path)?.as_ref()))
}

fn commit_diffs_fragment(b: &dyn GitBackend, rev: &str) -> Result<Markup, AppError> {
    Ok(view::commit_diffs_fragment(&b.commit_details(rev)?.files))
}

fn refs_page(b: &dyn GitBackend, repo: &str, base: &str) -> Result<Markup, AppError> {
    Ok(view::refs(
        repo,
        base,
        &side_info(b, base, repo),
        &b.refs()?,
    ))
}

fn releases_page(b: &dyn GitBackend, repo: &str, base: &str) -> Result<Markup, AppError> {
    Ok(view::releases(
        repo,
        base,
        &side_info(b, base, repo),
        &b.all_tags()?,
    ))
}

fn blame_page(b: &dyn GitBackend, repo: &str, base: &str, path: &str) -> Result<Markup, AppError> {
    Ok(view::blame(
        repo,
        base,
        &side_info(b, base, repo),
        path,
        &b.blame(path)?,
    ))
}

fn diff_page(
    b: &dyn GitBackend,
    repo: &str,
    base: &str,
    path: &str,
    q: &HashMap<String, String>,
) -> Result<Markup, AppError> {
    let staged = q.get("staged").map(|v| v == "1").unwrap_or(false);
    let diff = b.file_diff(path, staged)?;
    Ok(view::worktree_diff(
        repo,
        base,
        &side_info(b, base, repo),
        path,
        staged,
        diff.as_ref(),
    ))
}

fn log_page(
    b: &dyn GitBackend,
    repo: &str,
    base: &str,
    q: &HashMap<String, String>,
) -> Result<Response, AppError> {
    let offset = q.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
    let author = q.get("author").filter(|a| !a.is_empty()).cloned();
    let rev = q.get("ref").map(String::as_str).unwrap_or("HEAD");
    let entries = b.log(&LogOptions {
        limit: LOG_PAGE,
        offset,
        author: author.clone(),
        rev: (rev != "HEAD").then(|| rev.to_owned()),
        ..LogOptions::default()
    })?;
    if q.contains_key("partial") {
        return Ok(view::log_rows(base, &entries).into_response());
    }
    Ok(view::log(
        repo,
        base,
        &side_info(b, base, repo),
        &entries,
        offset,
        LOG_PAGE,
        author.as_deref(),
        rev,
    )
    .into_response())
}

fn tree_page(
    b: &dyn GitBackend,
    repo: &str,
    base: &str,
    rev: &str,
    path: &str,
) -> Result<Markup, AppError> {
    let entries = b.list_tree(rev, path)?;
    let paths: Vec<String> = entries.iter().map(|e| e.path.clone()).collect();
    let last = b.tree_last_commits(rev, &paths).unwrap_or_default();
    let files = b.list_files(rev).unwrap_or_default();
    Ok(view::tree(
        repo,
        base,
        &side_info(b, base, repo),
        path,
        &entries,
        &last,
        &files,
        rev,
    ))
}

fn blob_page(
    b: &dyn GitBackend,
    repo: &str,
    base: &str,
    path: &str,
    q: &HashMap<String, String>,
) -> Result<Response, AppError> {
    let rev = q.get("ref").map(String::as_str).unwrap_or("HEAD");
    let blob = b.read_blob(rev, path)?;
    if q.contains_key("raw") {
        return Ok((
            [("content-type", "text/plain; charset=utf-8")],
            blob.text.unwrap_or_default(),
        )
            .into_response());
    }
    // Pin the permalink to the resolved commit so it never drifts off the branch.
    let sha = b.rev_parse(rev).unwrap_or_else(|_| rev.to_owned());
    let perma = format!("{base}/blob/{path}?ref={sha}");
    let files = b.list_files(rev).unwrap_or_default();
    // Prose formats (Markdown, RST) render by default, with ?src=1 for source.
    let previewable = preview::is_renderable(path);
    let rendered = if previewable && !q.contains_key("src") {
        blob.text.as_deref().and_then(|t| preview::render(path, t))
    } else {
        None
    };
    Ok(view::blob(
        repo,
        base,
        &side_info(b, base, repo),
        &blob,
        rev,
        &perma,
        &files,
        rendered.as_deref(),
        previewable,
    )
    .into_response())
}

/// PR + CI badges for a branch, fetched lazily via `gh` (the forge escape
/// hatch). Empty when gh is absent/unauthenticated or there is no PR - the
/// section simply shows nothing rather than erroring.
fn forge_badges(workdir: &std::path::Path, branch: &str) -> String {
    let output = std::process::Command::new("gh")
        .current_dir(workdir)
        .args([
            "pr",
            "list",
            "--head",
            branch,
            "--json",
            "number,url,statusCheckRollup",
            "--limit",
            "5",
        ])
        .output();
    let Ok(output) = output else {
        return String::new();
    };
    if !output.status.success() {
        return String::new();
    }
    let Ok(prs) = serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout) else {
        return String::new();
    };
    let mut html = String::new();
    for pr in &prs {
        let num = pr["number"].as_u64().unwrap_or(0);
        let url = pr["url"].as_str().unwrap_or("#");
        let ci = pr["statusCheckRollup"]
            .as_array()
            .map(|a| ci_mark(a))
            .unwrap_or("");
        html.push_str(&format!(
            "<a class=\"prbadge\" href=\"{url}\">PR #{num}{ci}</a>"
        ));
    }
    html
}

/// Aggregate a PR's check-run states into one CI mark.
fn ci_mark(rollup: &[serde_json::Value]) -> &'static str {
    let (mut any, mut fail, mut pending) = (false, false, false);
    for c in rollup {
        any = true;
        let state = c["state"]
            .as_str()
            .or_else(|| c["conclusion"].as_str())
            .unwrap_or("");
        match state {
            "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED" => fail = true,
            "SUCCESS" | "NEUTRAL" | "SKIPPED" => {}
            _ => pending = true,
        }
    }
    if !any {
        ""
    } else if fail {
        " <span class=\"ci f\">\u{2717}</span>"
    } else if pending {
        " <span class=\"ci r\">\u{25cf}</span>"
    } else {
        " <span class=\"ci p\">\u{2713}</span>"
    }
}

fn search_page(
    b: &dyn GitBackend,
    repo: &str,
    base: &str,
    q: &HashMap<String, String>,
) -> Result<Markup, AppError> {
    let query = q.get("q").map(String::as_str).unwrap_or("").trim();
    let gq = parse_query(query).0;
    let matches = if query.is_empty() {
        Vec::new()
    } else {
        b.grep_query(&gq).unwrap_or_default()
    };
    Ok(view::search(
        repo,
        base,
        &side_info(b, base, repo),
        query,
        &gq.pattern,
        &matches,
    ))
}

/// Cross-repo code search over every repo in the managed root (or the one named
/// by a `repo:` scope), grouped by repo. Only available in multi-repo mode.
fn global_search_page(reg: &Registry, q: &HashMap<String, String>) -> Result<Markup, AppError> {
    const MAX_TOTAL: usize = 400;
    let query = q.get("q").map(String::as_str).unwrap_or("").trim();
    let (gq, scope) = parse_query(query);
    let mut groups: Vec<(String, Vec<rgit_git::GrepMatch>)> = Vec::new();
    if !query.is_empty() {
        let names: Vec<String> = match &scope {
            Some(name) => reg.list().into_iter().filter(|n| n == name).collect(),
            None => reg.list(),
        };
        let mut total = 0usize;
        for name in names {
            if total >= MAX_TOTAL {
                break;
            }
            if let Ok(b) = reg.resolve(&name) {
                if let Ok(mut ms) = b.grep_query(&gq) {
                    if ms.is_empty() {
                        continue;
                    }
                    ms.truncate(MAX_TOTAL - total);
                    total += ms.len();
                    groups.push((name, ms));
                }
            }
        }
    }
    Ok(view::global_search(query, &gq.pattern, &groups))
}

/// A process-wide embedding model, loaded once (the model download/load is too
/// slow to repeat per request). The load error is cached too, so a missing model
/// does not retry every request.
fn embedder() -> Result<&'static rgit_index::Embedder, AppError> {
    static CELL: std::sync::OnceLock<Result<rgit_index::Embedder, String>> =
        std::sync::OnceLock::new();
    match CELL.get_or_init(|| rgit_index::Embedder::new().map_err(|e| e.to_string())) {
        Ok(e) => Ok(e),
        Err(e) => Err(AppError(StatusCode::INTERNAL_SERVER_ERROR, e.clone())),
    }
}

const SEMANTIC_LIMIT: usize = 40;

/// How strongly git history reorders semantic results, and how far back the
/// churn/recency walk looks. Mirrors the CLI so both surfaces rank the same way.
const HISTORY_ALPHA: f32 = 0.5;
const HISTORY_WINDOW: usize = 500;

/// Per-path churn/recency weights for a repo, empty when history is unavailable.
fn history_boost(b: &dyn GitBackend) -> HashMap<String, f32> {
    b.file_activity(HISTORY_WINDOW)
        .map(|a| rgit_git::activity_weights(&a))
        .unwrap_or_default()
}

/// Meaning-based search of one repo's semantic index.
fn semantic_page(
    b: &dyn GitBackend,
    repo: &str,
    base: &str,
    q: &HashMap<String, String>,
) -> Result<Markup, AppError> {
    let query = q.get("q").map(String::as_str).unwrap_or("").trim();
    let index = rgit_index::load(&rgit_index::index_path(b.workdir()));
    let indexed = index.is_some();
    let hits = match (&index, query.is_empty()) {
        (Some(idx), false) => {
            let boost = history_boost(b);
            rgit_index::search_boosted(
                idx,
                embedder()?,
                query,
                SEMANTIC_LIMIT,
                &boost,
                HISTORY_ALPHA,
            )
            .map_err(|e| AppError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        }
        _ => Vec::new(),
    };
    Ok(view::semantic(
        repo,
        base,
        &side_info(b, base, repo),
        query,
        indexed,
        &hits,
    ))
}

/// Cross-repo meaning-based search over every repo's semantic index, grouped by
/// repo. Multi-repo mode only.
fn global_semantic_page(reg: &Registry, q: &HashMap<String, String>) -> Result<Markup, AppError> {
    let query = q.get("q").map(String::as_str).unwrap_or("").trim();
    let mut groups: Vec<(String, Vec<rgit_index::SearchHit>)> = Vec::new();
    if !query.is_empty() {
        let embedder = embedder()?;
        for name in reg.list() {
            if let Ok(b) = reg.resolve(&name) {
                if let Some(index) = rgit_index::load(&rgit_index::index_path(b.workdir())) {
                    let boost = history_boost(b.as_ref());
                    if let Ok(hits) = rgit_index::search_boosted(
                        &index,
                        embedder,
                        query,
                        SEMANTIC_LIMIT,
                        &boost,
                        HISTORY_ALPHA,
                    ) {
                        if !hits.is_empty() {
                            groups.push((name, hits));
                        }
                    }
                }
            }
        }
        // Keep the best across repos by cosine score.
        for (_, hits) in &mut groups {
            hits.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        groups.sort_by(|a, b| {
            let (sa, sb) = (
                a.1.first().map(|h| h.score).unwrap_or(0.0),
                b.1.first().map(|h| h.score).unwrap_or(0.0),
            );
            sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    Ok(view::global_semantic(query, &groups))
}

/// Parse a GitHub-style scoped query into a [`GrepQuery`] and an optional
/// `repo:` scope. Qualifiers: `lang:` / `ext:` narrow by extension, `path:` by a
/// path substring, a `/.../`-wrapped term is a regex, a `"..."`-wrapped term is
/// an exact phrase. The `repo:` scope is only meaningful for the cross-repo
/// search; a per-repo page ignores it. Everything else is the literal pattern.
fn parse_query(raw: &str) -> (GrepQuery, Option<String>) {
    let mut regex = false;
    let mut path = None;
    let mut repo = None;
    let mut exts: Vec<String> = Vec::new();
    let mut pattern = String::new();
    let mut terms: Vec<String> = Vec::new();
    for tok in tokenize(raw) {
        if let Some(v) = tok.strip_prefix("lang:") {
            exts.extend(lang_exts(v));
        } else if let Some(v) = tok.strip_prefix("ext:") {
            exts.push(v.trim_start_matches('.').to_ascii_lowercase());
        } else if let Some(v) = tok.strip_prefix("path:") {
            if !v.is_empty() {
                path = Some(v.to_owned());
            }
        } else if let Some(v) = tok.strip_prefix("repo:") {
            if !v.is_empty() {
                repo = Some(v.to_owned());
            }
        } else if tok.len() >= 2 && tok.starts_with('/') && tok.ends_with('/') {
            regex = true;
            pattern = tok[1..tok.len() - 1].to_owned();
        } else if tok.len() >= 2 && tok.starts_with('"') && tok.ends_with('"') {
            terms.push(tok[1..tok.len() - 1].to_owned());
        } else {
            terms.push(tok);
        }
    }
    if pattern.is_empty() {
        pattern = terms.join(" ");
    }
    (
        GrepQuery {
            pattern,
            regex,
            path,
            exts,
        },
        repo,
    )
}

/// Split a query into tokens, keeping `"..."` and `/.../` groups intact.
fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut delim: Option<char> = None;
    for c in s.chars() {
        match delim {
            Some(d) => {
                cur.push(c);
                if c == d {
                    out.push(std::mem::take(&mut cur));
                    delim = None;
                }
            }
            None => {
                if c.is_whitespace() {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                } else if (c == '"' || c == '/') && cur.is_empty() {
                    cur.push(c);
                    delim = Some(c);
                } else {
                    cur.push(c);
                }
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Resolve a `lang:` value to candidate file extensions using the bundled syntax
/// set (the same Linguist-derived data used for highlighting). Falls back to
/// treating the value itself as an extension when the language is unknown.
fn lang_exts(lang: &str) -> Vec<String> {
    let exts: Vec<String> = highlight::lang_extensions(lang)
        .into_iter()
        .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
        .collect();
    if exts.is_empty() {
        vec![lang.trim_start_matches('.').to_ascii_lowercase()]
    } else {
        exts
    }
}

fn files_body(b: &dyn GitBackend) -> Result<Response, AppError> {
    Ok((
        [("content-type", "text/plain; charset=utf-8")],
        b.list_files("HEAD")?.join("\n"),
    )
        .into_response())
}

fn prs_body(b: &dyn GitBackend) -> Result<Response, AppError> {
    let branch = b
        .status()
        .ok()
        .and_then(|s| s.head.branch)
        .unwrap_or_default();
    let html = if branch.is_empty() {
        String::new()
    } else {
        forge_badges(b.workdir(), &branch)
    };
    Ok(([("content-type", "text/html; charset=utf-8")], html).into_response())
}

fn archive_body(
    b: &dyn GitBackend,
    repo: &str,
    q: &HashMap<String, String>,
) -> Result<Response, AppError> {
    let rev = q.get("rev").map(String::as_str).unwrap_or("HEAD");
    let bytes = b.archive_targz(rev)?;
    let filename = format!("{repo}-{rev}.tar.gz");
    Ok((
        [
            ("content-type", "application/gzip".to_owned()),
            (
                "content-disposition",
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        bytes,
    )
        .into_response())
}

// --- single-repo handlers (no repo segment) ---

async fn s_summary(State(s): State<Shared>) -> Result<Markup, AppError> {
    let (b, r, base) = ctx(&s, None)?;
    summary_page(b.as_ref(), &r, &base)
}
async fn s_log(
    State(s): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let (b, r, base) = ctx(&s, None)?;
    log_page(b.as_ref(), &r, &base, &q)
}
async fn s_tree_root(
    State(s): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Markup, AppError> {
    let (b, r, base) = ctx(&s, None)?;
    let rev = q.get("ref").map(String::as_str).unwrap_or("HEAD");
    tree_page(b.as_ref(), &r, &base, rev, "")
}
async fn s_tree(
    State(s): State<Shared>,
    Path(path): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Markup, AppError> {
    let (b, r, base) = ctx(&s, None)?;
    let rev = q.get("ref").map(String::as_str).unwrap_or("HEAD");
    tree_page(b.as_ref(), &r, &base, rev, &path)
}
async fn s_blob(
    State(s): State<Shared>,
    Path(path): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let (b, r, base) = ctx(&s, None)?;
    blob_page(b.as_ref(), &r, &base, &path, &q)
}
async fn s_commit(State(s): State<Shared>, Path(rev): Path<String>) -> Result<Markup, AppError> {
    let (b, r, base) = ctx(&s, None)?;
    commit_page(b.as_ref(), &r, &base, &rev)
}
async fn s_commit_diff(
    State(s): State<Shared>,
    Path((rev, path)): Path<(String, String)>,
) -> Result<Markup, AppError> {
    let (b, _, _) = ctx(&s, None)?;
    commit_diff_fragment(b.as_ref(), &rev, &path)
}
async fn s_commit_diffs(
    State(s): State<Shared>,
    Path(rev): Path<String>,
) -> Result<Markup, AppError> {
    let (b, _, _) = ctx(&s, None)?;
    commit_diffs_fragment(b.as_ref(), &rev)
}
async fn s_refs(State(s): State<Shared>) -> Result<Markup, AppError> {
    let (b, r, base) = ctx(&s, None)?;
    refs_page(b.as_ref(), &r, &base)
}

async fn s_releases(State(s): State<Shared>) -> Result<Markup, AppError> {
    let (b, r, base) = ctx(&s, None)?;
    releases_page(b.as_ref(), &r, &base)
}

async fn m_releases(State(s): State<Shared>, Path(repo): Path<String>) -> Result<Markup, AppError> {
    let (b, r, base) = ctx(&s, Some(&repo))?;
    releases_page(b.as_ref(), &r, &base)
}
async fn s_blame(State(s): State<Shared>, Path(path): Path<String>) -> Result<Markup, AppError> {
    let (b, r, base) = ctx(&s, None)?;
    blame_page(b.as_ref(), &r, &base, &path)
}
async fn s_diff(
    State(s): State<Shared>,
    Path(path): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Markup, AppError> {
    let (b, r, base) = ctx(&s, None)?;
    diff_page(b.as_ref(), &r, &base, &path, &q)
}
async fn s_files(State(s): State<Shared>) -> Result<Response, AppError> {
    let (b, _, _) = ctx(&s, None)?;
    files_body(b.as_ref())
}
async fn s_archive(
    State(s): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let (b, r, _) = ctx(&s, None)?;
    archive_body(b.as_ref(), &r, &q)
}
async fn s_prs(State(s): State<Shared>) -> Result<Response, AppError> {
    let (b, _, _) = ctx(&s, None)?;
    prs_body(b.as_ref())
}
async fn s_search(
    State(s): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Markup, AppError> {
    let (b, r, base) = ctx(&s, None)?;
    search_page(b.as_ref(), &r, &base, &q)
}
async fn s_semantic(
    State(s): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Markup, AppError> {
    let (b, r, base) = ctx(&s, None)?;
    semantic_page(b.as_ref(), &r, &base, &q)
}

// --- multi-repo handlers (repo segment) ---

/// Light metadata for one repo on the index: description, latest commit,
/// branch/tag counts, and dominant language.
fn repo_card(b: &dyn GitBackend, name: &str) -> view::RepoCard {
    let description = std::fs::read_to_string(b.workdir().join(".git/description"))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty() && !s.starts_with("Unnamed repository"));
    let last = b
        .log(&LogOptions {
            limit: 1,
            ..LogOptions::default()
        })
        .ok()
        .and_then(|v| v.into_iter().next())
        .map(|e| (e.short_id, e.summary, e.when));
    let refs = b.refs().unwrap_or_default();
    let branches = refs.iter().filter(|r| r.kind == RefKind::Local).count();
    let tags = refs.iter().filter(|r| r.kind == RefKind::Tag).count();
    let language = languages(&b.list_files("HEAD").unwrap_or_default())
        .into_iter()
        .find(|(l, _, _)| l != "Other")
        .map(|(l, _, _)| l);
    view::RepoCard {
        name: name.to_owned(),
        description,
        last,
        branches,
        tags,
        language,
    }
}

async fn m_index(State(s): State<Shared>) -> Result<Markup, AppError> {
    match &s.repos {
        Repos::Multi(reg) => {
            let cards: Vec<view::RepoCard> =
                reg.list().into_iter().map(|name| reg.card(&name)).collect();
            Ok(view::index(&cards))
        }
        Repos::Single { backend, name } => Ok(view::index(&[repo_card(backend.as_ref(), name)])),
    }
}
async fn m_summary(State(s): State<Shared>, Path(repo): Path<String>) -> Result<Markup, AppError> {
    let (b, r, base) = ctx(&s, Some(&repo))?;
    summary_page(b.as_ref(), &r, &base)
}
async fn m_log(
    State(s): State<Shared>,
    Path(repo): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let (b, r, base) = ctx(&s, Some(&repo))?;
    log_page(b.as_ref(), &r, &base, &q)
}
async fn m_tree_root(
    State(s): State<Shared>,
    Path(repo): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Markup, AppError> {
    let (b, r, base) = ctx(&s, Some(&repo))?;
    let rev = q.get("ref").map(String::as_str).unwrap_or("HEAD");
    tree_page(b.as_ref(), &r, &base, rev, "")
}
async fn m_tree(
    State(s): State<Shared>,
    Path((repo, path)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Markup, AppError> {
    let (b, r, base) = ctx(&s, Some(&repo))?;
    let rev = q.get("ref").map(String::as_str).unwrap_or("HEAD");
    tree_page(b.as_ref(), &r, &base, rev, &path)
}
async fn m_blob(
    State(s): State<Shared>,
    Path((repo, path)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let (b, r, base) = ctx(&s, Some(&repo))?;
    blob_page(b.as_ref(), &r, &base, &path, &q)
}
async fn m_commit(
    State(s): State<Shared>,
    Path((repo, rev)): Path<(String, String)>,
) -> Result<Markup, AppError> {
    let (b, r, base) = ctx(&s, Some(&repo))?;
    commit_page(b.as_ref(), &r, &base, &rev)
}
async fn m_commit_diff(
    State(s): State<Shared>,
    Path((repo, rev, path)): Path<(String, String, String)>,
) -> Result<Markup, AppError> {
    let (b, _, _) = ctx(&s, Some(&repo))?;
    commit_diff_fragment(b.as_ref(), &rev, &path)
}
async fn m_commit_diffs(
    State(s): State<Shared>,
    Path((repo, rev)): Path<(String, String)>,
) -> Result<Markup, AppError> {
    let (b, _, _) = ctx(&s, Some(&repo))?;
    commit_diffs_fragment(b.as_ref(), &rev)
}
async fn m_refs(State(s): State<Shared>, Path(repo): Path<String>) -> Result<Markup, AppError> {
    let (b, r, base) = ctx(&s, Some(&repo))?;
    refs_page(b.as_ref(), &r, &base)
}
async fn m_blame(
    State(s): State<Shared>,
    Path((repo, path)): Path<(String, String)>,
) -> Result<Markup, AppError> {
    let (b, r, base) = ctx(&s, Some(&repo))?;
    blame_page(b.as_ref(), &r, &base, &path)
}
async fn m_diff(
    State(s): State<Shared>,
    Path((repo, path)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Markup, AppError> {
    let (b, r, base) = ctx(&s, Some(&repo))?;
    diff_page(b.as_ref(), &r, &base, &path, &q)
}
async fn m_files(State(s): State<Shared>, Path(repo): Path<String>) -> Result<Response, AppError> {
    let (b, _, _) = ctx(&s, Some(&repo))?;
    files_body(b.as_ref())
}
async fn m_archive(
    State(s): State<Shared>,
    Path(repo): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let (b, r, _) = ctx(&s, Some(&repo))?;
    archive_body(b.as_ref(), &r, &q)
}
async fn m_prs(State(s): State<Shared>, Path(repo): Path<String>) -> Result<Response, AppError> {
    let (b, _, _) = ctx(&s, Some(&repo))?;
    prs_body(b.as_ref())
}
async fn m_search(
    State(s): State<Shared>,
    Path(repo): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Markup, AppError> {
    let (b, r, base) = ctx(&s, Some(&repo))?;
    search_page(b.as_ref(), &r, &base, &q)
}
async fn m_semantic(
    State(s): State<Shared>,
    Path(repo): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Markup, AppError> {
    let (b, r, base) = ctx(&s, Some(&repo))?;
    semantic_page(b.as_ref(), &r, &base, &q)
}
async fn g_search(
    State(s): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Markup, AppError> {
    match &s.repos {
        Repos::Multi(reg) => global_search_page(reg, &q),
        Repos::Single { .. } => Err(AppError(StatusCode::NOT_FOUND, "not found".into())),
    }
}
async fn g_semantic(
    State(s): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Markup, AppError> {
    match &s.repos {
        Repos::Multi(reg) => global_semantic_page(reg, &q),
        Repos::Single { .. } => Err(AppError(StatusCode::NOT_FOUND, "not found".into())),
    }
}
async fn g_repos(State(s): State<Shared>) -> Response {
    let names = match &s.repos {
        Repos::Multi(reg) => reg.list(),
        Repos::Single { name, .. } => vec![name.clone()],
    };
    let body = serde_json::to_string(&names).unwrap_or_else(|_| "[]".into());
    ([("content-type", "application/json")], body).into_response()
}

fn single_router(state: Shared) -> Router {
    Router::new()
        .route("/", get(s_summary))
        .route("/log", get(s_log))
        .route("/tree", get(s_tree_root))
        .route("/tree/{*path}", get(s_tree))
        .route("/blob/{*path}", get(s_blob))
        .route("/commit/{rev}", get(s_commit))
        .route("/commit/{rev}/diffs", get(s_commit_diffs))
        .route("/commit/{rev}/diff/{*path}", get(s_commit_diff))
        .route("/refs", get(s_refs))
        .route("/releases", get(s_releases))
        .route("/blame/{*path}", get(s_blame))
        .route("/diff/{*path}", get(s_diff))
        .route("/files", get(s_files))
        .route("/archive", get(s_archive))
        .route("/prs", get(s_prs))
        .route("/search", get(s_search))
        .route("/semantic", get(s_semantic))
        .route("/agent", get(agent_page))
        .route("/api/repos", get(g_repos))
        .with_state(state)
}

fn multi_router(state: Shared) -> Router {
    Router::new()
        .route("/", get(m_index))
        .route("/agent", get(agent_page))
        .route("/search", get(g_search))
        .route("/semantic", get(g_semantic))
        .route("/api/repos", get(g_repos))
        .route("/{repo}", get(m_summary))
        .route("/{repo}/log", get(m_log))
        .route("/{repo}/tree", get(m_tree_root))
        .route("/{repo}/tree/{*path}", get(m_tree))
        .route("/{repo}/blob/{*path}", get(m_blob))
        .route("/{repo}/commit/{rev}", get(m_commit))
        .route("/{repo}/commit/{rev}/diffs", get(m_commit_diffs))
        .route("/{repo}/commit/{rev}/diff/{*path}", get(m_commit_diff))
        .route("/{repo}/refs", get(m_refs))
        .route("/{repo}/releases", get(m_releases))
        .route("/{repo}/blame/{*path}", get(m_blame))
        .route("/{repo}/diff/{*path}", get(m_diff))
        .route("/{repo}/files", get(m_files))
        .route("/{repo}/archive", get(m_archive))
        .route("/{repo}/prs", get(m_prs))
        .route("/{repo}/search", get(m_search))
        .route("/{repo}/semantic", get(m_semantic))
        .with_state(state)
}

/// Derive a display name for a repo from its working directory.
pub fn repo_name(backend: &dyn GitBackend) -> String {
    backend
        .workdir()
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo")
        .to_owned()
}

async fn run(router: Router, addr: SocketAddr) -> std::io::Result<()> {
    let app = router.layer(tower_http::compression::CompressionLayer::new());
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("rgit serve on http://{addr}");
    axum::serve(listener, app).await
}

/// Serve one repo (routes at `/...`) until the process ends. `clone_base`, when
/// set, makes the shown clone URL point at this server.
pub async fn serve(
    backend: Arc<dyn GitBackend>,
    repo: String,
    addr: SocketAddr,
    clone_base: Option<String>,
    mcp: Option<Router>,
) -> std::io::Result<()> {
    let _ = CLONE_BASE.set(clone_base);
    let state = Arc::new(AppState {
        repos: Repos::Single {
            backend,
            name: repo,
        },
    });
    run(with_mcp(single_router(state), mcp), addr).await
}

/// Serve every repo under a managed root (index at `/`, repos at `/{repo}/...`).
pub async fn serve_root(
    root: PathBuf,
    addr: SocketAddr,
    clone_base: Option<String>,
    mcp: Option<Router>,
) -> std::io::Result<()> {
    let _ = CLONE_BASE.set(clone_base);
    let state = Arc::new(AppState {
        repos: Repos::Multi(Registry::new(root)),
    });
    run(with_mcp(multi_router(state), mcp), addr).await
}

/// Merge the optional MCP router (POST /mcp) into the site router. The caller
/// (rgit-cli) owns the MCP handler, so it is passed in rather than built here.
fn with_mcp(router: Router, mcp: Option<Router>) -> Router {
    match mcp {
        Some(mcp) => router.merge(mcp),
        None => router,
    }
}

#[cfg(test)]
mod tests {
    use super::Registry;

    #[test]
    fn ci_mark_aggregates_check_states() {
        use serde_json::json;
        assert_eq!(super::ci_mark(&[]), "");
        let pass = [json!({"state": "SUCCESS"}), json!({"state": "SKIPPED"})];
        assert!(
            super::ci_mark(&pass).contains('\u{2713}'),
            "all-pass is a check"
        );
        let fail = [json!({"state": "SUCCESS"}), json!({"state": "FAILURE"})];
        assert!(
            super::ci_mark(&fail).contains('\u{2717}'),
            "any fail is a cross"
        );
        let pending = [json!({"state": "SUCCESS"}), json!({"state": "IN_PROGRESS"})];
        assert!(
            super::ci_mark(&pending).contains('\u{25cf}'),
            "any pending is a dot"
        );
    }

    #[test]
    fn repo_names_are_a_single_safe_segment() {
        assert!(Registry::valid("dftracer-utils"));
        assert!(Registry::valid("my_repo.git"));
        // Traversal and separators can never name a dir outside the root.
        assert!(!Registry::valid(".."));
        assert!(!Registry::valid("."));
        assert!(!Registry::valid(""));
        assert!(!Registry::valid(".hidden"));
        assert!(!Registry::valid("a/b"));
        assert!(!Registry::valid("a\\b"));
        assert!(!Registry::valid("../etc/passwd"));
    }
}
