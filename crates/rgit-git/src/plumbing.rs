//! Read-only plumbing over libgit2 for rgit's git-compatible commands
//! (rev-parse, ls-files, ls-tree, cat-file, for-each-ref, rev-list, grep, ...).

use std::path::Path;

use git2::{ObjectType, Oid, Pathspec, Repository, Status, StatusOptions};

use crate::error::GitError;

/// An author, committer or tagger: name, email, unix time and UTC offset in minutes.
#[derive(Debug, Clone, Default)]
pub struct Ident {
    pub name: String,
    pub email: String,
    pub time: i64,
    pub offset: i32,
}

impl Ident {
    fn from(sig: &git2::Signature) -> Self {
        Ident {
            name: String::from_utf8_lossy(sig.name_bytes()).into_owned(),
            email: String::from_utf8_lossy(sig.email_bytes()).into_owned(),
            time: sig.when().seconds(),
            offset: sig.when().offset_minutes(),
        }
    }
}

/// An object's type name (`blob`, `tree`, `commit`, `tag`), its id and raw content.
pub struct RawObject {
    pub id: String,
    pub kind: &'static str,
    pub data: Vec<u8>,
}

/// One entry of a tree listing (`git ls-tree`).
pub struct TreeItem {
    pub mode: i32,
    pub kind: &'static str,
    pub id: String,
    /// Blob size, when sizes were asked for.
    pub size: Option<u64>,
    pub path: String,
}

/// How `ls_tree` walks: recurse into subtrees (-r), show trees it recurses
/// into (-t), show only trees (-d), look up blob sizes (-l).
#[derive(Default, Clone, Copy)]
pub struct TreeWalk {
    pub recursive: bool,
    pub show_trees: bool,
    pub only_trees: bool,
    pub sizes: bool,
}

/// One index entry (`git ls-files --stage`).
pub struct IndexItem {
    pub mode: u32,
    pub id: String,
    pub stage: u8,
    pub path: String,
}

/// How a working-tree path differs from the index (`git ls-files -o/-i/-m/-d`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum PathState {
    Untracked,
    Ignored,
    Modified,
    Deleted,
}

/// A reference with what `for-each-ref` formats need.
pub struct RefDetail {
    /// Full name, e.g. `refs/heads/main`.
    pub name: String,
    /// The object the ref points at (a tag object for annotated tags).
    pub id: String,
    pub kind: &'static str,
    /// The non-tag object an annotated tag peels to.
    pub peeled: Option<String>,
    /// Where a symbolic ref points.
    pub symref: Option<String>,
    /// A local branch's upstream, e.g. `refs/remotes/origin/main`.
    pub upstream: Option<String>,
    /// The commit or tag message.
    pub message: String,
    pub author: Option<Ident>,
    pub committer: Option<Ident>,
    pub tagger: Option<Ident>,
    /// What an annotated tag peels to, for `%(*field)` atoms.
    pub deref: Option<Box<RefDetail>>,
}

/// A commit from `rev_walk`.
pub struct WalkCommit {
    pub id: String,
    pub parents: Vec<String>,
    pub author: Ident,
    pub committer: Ident,
    pub summary: String,
    /// git's mark for it, as [`crate::LogEntry::mark`].
    pub mark: Option<char>,
}

/// A reflog entry, newest first.
pub struct ReflogItem {
    pub id: String,
    pub message: String,
    /// Who moved the ref, and when.
    pub who: Ident,
}

/// How `git grep` reads its patterns.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum GrepSyntax {
    #[default]
    Basic,
    Extended,
    Fixed,
    /// Perl-compatible (`-P`), with lookaround and backreferences.
    Perl,
}

/// How `git grep` combines its patterns (`--and`, `--or`, `--not`, `( )`):
/// atoms index into [`GitGrep::patterns`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrepExpr {
    Atom(usize),
    Not(Box<GrepExpr>),
    And(Box<GrepExpr>, Box<GrepExpr>),
    Or(Box<GrepExpr>, Box<GrepExpr>),
}

/// A `git grep` search over tracked files: the working tree, the index
/// (`cached`) or a revision's tree.
#[derive(Default, Clone)]
pub struct GitGrep {
    pub patterns: Vec<String>,
    pub syntax: GrepSyntax,
    pub ignore_case: bool,
    pub word: bool,
    pub invert: bool,
    pub cached: bool,
    pub rev: Option<String>,
    pub paths: Vec<String>,
    /// Context lines before and after each match (`-B`, `-A`).
    pub before: usize,
    pub after: usize,
    /// Stop a file after this many matching lines (`-m`).
    pub max_count: Option<u64>,
    /// Fill `GrepHit::parts` with each match (`-o`).
    pub only_matching: bool,
    /// Fill each hit's match `spans` (to color them).
    pub spans: bool,
    /// Return one `line` 0 hit per file with no match instead (`-L`).
    pub files_without_match: bool,
    /// Skip binary files (`-I`).
    pub skip_binary: bool,
    /// How the patterns combine; `None` matches any of them.
    pub expr: Option<GrepExpr>,
    /// Keep only files where every top-level `--or` branch matches a line.
    pub all_match: bool,
    /// Show the line naming the enclosing function (`-p`).
    pub show_function: bool,
    /// Show the whole enclosing function (`-W`).
    pub function_context: bool,
    /// Search untracked files too (`--untracked`).
    pub untracked: bool,
    /// Search ignored files too with `untracked` (`--no-exclude-standard`).
    pub no_exclude: bool,
    /// Search the checked-out submodules too.
    pub recurse_submodules: bool,
}

/// A matching line, a context line, or a whole file (`line` 0): a binary file
/// that matches, or with `files_without_match` a file that does not.
pub struct GrepHit {
    pub path: String,
    pub line: u64,
    pub text: String,
    pub binary: bool,
    pub context: bool,
    /// A context line naming the enclosing function (`-p`, `-W`).
    pub function: bool,
    /// The matching parts of the line, with `only_matching`.
    pub parts: Vec<String>,
    /// The byte ranges of the matches in `text`, with `spans`.
    pub spans: Vec<(usize, usize)>,
}

/// The ignore rule that excludes a path (`git check-ignore -v`).
pub struct IgnoreRule {
    pub source: String,
    pub line: usize,
    pub pattern: String,
    /// A `!pattern` that keeps the path from being ignored.
    pub negated: bool,
}

/// Object store counts (`git count-objects -v`); sizes in KiB.
pub struct ObjectCounts {
    pub count: usize,
    pub size: u64,
    pub in_pack: usize,
    pub packs: usize,
    pub size_pack: u64,
    pub prune_packable: usize,
}

fn kind_name(kind: Option<ObjectType>) -> &'static str {
    match kind {
        Some(ObjectType::Blob) => "blob",
        Some(ObjectType::Tree) => "tree",
        Some(ObjectType::Commit) => "commit",
        Some(ObjectType::Tag) => "tag",
        _ => "unknown",
    }
}

/// libgit2's revparse, plus git's `:path` and `:<stage>:path` index entries.
fn revparse<'r>(repo: &'r Repository, rev: &str) -> Result<git2::Object<'r>, GitError> {
    if let Some(rest) = rev.strip_prefix(':')
        && !rest.is_empty()
        && !rest.starts_with('/')
    {
        let (stage, path) = match rest.split_once(':') {
            Some((n @ ("0" | "1" | "2" | "3"), p)) => (n.parse().unwrap_or(0), p),
            _ => (0, rest),
        };
        if let Some(e) = repo.index()?.get_path(Path::new(path), stage) {
            return Ok(repo.find_object(e.id, None)?);
        }
    }
    Ok(repo.revparse_single(rev)?)
}

pub(crate) fn resolve(repo: &Repository, rev: &str) -> Result<String, GitError> {
    Ok(revparse(repo, rev)?.id().to_string())
}

/// git's abbreviation length when none is asked for: core.abbrev, or with
/// `auto` (the default) half the bits of the packed object count, at least 7.
fn default_abbrev(repo: &Repository) -> usize {
    if let Ok(v) = repo.config().and_then(|c| c.get_string("core.abbrev"))
        && !v.eq_ignore_ascii_case("auto")
    {
        return match v.to_ascii_lowercase().as_str() {
            "false" | "no" | "off" => 40,
            n => n.parse().unwrap_or(7),
        };
    }
    let count = pack_index(repo).count as u64;
    let bits = 64 - count.leading_zeros() as usize;
    bits.div_ceil(2).max(7)
}

/// The shortest unique prefix of `id` of at least `min` digits; `min` 0 is
/// git's default length.
pub(crate) fn abbrev(repo: &Repository, id: &str, min: usize) -> Result<String, GitError> {
    let mut len = if min == 0 {
        default_abbrev(repo)
    } else {
        min.max(4)
    }
    .min(id.len());
    let objects = repo.commondir().join("objects");
    let Ok(oid) = Oid::from_str(id) else {
        return Ok(id[..len].to_owned());
    };
    if objects.join("info/alternates").exists() {
        let odb = repo.odb()?;
        while len < id.len() {
            match odb.exists_prefix(Oid::from_str(&id[..len])?, len) {
                Err(e) if e.code() == git2::ErrorCode::Ambiguous => len += 1,
                _ => break,
            }
        }
        return Ok(id[..len].to_owned());
    }
    // The digits it shares with its neighbours among packed and loose ids.
    let common = |a: &[u8], b: &[u8]| {
        let bytes = a.iter().zip(b).take_while(|(x, y)| x == y).count();
        match (a.get(bytes), b.get(bytes)) {
            (Some(x), Some(y)) if x >> 4 == y >> 4 => bytes * 2 + 1,
            _ => bytes * 2,
        }
    };
    let raw = oid.as_bytes();
    let index = pack_index(repo);
    let at = index.ids.partition_point(|x| x.as_slice() < raw);
    let mut shared = [at.checked_sub(1), Some(at), Some(at + 1)]
        .into_iter()
        .flatten()
        .filter_map(|i| index.ids.get(i))
        .filter(|x| x.as_slice() != raw)
        .map(|x| common(x, raw))
        .max()
        .unwrap_or(0);
    for e in std::fs::read_dir(objects.join(&id[..2]))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = e.file_name();
        let rest = name.to_string_lossy();
        if rest.len() == id.len() - 2 && rest != id[2..] {
            let n = rest
                .bytes()
                .zip(id[2..].bytes())
                .take_while(|(a, b)| a == b)
                .count();
            shared = shared.max(n + 2);
        }
    }
    len = len.max(shared + 1).min(id.len());
    Ok(id[..len].to_owned())
}

pub(crate) fn full_ref_name(repo: &Repository, rev: &str) -> Result<Option<String>, GitError> {
    let (_, reference) = repo.revparse_ext(rev)?;
    let Some(reference) = reference else {
        return Ok(None);
    };
    let reference = if reference.symbolic_target_bytes().is_some() {
        reference.resolve()?
    } else {
        reference
    };
    Ok(Some(
        String::from_utf8_lossy(reference.name_bytes()).into_owned(),
    ))
}

pub(crate) fn symbolic_ref(repo: &Repository, name: &str) -> Result<Option<String>, GitError> {
    let reference = repo.find_reference(name)?;
    Ok(reference
        .symbolic_target_bytes()
        .map(|t| String::from_utf8_lossy(t).into_owned()))
}

pub(crate) fn read_object(repo: &Repository, rev: &str) -> Result<RawObject, GitError> {
    let id = revparse(repo, rev)?.id();
    let odb = repo.odb()?;
    let obj = odb.read(id)?;
    Ok(RawObject {
        id: id.to_string(),
        kind: kind_name(Some(obj.kind())),
        data: obj.data().to_vec(),
    })
}

pub(crate) fn ls_tree(
    repo: &Repository,
    rev: &str,
    paths: &[String],
    opts: TreeWalk,
) -> Result<Vec<TreeItem>, GitError> {
    let tree = repo.revparse_single(rev)?.peel_to_tree()?;
    let mut out = Vec::new();
    walk_tree(repo, &tree, "", paths, opts, &mut out)?;
    Ok(out)
}

fn walk_tree(
    repo: &Repository,
    tree: &git2::Tree,
    prefix: &str,
    paths: &[String],
    opts: TreeWalk,
    out: &mut Vec<TreeItem>,
) -> Result<(), GitError> {
    for e in tree.iter() {
        let path = format!("{prefix}{}", String::from_utf8_lossy(e.name_bytes()));
        let is_tree = e.kind() == Some(ObjectType::Tree);
        let (mut show, mut descend) = (paths.is_empty(), paths.is_empty() && opts.recursive);
        for p in paths {
            let t = p.trim_end_matches('/');
            if t.is_empty() || t == "." || path.starts_with(&format!("{t}/")) {
                show = true;
                descend |= opts.recursive;
            } else if path == t {
                if p.ends_with('/') && is_tree {
                    descend = true;
                } else {
                    show = true;
                    descend |= opts.recursive;
                }
            } else if t.starts_with(&format!("{path}/")) {
                descend = true;
            }
        }
        let item = || -> Result<TreeItem, GitError> {
            let size = if opts.sizes && e.kind() == Some(ObjectType::Blob) {
                Some(repo.odb()?.read_header(e.id())?.0 as u64)
            } else {
                None
            };
            Ok(TreeItem {
                mode: e.filemode(),
                kind: kind_name(e.kind()),
                id: e.id().to_string(),
                size,
                path: path.clone(),
            })
        };
        if is_tree && descend {
            if opts.show_trees || opts.only_trees && show {
                out.push(item()?);
            }
            let sub = repo.find_tree(e.id())?;
            walk_tree(repo, &sub, &format!("{path}/"), paths, opts, out)?;
        } else if show && (!opts.only_trees || is_tree) {
            out.push(item()?);
        }
    }
    Ok(())
}

pub(crate) fn index_entries(repo: &Repository) -> Result<Vec<IndexItem>, GitError> {
    let index = repo.index()?;
    Ok(index
        .iter()
        .map(|e| IndexItem {
            mode: e.mode,
            id: e.id.to_string(),
            stage: ((e.flags >> 12) & 3) as u8,
            path: String::from_utf8_lossy(&e.path).into_owned(),
        })
        .collect())
}

pub(crate) fn path_states(
    repo: &Repository,
    ignored: bool,
) -> Result<Vec<(String, PathState)>, GitError> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(ignored)
        .recurse_ignored_dirs(ignored)
        .exclude_submodules(true);
    let mut out = Vec::new();
    for e in repo.statuses(Some(&mut opts))?.iter() {
        let s = e.status();
        let state = if s.contains(Status::IGNORED) {
            PathState::Ignored
        } else if s.contains(Status::WT_NEW) {
            PathState::Untracked
        } else if s.contains(Status::WT_DELETED) {
            PathState::Deleted
        } else if s.intersects(Status::WT_MODIFIED | Status::WT_TYPECHANGE | Status::WT_RENAMED) {
            PathState::Modified
        } else {
            continue;
        };
        out.push((String::from_utf8_lossy(e.path_bytes()).into_owned(), state));
    }
    out.sort();
    Ok(out)
}

pub(crate) fn ref_details(repo: &Repository) -> Result<Vec<RefDetail>, GitError> {
    let mut out = Vec::new();
    for reference in repo.references()? {
        let reference = reference?;
        let name = String::from_utf8_lossy(reference.name_bytes()).into_owned();
        let symref = reference
            .symbolic_target_bytes()
            .map(|t| String::from_utf8_lossy(t).into_owned());
        let Ok(id) = reference.resolve().map(|r| r.target()) else {
            continue;
        };
        let Some(id) = id else { continue };
        let obj = repo.find_object(id, None)?;
        let upstream = name
            .starts_with("refs/heads/")
            .then(|| repo.branch_upstream_name(&name).ok())
            .flatten()
            .and_then(|b| b.as_str().ok().map(str::to_owned));
        let mut detail = object_detail(&obj, name);
        detail.upstream = upstream;
        detail.symref = symref;
        if obj.kind() == Some(ObjectType::Tag)
            && let Ok(target) = obj.peel(ObjectType::Any)
        {
            detail.peeled = Some(target.id().to_string());
            detail.deref = Some(Box::new(object_detail(&target, String::new())));
        }
        out.push(detail);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// A ref's details from the object it points at, without ref-only fields.
fn object_detail(obj: &git2::Object, name: String) -> RefDetail {
    let mut detail = RefDetail {
        name,
        id: obj.id().to_string(),
        kind: kind_name(obj.kind()),
        peeled: None,
        symref: None,
        upstream: None,
        message: String::new(),
        author: None,
        committer: None,
        tagger: None,
        deref: None,
    };
    if let Some(commit) = obj.as_commit() {
        detail.message = String::from_utf8_lossy(commit.message_bytes()).into_owned();
        detail.author = Some(Ident::from(&commit.author()));
        detail.committer = Some(Ident::from(&commit.committer()));
    } else if let Some(tag) = obj.as_tag() {
        detail.message =
            String::from_utf8_lossy(tag.message_bytes().unwrap_or_default()).into_owned();
        detail.tagger = tag.tagger().as_ref().map(Ident::from);
    }
    detail
}

fn commit_id(repo: &Repository, rev: &str) -> Result<Oid, GitError> {
    Ok(repo.revparse_single(rev)?.peel_to_commit()?.id())
}

pub(crate) fn rev_walk(
    repo: &Repository,
    opts: &crate::LogOptions,
) -> Result<Vec<WalkCommit>, GitError> {
    let mut out = Vec::new();
    for w in crate::walk::walk(repo, opts)? {
        let commit = repo.find_commit(w.id)?;
        out.push(WalkCommit {
            id: w.id.to_string(),
            parents: w.parents.iter().map(Oid::to_string).collect(),
            author: Ident::from(&commit.author()),
            committer: Ident::from(&commit.committer()),
            summary: commit
                .summary_bytes()
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .unwrap_or_default(),
            mark: w.mark,
        });
    }
    Ok(out)
}

pub(crate) fn list_objects(
    repo: &Repository,
    commits: &[String],
    edges: &[String],
) -> Result<Vec<(String, String, bool)>, GitError> {
    fn mark(repo: &Repository, tree: Oid, seen: &mut std::collections::HashSet<Oid>) {
        if !seen.insert(tree) {
            return;
        }
        let Ok(tree) = repo.find_tree(tree) else {
            return;
        };
        for e in tree.iter() {
            if e.kind() == Some(ObjectType::Tree) {
                mark(repo, e.id(), seen);
            } else {
                seen.insert(e.id());
            }
        }
    }
    fn walk(
        repo: &Repository,
        tree: Oid,
        path: &str,
        seen: &mut std::collections::HashSet<Oid>,
        out: &mut Vec<(String, String, bool)>,
    ) {
        if !seen.insert(tree) {
            return;
        }
        let Ok(t) = repo.find_tree(tree) else {
            out.push((tree.to_string(), path.to_owned(), true));
            return;
        };
        out.push((tree.to_string(), path.to_owned(), false));
        for e in t.iter() {
            let name = String::from_utf8_lossy(e.name_bytes());
            let sub = if path.is_empty() {
                name.into_owned()
            } else {
                format!("{path}/{name}")
            };
            match e.kind() {
                Some(ObjectType::Tree) => walk(repo, e.id(), &sub, seen, out),
                // Submodule commits are not in this repository.
                Some(ObjectType::Commit) => {}
                _ => {
                    if seen.insert(e.id()) {
                        let missing = repo.odb().is_ok_and(|db| !db.exists(e.id()));
                        out.push((e.id().to_string(), sub, missing));
                    }
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    for id in edges {
        mark(
            repo,
            repo.find_commit(Oid::from_str(id)?)?.tree_id(),
            &mut seen,
        );
    }
    let mut out = Vec::new();
    for id in commits {
        let tree = repo.find_commit(Oid::from_str(id)?)?.tree_id();
        walk(repo, tree, "", &mut seen, &mut out);
    }
    Ok(out)
}

pub(crate) fn merge_bases(
    repo: &Repository,
    a: &str,
    b: &str,
    all: bool,
) -> Result<Vec<String>, GitError> {
    let (a, b) = (commit_id(repo, a)?, commit_id(repo, b)?);
    if !all {
        return Ok(match repo.merge_base(a, b) {
            Ok(base) => vec![base.to_string()],
            Err(e) if e.code() == git2::ErrorCode::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        });
    }
    match repo.merge_bases(a, b) {
        Ok(bases) => Ok(bases.iter().map(Oid::to_string).collect()),
        Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn reflog(repo: &Repository, name: &str) -> Result<Vec<ReflogItem>, GitError> {
    let full = if name == "HEAD" {
        name.to_owned()
    } else {
        let reference = repo.resolve_reference_from_short_name(name)?;
        String::from_utf8_lossy(reference.name_bytes()).into_owned()
    };
    Ok(repo
        .reflog(&full)?
        .iter()
        .map(|e| ReflogItem {
            id: e.id_new().to_string(),
            who: Ident::from(&e.committer()),
            message: e
                .message_bytes()
                .map(|m| String::from_utf8_lossy(m).into_owned())
                .unwrap_or_default(),
        })
        .collect())
}

/// A POSIX basic regex as the Rust regex syntax: `\+ \? \| \{ \} \( \)` are
/// operators and their bare forms are literal.
pub(crate) fn basic_to_extended(pattern: &str) -> String {
    let mut out = String::new();
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(n @ ('+' | '?' | '|' | '{' | '}' | '(' | ')')) => out.push(n),
                Some(n) => {
                    out.push('\\');
                    out.push(n);
                }
                None => out.push_str("\\\\"),
            },
            '+' | '?' | '|' | '{' | '}' | '(' | ')' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

/// One `grep` pattern, compiled for its syntax.
enum Pattern {
    Plain(grep::regex::RegexMatcher),
    Perl(fancy_regex::Regex),
}

impl Pattern {
    fn new(p: &str, q: &GitGrep) -> Result<Self, GitError> {
        let bad = |e: String| GitError::Other(e);
        if q.syntax == GrepSyntax::Perl {
            let mut p = format!("(?:{p})");
            if q.word {
                p = format!(r"(?<!\w){p}(?!\w)");
            }
            if q.ignore_case {
                p = format!("(?i){p}");
            }
            return fancy_regex::Regex::new(&p)
                .map(Pattern::Perl)
                .map_err(|e| bad(e.to_string()));
        }
        let p = match q.syntax {
            GrepSyntax::Fixed => regex::escape(p),
            GrepSyntax::Basic => basic_to_extended(p),
            _ => p.to_owned(),
        };
        grep::regex::RegexMatcherBuilder::new()
            .case_insensitive(q.ignore_case)
            .word(q.word)
            .build(&p)
            .map(Pattern::Plain)
            .map_err(|e| bad(e.to_string()))
    }

    /// The first match in `line` at or after `at`.
    fn find_at(&self, line: &[u8], at: usize) -> Option<(usize, usize)> {
        match self {
            Pattern::Plain(m) => {
                use grep::matcher::Matcher;
                m.find_at(line, at)
                    .ok()
                    .flatten()
                    .map(|m| (m.start(), m.end()))
            }
            Pattern::Perl(re) => {
                let text = std::str::from_utf8(line).ok()?;
                let m = re.find_from_pos(text, at).ok().flatten()?;
                Some((m.start(), m.end()))
            }
        }
    }
}

fn eval(e: &GrepExpr, atoms: &[Pattern], line: &[u8]) -> bool {
    match e {
        GrepExpr::Atom(i) => atoms[*i].find_at(line, 0).is_some(),
        GrepExpr::Not(x) => !eval(x, atoms, line),
        GrepExpr::And(a, b) => eval(a, atoms, line) && eval(b, atoms, line),
        GrepExpr::Or(a, b) => eval(a, atoms, line) || eval(b, atoms, line),
    }
}

/// A `diff.<driver>.xfuncname` (or `funcname`) pattern list: the first
/// matching line decides, and a `!` line rejects.
type Funcname = Vec<(bool, regex::bytes::Regex)>;

fn funcname_driver(repo: &Repository, path: &str) -> Option<Funcname> {
    let driver = match git2::AttrValue::from_string(
        repo.get_attr(Path::new(path), "diff", git2::AttrCheckFlags::default())
            .ok()
            .flatten(),
    ) {
        git2::AttrValue::String(s) => s.to_owned(),
        _ => return None,
    };
    let config = repo.config().ok()?;
    let (text, basic) = match config.get_string(&format!("diff.{driver}.xfuncname")) {
        Ok(t) => (t, false),
        Err(_) => (
            config.get_string(&format!("diff.{driver}.funcname")).ok()?,
            true,
        ),
    };
    // ponytail: git's built-in drivers (cpp, rust, ...) need their patterns
    // copied in; until then an attribute without config uses the default.
    text.split('\n')
        .map(|l| {
            let (neg, l) = l.strip_prefix('!').map_or((false, l), |l| (true, l));
            let l = if basic {
                basic_to_extended(l)
            } else {
                l.to_owned()
            };
            regex::bytes::Regex::new(&l).ok().map(|re| (neg, re))
        })
        .collect()
}

fn is_funcname(driver: Option<&Funcname>, line: &[u8]) -> bool {
    match driver {
        Some(pats) => pats
            .iter()
            .find(|(_, re)| re.is_match(line))
            .is_some_and(|(neg, _)| !neg),
        None => line
            .first()
            .is_some_and(|&b| b.is_ascii_alphabetic() || b == b'_' || b == b'$'),
    }
}

fn is_blank(line: &[u8]) -> bool {
    line.iter()
        .all(|b| matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r'))
}

/// One file's matching and context lines, as git's `grep_source_1` picks them.
struct FileSearch<'a> {
    q: &'a GitGrep,
    atoms: &'a [Pattern],
    path: &'a str,
    lines: Vec<&'a [u8]>,
    driver: Option<&'a Funcname>,
    last_shown: usize,
    hits: Vec<GrepHit>,
}

impl FileSearch<'_> {
    fn func(&self, lno: usize) -> bool {
        is_funcname(self.driver, self.lines[lno - 1])
    }

    fn show(&mut self, lno: usize, sign: char) {
        let bytes = self.lines[lno - 1];
        let mut parts = Vec::new();
        let mut spans = Vec::new();
        if (self.q.only_matching || self.q.spans) && sign == ':' {
            let mut at = 0;
            // git's next_match: the earliest match of any pattern, the
            // longest on a tie; an empty match ends the line.
            while let Some((s, e)) = self
                .atoms
                .iter()
                .filter_map(|a| a.find_at(bytes, at))
                .min_by_key(|&(s, e)| (s, std::cmp::Reverse(e)))
            {
                if s == e {
                    break;
                }
                spans.push((s, e));
                if self.q.only_matching {
                    parts.push(String::from_utf8_lossy(&bytes[s..e]).into_owned());
                }
                at = e;
            }
        }
        self.hits.push(GrepHit {
            path: self.path.to_owned(),
            line: lno as u64,
            text: String::from_utf8_lossy(bytes)
                .trim_end_matches('\r')
                .to_owned(),
            binary: false,
            context: sign != ':',
            function: sign == '=',
            parts,
            spans,
        });
        self.last_shown = lno;
    }

    fn funcname_line(&mut self, mut lno: usize) {
        while lno > 1 {
            lno -= 1;
            if lno <= self.last_shown {
                break;
            }
            if self.func(lno) {
                self.show(lno, '=');
                break;
            }
        }
    }

    fn pre_context(&mut self, lno: usize) {
        let q = self.q;
        let mut from = lno.saturating_sub(q.before).max(1);
        if from <= self.last_shown {
            from = self.last_shown + 1;
        }
        let orig_from = from;
        let (mut funcname_needed, mut comment_needed) = (q.show_function, false);
        if q.function_context {
            if self.func(lno) {
                comment_needed = true;
            } else {
                funcname_needed = true;
            }
            from = self.last_shown + 1;
        }
        let (mut cur, mut funcname_lno) = (lno, 0);
        while cur > 1 && cur > from {
            cur -= 1;
            let line = self.lines[cur - 1];
            if comment_needed && (is_blank(line) || self.func(cur)) {
                comment_needed = false;
                from = orig_from;
                if cur < from {
                    cur += 1;
                    break;
                }
            }
            if funcname_needed && self.func(cur) {
                funcname_lno = cur;
                funcname_needed = false;
                if q.function_context {
                    comment_needed = true;
                } else {
                    from = orig_from;
                }
            }
        }
        if q.show_function && funcname_needed {
            self.funcname_line(cur);
        }
        while cur < lno {
            self.show(cur, if cur == funcname_lno { '=' } else { '-' });
            cur += 1;
        }
    }

    fn run(&mut self, expr: &GrepExpr) {
        let q = self.q;
        let (mut count, mut last_hit, mut show_function) = (0u64, 0, false);
        let mut peek: Option<usize> = None;
        for lno in 1..=self.lines.len() {
            let line = self.lines[lno - 1];
            if eval(expr, self.atoms, line) != q.invert && q.max_count.is_none_or(|m| count < m) {
                count += 1;
                if q.before > 0 || q.function_context {
                    self.pre_context(lno);
                } else if q.show_function {
                    self.funcname_line(lno);
                }
                self.show(lno, ':');
                last_hit = lno;
                show_function |= q.function_context;
                continue;
            }
            // Trailing blank lines belong to the function only when more
            // of its body follows them.
            if show_function && peek.is_none_or(|p| p < lno) {
                let mut p = lno;
                while p <= self.lines.len() && is_blank(self.lines[p - 1]) {
                    p += 1;
                }
                if p > self.lines.len() || self.func(p) {
                    show_function = false;
                }
                peek = Some(p);
            }
            if show_function || last_hit > 0 && lno <= last_hit + q.after {
                self.show(lno, '-');
            }
        }
    }
}

/// The `--or` branches at the top of `e`, each of which `--all-match` needs
/// on some line.
fn or_chain(e: &GrepExpr) -> Vec<&GrepExpr> {
    match e {
        GrepExpr::Or(a, b) => {
            let mut v = vec![&**a];
            v.extend(or_chain(b));
            v
        }
        e => vec![e],
    }
}

/// A file to search: its contents, or where to read them.
enum Source {
    Data(Vec<u8>),
    File(std::path::PathBuf),
}

/// The tracked files `q` searches in `repo`, named under `prefix`: the
/// working tree, the index or a revision, and the submodules' with
/// `recurse_submodules`.
fn tracked(
    repo: &Repository,
    workdir: &Path,
    prefix: &str,
    rev: Option<Oid>,
    q: &GitGrep,
    keep: &dyn Fn(&str) -> bool,
    files: &mut Vec<(String, Source)>,
) -> Result<(), GitError> {
    // Each path with its blob, or its commit for a submodule.
    let mut entries: Vec<(String, Oid, bool)> = Vec::new();
    if let Some(rev) = rev {
        let tree = repo.find_object(rev, None)?.peel_to_tree()?;
        tree.walk(git2::TreeWalkMode::PreOrder, |root, e| {
            let path = format!("{prefix}{root}{}", String::from_utf8_lossy(e.name_bytes()));
            match e.kind() {
                Some(ObjectType::Blob) => entries.push((path, e.id(), false)),
                Some(ObjectType::Commit) => entries.push((path, e.id(), true)),
                _ => {}
            }
            git2::TreeWalkResult::Ok
        })?;
    } else {
        for e in repo.index()?.iter() {
            let path = format!("{prefix}{}", String::from_utf8_lossy(&e.path));
            if entries.last().is_none_or(|(last, _, _)| *last != path) {
                entries.push((path, e.id, e.mode == 0o160000));
            }
        }
    }
    for (path, id, link) in entries {
        if link {
            let dir = workdir.join(&path[prefix.len()..]);
            if q.recurse_submodules
                && submodule_active(repo, &path[prefix.len()..])
                && let Ok(sub) = Repository::open(&dir)
            {
                let rev = rev.map(|_| id);
                tracked(&sub, &dir, &format!("{path}/"), rev, q, keep, files)?;
            }
        } else if keep(&path) {
            let src = if q.cached || rev.is_some() {
                Source::Data(repo.find_blob(id)?.content().to_vec())
            } else {
                Source::File(workdir.join(&path[prefix.len()..]))
            };
            files.push((path, src));
        }
    }
    Ok(())
}

/// Whether the submodule at `path` is active, as git decides: its
/// `submodule.<name>.active`, else a `submodule.active` pathspec, else a
/// `submodule.<name>.url` (set by `submodule init`).
fn submodule_active(repo: &Repository, path: &str) -> bool {
    let (Ok(sub), Ok(config)) = (repo.find_submodule(path), repo.config()) else {
        return false;
    };
    let name = sub.name().unwrap_or(path);
    if let Ok(active) = config.get_bool(&format!("submodule.{name}.active")) {
        return active;
    }
    let mut specs: Vec<String> = Vec::new();
    if let Ok(entries) = config.multivar("submodule.active", None) {
        let _ = entries.for_each(|e| specs.extend(e.value().map(str::to_owned)));
    }
    if !specs.is_empty() {
        return Pathspec::new(specs.iter())
            .is_ok_and(|s| s.matches_path(Path::new(path), crate::pathspec_flags()));
    }
    config.get_string(&format!("submodule.{name}.url")).is_ok()
}

/// Every file under `root` but `.git`, as `git grep --no-index` walks it;
/// `exclude` leaves out what `.gitignore` ignores.
fn walk_files(root: &Path, exclude: bool) -> Vec<String> {
    let mut out: Vec<String> = ignore::WalkBuilder::new(root)
        .hidden(false)
        .ignore(false)
        .parents(exclude)
        .git_ignore(exclude)
        .git_exclude(exclude)
        .git_global(exclude)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter_map(|e| {
            let p = e.path().strip_prefix(root).ok()?;
            Some(p.to_string_lossy().into_owned())
        })
        .collect();
    out.sort();
    out
}

/// `git grep --no-index`: search every file under `root`, tracked or not;
/// `exclude` leaves out ignored ones (`--exclude-standard`).
pub fn grep_dir(root: &Path, q: &GitGrep, exclude: bool) -> Result<Vec<GrepHit>, GitError> {
    let keep = keeper(q)?;
    let files = walk_files(root, exclude)
        .into_iter()
        .filter(|p| keep(p))
        .map(|p| {
            let file = root.join(&p);
            (p, Source::File(file))
        })
        .collect();
    search(None, files, q)
}

fn keeper(q: &GitGrep) -> Result<impl Fn(&str) -> bool, GitError> {
    let spec = (!q.paths.is_empty())
        .then(|| Pathspec::new(q.paths.iter()))
        .transpose()?;
    Ok(move |p: &str| {
        spec.as_ref()
            .is_none_or(|s| s.matches_path(Path::new(p), crate::pathspec_flags()))
    })
}

pub(crate) fn grep(
    repo: &Repository,
    workdir: &Path,
    q: &GitGrep,
) -> Result<Vec<GrepHit>, GitError> {
    let keep = keeper(q)?;
    let rev = q
        .rev
        .as_deref()
        .map(|r| repo.revparse_single(r).map(|o| o.id()))
        .transpose()?;
    let mut files = Vec::new();
    tracked(repo, workdir, "", rev, q, &keep, &mut files)?;
    if q.untracked && rev.is_none() && !q.cached {
        let mut opts = StatusOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(true)
            .include_ignored(q.no_exclude)
            .recurse_ignored_dirs(q.no_exclude);
        for s in repo.statuses(Some(&mut opts))?.iter() {
            if s.status().intersects(Status::WT_NEW | Status::IGNORED)
                && let Ok(path) = s.path()
                && keep(path)
                && !path.ends_with('/')
            {
                files.push((path.to_owned(), Source::File(workdir.join(path))));
            }
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
    }
    search(Some(repo), files, q)
}

fn search(
    repo: Option<&Repository>,
    files: Vec<(String, Source)>,
    q: &GitGrep,
) -> Result<Vec<GrepHit>, GitError> {
    use rayon::prelude::*;

    let atoms = q
        .patterns
        .iter()
        .map(|p| Pattern::new(p, q))
        .collect::<Result<Vec<_>, _>>()?;
    let Some(expr) = q.expr.clone().or_else(|| {
        (0..atoms.len())
            .rev()
            .map(GrepExpr::Atom)
            .reduce(|b, a| GrepExpr::Or(Box::new(a), Box::new(b)))
    }) else {
        return Err(GitError::Other("no pattern given".into()));
    };
    let chain = or_chain(&expr);
    let funcnames = q.show_function || q.function_context;
    let drivers: Vec<Option<Funcname>> = files
        .iter()
        .map(|(p, _)| {
            repo.filter(|_| funcnames)
                .and_then(|r| funcname_driver(r, p))
        })
        .collect();
    Ok(files
        .into_par_iter()
        .zip(drivers)
        .flat_map_iter(|((path, src), driver)| {
            let data = match src {
                Source::Data(d) => d,
                Source::File(f) => match std::fs::read(f) {
                    Ok(d) => d,
                    Err(_) => return Vec::new().into_iter(),
                },
            };
            let binary = data[..data.len().min(8000)].contains(&0);
            let mut lines: Vec<&[u8]> = data.split(|&b| b == b'\n').collect();
            if data.is_empty() || data.ends_with(b"\n") {
                lines.pop();
            }
            let mut file = FileSearch {
                q,
                atoms: &atoms,
                path: &path,
                lines,
                driver: driver.as_ref(),
                last_shown: 0,
                hits: Vec::new(),
            };
            let all = !q.all_match
                || chain
                    .iter()
                    .all(|e| file.lines.iter().any(|l| eval(e, &atoms, l)));
            if !(binary && q.skip_binary) && all {
                file.run(&expr);
            }
            let mut hits = file.hits;
            let matched = hits.iter().any(|h| !h.context);
            let whole = |binary| GrepHit {
                path: path.clone(),
                line: 0,
                text: String::new(),
                binary,
                context: false,
                function: false,
                parts: Vec::new(),
                spans: Vec::new(),
            };
            if q.files_without_match {
                hits = if matched || binary && q.skip_binary {
                    Vec::new()
                } else {
                    vec![whole(false)]
                };
            } else if matched && binary {
                hits = vec![whole(true)];
            }
            hits.into_iter()
        })
        .collect())
}

pub(crate) fn check_ignore(
    repo: &Repository,
    workdir: &Path,
    path: &str,
    no_index: bool,
) -> Result<Option<IgnoreRule>, GitError> {
    if !no_index && repo.index()?.get_path(Path::new(path), 0).is_some() {
        return Ok(None);
    }
    let ignored = repo.is_path_ignored(path)?;
    let is_dir = workdir.join(path).is_dir();
    let git_dir = repo
        .path()
        .strip_prefix(workdir)
        .unwrap_or(repo.path())
        .to_path_buf();
    // Sources from the highest precedence: the deepest .gitignore first.
    let mut sources: Vec<(std::path::PathBuf, String, &str)> = Vec::new();
    let mut dir = Path::new(path).parent();
    while let Some(d) = dir {
        let file = d.join(".gitignore");
        let rel = path
            .strip_prefix(&format!("{}/", d.display()))
            .unwrap_or(path);
        sources.push((file.clone(), file.display().to_string(), rel));
        dir = d.parent();
    }
    let exclude = git_dir.join("info/exclude");
    sources.push((exclude.clone(), exclude.display().to_string(), path));
    if let Ok(global) = repo.config()?.get_path("core.excludesFile") {
        sources.push((global.clone(), global.display().to_string(), path));
    }
    for (file, source, rel) in sources {
        let Ok(text) = std::fs::read_to_string(workdir.join(&file)) else {
            continue;
        };
        for (i, line) in text
            .lines()
            .collect::<Vec<_>>()
            .into_iter()
            .enumerate()
            .rev()
        {
            let mut builder = ignore::gitignore::GitignoreBuilder::new("");
            if builder.add_line(None, line).is_err() {
                continue;
            }
            let Ok(gi) = builder.build() else { continue };
            let negated = match gi.matched_path_or_any_parents(rel, is_dir) {
                ignore::Match::Ignore(_) => false,
                ignore::Match::Whitelist(_) => true,
                ignore::Match::None => continue,
            };
            return Ok((negated != ignored).then(|| IgnoreRule {
                source,
                line: i + 1,
                pattern: line.to_owned(),
                negated,
            }));
        }
    }
    Ok(ignored.then(|| IgnoreRule {
        source: String::new(),
        line: 0,
        pattern: String::new(),
        negated: false,
    }))
}

pub(crate) fn ident(repo: &Repository, committer: bool) -> Result<String, GitError> {
    let role = if committer { "COMMITTER" } else { "AUTHOR" };
    let config = repo.config()?;
    let get = |var: &str, key: &str| {
        std::env::var(format!("GIT_{role}_{var}"))
            .ok()
            .or_else(|| config.get_string(key).ok())
    };
    let (Some(name), Some(email)) = (get("NAME", "user.name"), get("EMAIL", "user.email")) else {
        return Err(GitError::Other(format!(
            "{} identity unknown; set user.name and user.email",
            if committer { "committer" } else { "author" }
        )));
    };
    let now = git2::Signature::now(&name, &email)?.when();
    let (seconds, offset) = std::env::var(format!("GIT_{role}_DATE"))
        .ok()
        .and_then(|d| parse_git_date(&d, now.offset_minutes()))
        .unwrap_or((now.seconds(), now.offset_minutes()));
    let sign = if offset < 0 { '-' } else { '+' };
    Ok(format!(
        "{name} <{email}> {seconds} {sign}{:02}{:02}",
        offset.abs() / 60,
        offset.abs() % 60
    ))
}

/// A `+HHMM` / `-HHMM` zone (or `Z`) as minutes east of UTC.
fn parse_zone(s: &str) -> Option<i32> {
    if matches!(s, "Z" | "UTC" | "GMT") {
        return Some(0);
    }
    let (sign, digits) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    let digits = digits.replace(':', "");
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i32 = digits.parse().ok()?;
    Some(sign * (n / 100 * 60 + n % 100))
}

/// A date as git reads GIT_AUTHOR_DATE: `@<unix> [<zone>]`, `<unix> <zone>`,
/// ISO 8601 (`2024-01-02T10:00:00+0200`) or RFC 2822
/// (`Mon, 3 Jul 2006 17:18:43 +0200`), with the time and zone (minutes east
/// of UTC); `local` is the zone when none is given.
pub(crate) fn parse_git_date(s: &str, local: i32) -> Option<(i64, i32)> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let s = s.trim();
    let raw = |r: &str| {
        let mut it = r.split_whitespace();
        let t = it.next()?.parse().ok()?;
        Some((t, it.next().and_then(parse_zone).unwrap_or(local)))
    };
    if let Some(r) = s.strip_prefix('@') {
        return raw(r);
    }
    if s.split_whitespace()
        .next()
        .is_some_and(|t| t.len() >= 9 && t.bytes().all(|b| b.is_ascii_digit()))
    {
        return raw(s);
    }
    let (mut date, mut time, mut zone) = (None, (0, 0, 0), None);
    let (mut day, mut month, mut year) = (None, None, None);
    let mut tokens: Vec<String> = Vec::new();
    for tok in s.split([' ', ',']).filter(|t| !t.is_empty()) {
        match tok.split_once('T') {
            Some((d, t)) if d.contains('-') => tokens.extend([d.to_owned(), t.to_owned()]),
            _ => tokens.push(tok.to_owned()),
        }
    }
    for tok in &tokens {
        if let Some(z) = parse_zone(tok) {
            zone = Some(z);
        } else if tok.contains(':') {
            let cut = tok.find(['+', '-', 'Z']).unwrap_or(tok.len());
            zone = parse_zone(&tok[cut..]).or(zone);
            let mut parts = tok[..cut].split(':').map(|p| p.parse::<i64>().ok());
            time = (
                parts.next()??,
                parts.next()??,
                parts.next().flatten().unwrap_or(0),
            );
        } else if let [y, m, d] = tok.split('-').collect::<Vec<_>>()[..] {
            date = Some((y.parse().ok()?, m.parse().ok()?, d.parse().ok()?));
        } else if let Some(m) = MONTHS
            .iter()
            .position(|m| tok.to_ascii_lowercase().starts_with(m))
        {
            month = Some(m as i64 + 1);
        } else if let Ok(n) = tok.parse::<i64>() {
            if n > 31 {
                year = Some(n)
            } else {
                day = Some(n)
            }
        }
    }
    let (y, m, d) = date.or_else(|| Some((year?, month?, day?)))?;
    // days_from_civil (Howard Hinnant, public domain).
    let yy = if m <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
    let secs = days * 86400 + time.0 * 3600 + time.1 * 60 + time.2;
    let zone = zone.unwrap_or(local);
    Some((secs - i64::from(zone) * 60, zone))
}

#[cfg(test)]
#[test]
fn dates_parse_like_git() {
    assert_eq!(
        parse_git_date("2024-01-02T10:00:00+0200", 0),
        Some((1704182400, 120))
    );
    assert_eq!(
        parse_git_date("@1700000000 +0100", 0),
        Some((1700000000, 60))
    );
    assert_eq!(
        parse_git_date("1700000000 -0130", 0),
        Some((1700000000, -90))
    );
    assert_eq!(
        parse_git_date("Mon, 3 Jul 2006 17:18:43 +0200", 0),
        Some((1151939923, 120))
    );
    assert_eq!(
        parse_git_date("2024-01-02 10:00:00", 60),
        Some((1704182400 + 3600, 60))
    );
}

/// One `.idx` file: ids sorted, each id's offset in its `.pack`.
struct PackIdx {
    pack: std::path::PathBuf,
    ids: Vec<[u8; 20]>,
    offsets: Vec<u64>,
    /// (offset, index into `ids`), sorted by offset.
    by_offset: Vec<(u64, usize)>,
    /// Where the last object's data ends: the pack's size less its checksum.
    end: u64,
}

impl PackIdx {
    /// Read a v2 index (git has written nothing older since 1.5.2).
    fn read(idx: &Path) -> Option<PackIdx> {
        let data = std::fs::read(idx).ok()?;
        if data.get(..8)? != b"\xfftOc\0\0\0\x02" {
            return None;
        }
        let u32_at = |at: usize| Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?));
        let n = u32_at(8 + 255 * 4)? as usize;
        let ids_at = 8 + 256 * 4;
        let offsets_at = ids_at + n * 24;
        let large_at = offsets_at + n * 4;
        let mut ids = Vec::with_capacity(n);
        let mut offsets = Vec::with_capacity(n);
        for i in 0..n {
            ids.push(
                data.get(ids_at + i * 20..ids_at + i * 20 + 20)?
                    .try_into()
                    .ok()?,
            );
            let off = u32_at(offsets_at + i * 4)?;
            offsets.push(if off & 0x8000_0000 == 0 {
                u64::from(off)
            } else {
                let at = large_at + (off & 0x7fff_ffff) as usize * 8;
                u64::from_be_bytes(data.get(at..at + 8)?.try_into().ok()?)
            });
        }
        let mut by_offset: Vec<(u64, usize)> = offsets.iter().copied().zip(0..).collect();
        by_offset.sort_unstable();
        let pack = idx.with_extension("pack");
        let end = std::fs::metadata(&pack).ok()?.len().saturating_sub(20);
        Some(PackIdx {
            pack,
            ids,
            offsets,
            by_offset,
            end,
        })
    }
}

/// Every pack index of a repository, read once per process and again only
/// when the pack folder changes.
pub(crate) struct PackIndex {
    packs: Vec<PackIdx>,
    /// Every packed id, sorted and deduplicated, for abbreviations.
    ids: Vec<[u8; 20]>,
    /// Packed objects counting duplicates, as git's approximate count does.
    count: usize,
}

pub(crate) fn pack_index(repo: &Repository) -> std::sync::Arc<PackIndex> {
    use std::sync::{Arc, Mutex};
    type Cache =
        std::collections::HashMap<std::path::PathBuf, (std::time::SystemTime, Arc<PackIndex>)>;
    static CACHE: Mutex<Option<Cache>> = Mutex::new(None);
    let dir = repo.commondir().join("objects/pack");
    let stamp = std::fs::metadata(&dir)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH);
    let mut cache = CACHE.lock().expect("pack index cache");
    let cache = cache.get_or_insert_with(Default::default);
    if let Some((at, index)) = cache.get(&dir)
        && *at == stamp
    {
        return index.clone();
    }
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "idx"))
        .collect();
    paths.sort();
    let packs: Vec<PackIdx> = paths.iter().filter_map(|p| PackIdx::read(p)).collect();
    let count = packs.iter().map(|p| p.ids.len()).sum();
    let mut ids: Vec<[u8; 20]> = packs.iter().flat_map(|p| p.ids.iter().copied()).collect();
    ids.sort_unstable();
    ids.dedup();
    let index = Arc::new(PackIndex { packs, ids, count });
    cache.insert(dir, (stamp, index.clone()));
    index
}

/// An object's size on disk and the object it is stored as a delta of
/// (cat-file's `%(objectsize:disk)` and `%(deltabase)`).
pub(crate) fn object_disk(repo: &Repository, id: &str) -> Result<(u64, Option<String>), GitError> {
    let oid = Oid::from_str(id)?;
    let raw: [u8; 20] = oid
        .as_bytes()
        .try_into()
        .map_err(|_| GitError::Other("bad id".into()))?;
    for pack in &pack_index(repo).packs {
        let Ok(i) = pack.ids.binary_search(&raw) else {
            continue;
        };
        let at = pack.offsets[i];
        let pos = pack.by_offset.partition_point(|&(o, _)| o <= at);
        let next = pack.by_offset.get(pos).map_or(pack.end, |&(o, _)| o);
        let mut head = [0u8; 32];
        let mut file = std::fs::File::open(&pack.pack)?;
        std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(at))?;
        let got = std::io::Read::read(&mut file, &mut head)?;
        let head = &head[..got];
        let kind = head.first().map_or(0, |b| (b >> 4) & 7);
        let mut p = 1 + head.iter().take_while(|b| *b & 0x80 != 0).count();
        let base = match kind {
            6 => {
                let mut c = head.get(p).copied().unwrap_or(0);
                let mut back = u64::from(c & 0x7f);
                while c & 0x80 != 0 {
                    p += 1;
                    c = head.get(p).copied().unwrap_or(0);
                    back = ((back + 1) << 7) | u64::from(c & 0x7f);
                }
                let base_at = at.saturating_sub(back);
                pack.by_offset
                    .binary_search_by_key(&base_at, |&(o, _)| o)
                    .ok()
                    .map(|j| Oid::from_bytes(&pack.ids[pack.by_offset[j].1]))
                    .transpose()?
            }
            7 => head.get(p..p + 20).map(Oid::from_bytes).transpose()?,
            _ => None,
        };
        return Ok((next - at, base.map(|b| b.to_string())));
    }
    let loose = repo
        .commondir()
        .join("objects")
        .join(&id[..2])
        .join(&id[2..]);
    Ok((std::fs::metadata(loose)?.len(), None))
}

pub(crate) fn count_objects(repo: &Repository) -> Result<ObjectCounts, GitError> {
    let objects = repo.commondir().join("objects");
    let disk = |m: &std::fs::Metadata| {
        #[cfg(unix)]
        {
            std::os::unix::fs::MetadataExt::blocks(m) * 512
        }
        #[cfg(not(unix))]
        {
            m.len()
        }
    };
    let mut packed = std::collections::HashSet::new();
    let (mut in_pack, mut packs, mut size_pack) = (0, 0, 0);
    if let Ok(dir) = std::fs::read_dir(objects.join("pack")) {
        for e in dir.flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "pack") {
                continue;
            }
            let idx = std::fs::read(p.with_extension("idx"))?;
            // idx v2: magic, version, 256 fanout counts, then the sorted ids.
            let n = u32::from_be_bytes(idx[1028..1032].try_into().unwrap_or_default()) as usize;
            for i in 0..n {
                let at = 1032 + i * 20;
                if let Some(raw) = idx.get(at..at + 20) {
                    packed.insert(raw.to_vec());
                }
            }
            in_pack += n;
            packs += 1;
            size_pack += e.metadata()?.len() + idx.len() as u64;
        }
    }
    let (mut count, mut size, mut prune_packable) = (0, 0, 0);
    for d in std::fs::read_dir(&objects)?.flatten() {
        let name = d.file_name().to_string_lossy().into_owned();
        if name.len() != 2 || !name.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        for f in std::fs::read_dir(d.path())?.flatten() {
            count += 1;
            size += disk(&f.metadata()?);
            let hex = format!("{name}{}", f.file_name().to_string_lossy());
            if Oid::from_str(&hex).is_ok_and(|oid| packed.contains(oid.as_bytes())) {
                prune_packable += 1;
            }
        }
    }
    Ok(ObjectCounts {
        count,
        size: size / 1024,
        in_pack,
        packs,
        size_pack: size_pack / 1024,
        prune_packable,
    })
}

/// `git hash-object`: the id of `data` as a `kind` object, in the repository at
/// `git_dir` or outside one. A blob with a `path` (relative to the working tree)
/// goes through git's clean filters first: a `filter=` driver, then
/// autocrlf/`text`/`eol` and `ident`. `write` stores the object; `literally`
/// skips the format check of trees, commits and tags.
pub fn hash_object(
    git_dir: Option<&Path>,
    kind: &str,
    data: &[u8],
    path: Option<&str>,
    write: bool,
    literally: bool,
) -> Result<String, GitError> {
    let kind = ObjectType::from_str(kind)
        .filter(|k| {
            matches!(
                k,
                ObjectType::Blob | ObjectType::Tree | ObjectType::Commit | ObjectType::Tag
            )
        })
        .ok_or_else(|| GitError::Other(format!("invalid object type \"{kind}\"")))?;
    if !literally {
        check_object(kind, data)?;
    }
    let repo = git_dir.map(Repository::open).transpose()?;
    let filtered;
    let data = match (&repo, path) {
        (Some(repo), Some(path)) if kind == ObjectType::Blob && repo.workdir().is_some() => {
            filtered = clean(repo, path, data)?;
            &filtered[..]
        }
        _ => data,
    };
    let oid = match (&repo, write) {
        (Some(repo), true) => repo.odb()?.write(kind, data)?,
        (None, true) => return Err(GitError::Other("not in a git directory".to_owned())),
        _ => Oid::hash_object(kind, data)?,
    };
    Ok(oid.to_string())
}

/// `data` as git would store the file `path`: its clean filter driver, then
/// libgit2's crlf/eol and ident filters.
fn clean(repo: &Repository, path: &str, data: &[u8]) -> Result<Vec<u8>, GitError> {
    let data = filter_driver(repo, path, data.to_vec(), "clean")?;
    builtin_filters(repo, path, data, true)
}

/// A blob as git would check it out to `path`: libgit2's ident and crlf/eol
/// filters, then its smudge filter driver (`cat-file --filters`).
pub(crate) fn smudge(repo: &Repository, path: &str, data: &[u8]) -> Result<Vec<u8>, GitError> {
    let data = builtin_filters(repo, path, data.to_vec(), false)?;
    filter_driver(repo, path, data, "smudge")
}

/// A blob through `path`'s `diff.<driver>.textconv` command, or unchanged
/// without one (`cat-file --textconv`).
pub(crate) fn textconv(repo: &Repository, path: &str, data: &[u8]) -> Result<Vec<u8>, GitError> {
    let driver = repo
        .get_attr(Path::new(path), "diff", git2::AttrCheckFlags::default())?
        .map(str::to_owned);
    let Some(cmd) = driver.and_then(|d| {
        repo.config()
            .ok()?
            .get_string(&format!("diff.{d}.textconv"))
            .ok()
    }) else {
        return Ok(data.to_vec());
    };
    // git hands the command the file as it would be checked out.
    let data = smudge(repo, path, data)?;
    let tmp = std::env::temp_dir().join(format!("rgit-textconv-{}", std::process::id()));
    std::fs::write(&tmp, &data)?;
    let out = std::process::Command::new("sh")
        .args(["-c", &format!("{cmd} \"$@\""), &cmd])
        .arg(&tmp)
        .current_dir(repo.workdir().unwrap_or(repo.path()))
        .output();
    let _ = std::fs::remove_file(&tmp);
    let out = out?;
    if !out.status.success() {
        return Err(GitError::Other("unable to read files to diff".to_owned()));
    }
    Ok(out.stdout)
}

/// Run `path`'s `filter.<driver>.<which>` command on `data`, if it has one.
fn filter_driver(
    repo: &Repository,
    path: &str,
    mut data: Vec<u8>,
    which: &str,
) -> Result<Vec<u8>, GitError> {
    let driver = repo
        .get_attr(Path::new(path), "filter", git2::AttrCheckFlags::default())?
        .map(str::to_owned);
    if let Some(name) = driver {
        let config = repo.config()?;
        let required = config
            .get_bool(&format!("filter.{name}.required"))
            .unwrap_or(false);
        match config.get_string(&format!("filter.{name}.{which}")) {
            Ok(cmd) => {
                let quoted = format!("'{}'", path.replace('\'', "'\\''"));
                let workdir = repo.workdir().unwrap_or(repo.path());
                match run_filter(&cmd.replace("%f", &quoted), workdir, &data) {
                    Ok(out) => data = out,
                    Err(e) if required => return Err(e),
                    Err(_) => {}
                }
            }
            Err(_) if required => {
                return Err(GitError::Other(format!(
                    "{path}: {which} filter '{name}' failed"
                )));
            }
            Err(_) => {}
        }
    }
    Ok(data)
}

/// libgit2's crlf/eol and ident filters for `path`, toward the object
/// database or the working tree.
fn builtin_filters(
    repo: &Repository,
    path: &str,
    data: Vec<u8>,
    to_odb: bool,
) -> Result<Vec<u8>, GitError> {
    use std::ffi::{CString, c_char, c_int, c_void};
    unsafe extern "C" {
        fn git_filter_list_load(
            filters: *mut *mut c_void,
            repo: *mut libgit2_sys::git_repository,
            blob: *mut libgit2_sys::git_blob,
            path: *const c_char,
            mode: c_int,
            flags: u32,
        ) -> c_int;
        fn git_filter_list_apply_to_buffer(
            out: *mut libgit2_sys::git_buf,
            filters: *mut c_void,
            data: *const c_char,
            len: usize,
        ) -> c_int;
        fn git_filter_list_free(filters: *mut c_void);
    }
    const ALLOW_UNSAFE: u32 = 1;
    let cpath = CString::new(path).map_err(|_| GitError::Other(format!("bad path {path:?}")))?;
    let mut filters = std::ptr::null_mut();
    // SAFETY: the repo outlives the call; libgit2 owns `filters` until freed,
    // and `out` until disposed.
    unsafe {
        let rc = git_filter_list_load(
            &mut filters,
            git2::Binding::raw(repo),
            std::ptr::null_mut(),
            cpath.as_ptr(),
            c_int::from(to_odb),
            ALLOW_UNSAFE,
        );
        if rc < 0 {
            return Err(git2::Error::last_error(rc).into());
        }
        if filters.is_null() {
            return Ok(data);
        }
        let mut out = libgit2_sys::git_buf {
            ptr: std::ptr::null_mut(),
            reserved: 0,
            size: 0,
        };
        let rc =
            git_filter_list_apply_to_buffer(&mut out, filters, data.as_ptr().cast(), data.len());
        git_filter_list_free(filters);
        if rc < 0 {
            libgit2_sys::git_buf_dispose(&mut out);
            return Err(git2::Error::last_error(rc).into());
        }
        let bytes = if out.ptr.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(out.ptr as *const u8, out.size).to_vec()
        };
        libgit2_sys::git_buf_dispose(&mut out);
        Ok(bytes)
    }
}

/// Run a filter driver command through the shell, feeding it `data`.
fn run_filter(cmd: &str, dir: &Path, data: &[u8]) -> Result<Vec<u8>, GitError> {
    use std::io::Write;
    let mut child = std::process::Command::new("sh")
        .args(["-c", cmd])
        .current_dir(dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let input = data.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let out = child.wait_with_output()?;
    let _ = writer.join();
    if !out.status.success() {
        return Err(GitError::Other(format!("filter '{cmd}' failed")));
    }
    Ok(out.stdout)
}

/// git's format check for `hash-object -t tree|commit|tag`.
fn check_object(kind: ObjectType, data: &[u8]) -> Result<(), GitError> {
    let bad = |why: &str| Err(GitError::Other(format!("object fails fsck: {why}")));
    let hex = |s: &str| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit());
    match kind {
        ObjectType::Tree => {
            let mut rest = data;
            while !rest.is_empty() {
                let Some(nul) = rest.iter().position(|b| *b == 0) else {
                    return bad("badTree: truncated entry");
                };
                let head = String::from_utf8_lossy(&rest[..nul]);
                let ok = head.split_once(' ').is_some_and(|(mode, name)| {
                    !name.is_empty() && mode.bytes().all(|b| (b'0'..=b'7').contains(&b))
                });
                if !ok || rest.len() < nul + 21 {
                    return bad("badTree: malformed entry");
                }
                rest = &rest[nul + 21..];
            }
            Ok(())
        }
        ObjectType::Commit | ObjectType::Tag => {
            let text = String::from_utf8_lossy(data);
            let header = text.split("\n\n").next().unwrap_or_default();
            let mut lines = header.lines();
            let mut field = |name: &str| {
                lines
                    .next()
                    .and_then(|l| l.strip_prefix(name))
                    .and_then(|l| l.strip_prefix(' '))
                    .map(str::to_owned)
            };
            if kind == ObjectType::Commit {
                if !field("tree").is_some_and(|t| hex(&t)) {
                    return bad("missingTree: invalid format - expected 'tree' line");
                }
                let rest: Vec<&str> = header.lines().skip(1).collect();
                let after_parents = rest
                    .iter()
                    .skip_while(|l| l.strip_prefix("parent ").is_some_and(hex))
                    .collect::<Vec<_>>();
                if !after_parents
                    .first()
                    .is_some_and(|l| l.starts_with("author "))
                {
                    return bad("missingAuthor: invalid format - expected 'author' line");
                }
                if !after_parents
                    .get(1)
                    .is_some_and(|l| l.starts_with("committer "))
                {
                    return bad("missingCommitter: invalid format - expected 'committer' line");
                }
            } else {
                if !field("object").is_some_and(|o| hex(&o)) {
                    return bad("missingObject: invalid format - expected 'object' line");
                }
                if field("type").is_none() {
                    return bad("missingTypeEntry: invalid format - expected 'type' line");
                }
                if field("tag").is_none() {
                    return bad("missingTagEntry: invalid format - expected 'tag' line");
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
