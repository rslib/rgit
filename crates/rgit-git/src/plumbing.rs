//! Read-only plumbing over libgit2 for rgit's git-compatible commands
//! (rev-parse, ls-files, ls-tree, cat-file, for-each-ref, rev-list, grep, ...).

use std::path::Path;

use git2::{ObjectType, Oid, Pathspec, PathspecFlags, Repository, Status, StatusOptions};

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
}

/// What `rev_walk` walks, as git's rev-list takes it.
#[derive(Default, Clone)]
pub struct RevWalk {
    /// `A`, `^A`, `A..B` or `A...B`.
    pub revs: Vec<String>,
    pub all: bool,
    pub first_parent: bool,
    pub merges: bool,
    pub no_merges: bool,
    pub max: Option<usize>,
    /// Leave out the first N commits (`--skip`).
    pub skip: usize,
    /// Ref globs to walk too (`refs/heads/*` for `--branches`).
    pub globs: Vec<String>,
    /// Never show a parent before all its children (`--topo-order`).
    pub topo: bool,
    /// Only commits that change these paths, with git's default history
    /// simplification: a merge that matches one parent on the paths follows
    /// only that parent.
    pub paths: Vec<String>,
}

/// A commit from `rev_walk`.
pub struct WalkCommit {
    pub id: String,
    pub parents: Vec<String>,
    pub author: Ident,
    pub committer: Ident,
    pub summary: String,
}

/// A reflog entry, newest first.
pub struct ReflogItem {
    pub id: String,
    pub message: String,
}

/// How `git grep` reads its patterns.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum GrepSyntax {
    #[default]
    Basic,
    Extended,
    Fixed,
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
    /// Return one `line` 0 hit per file with no match instead (`-L`).
    pub files_without_match: bool,
    /// Skip binary files (`-I`).
    pub skip_binary: bool,
}

/// A matching line, a context line, or a whole file (`line` 0): a binary file
/// that matches, or with `files_without_match` a file that does not.
pub struct GrepHit {
    pub path: String,
    pub line: u64,
    pub text: String,
    pub binary: bool,
    pub context: bool,
    /// The matching parts of the line, with `only_matching`.
    pub parts: Vec<String>,
}

/// The ignore rule that excludes a path (`git check-ignore -v`).
pub struct IgnoreRule {
    pub source: String,
    pub line: usize,
    pub pattern: String,
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

pub(crate) fn resolve(repo: &Repository, rev: &str) -> Result<String, GitError> {
    Ok(repo.revparse_single(rev)?.id().to_string())
}

/// git's abbreviation length when none is asked for: core.abbrev, or with
/// `auto` (the default) half the bits of the packed object count, at least 7.
// ponytail: reads the pack index headers on every call; cache per repo if
// long listings get slow.
fn default_abbrev(repo: &Repository) -> usize {
    if let Ok(v) = repo.config().and_then(|c| c.get_string("core.abbrev"))
        && !v.eq_ignore_ascii_case("auto")
    {
        return match v.to_ascii_lowercase().as_str() {
            "false" | "no" | "off" => 40,
            n => n.parse().unwrap_or(7),
        };
    }
    let count: u64 = std::fs::read_dir(repo.commondir().join("objects/pack"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "idx"))
        .filter_map(|e| {
            let mut head = [0u8; 1032];
            std::io::Read::read_exact(&mut std::fs::File::open(e.path()).ok()?, &mut head).ok()?;
            Some(u64::from(u32::from_be_bytes(head[1028..].try_into().ok()?)))
        })
        .sum();
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
    let odb = repo.odb()?;
    while len < id.len() {
        match odb.exists_prefix(Oid::from_str(&id[..len])?, len) {
            Err(e) if e.code() == git2::ErrorCode::Ambiguous => len += 1,
            _ => break,
        }
    }
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
    let id = repo.revparse_single(rev)?.id();
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
        let mut detail = RefDetail {
            upstream: name
                .starts_with("refs/heads/")
                .then(|| repo.branch_upstream_name(&name).ok())
                .flatten()
                .and_then(|b| b.as_str().ok().map(str::to_owned)),
            name,
            id: id.to_string(),
            kind: kind_name(obj.kind()),
            peeled: None,
            symref,
            message: String::new(),
            author: None,
            committer: None,
            tagger: None,
        };
        if let Some(commit) = obj.as_commit() {
            detail.message = String::from_utf8_lossy(commit.message_bytes()).into_owned();
            detail.author = Some(Ident::from(&commit.author()));
            detail.committer = Some(Ident::from(&commit.committer()));
        } else if let Some(tag) = obj.as_tag() {
            detail.message =
                String::from_utf8_lossy(tag.message_bytes().unwrap_or_default()).into_owned();
            detail.tagger = tag.tagger().as_ref().map(Ident::from);
            detail.peeled = obj.peel(ObjectType::Any).ok().map(|o| o.id().to_string());
        }
        out.push(detail);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn commit_id(repo: &Repository, rev: &str) -> Result<Oid, GitError> {
    Ok(repo.revparse_single(rev)?.peel_to_commit()?.id())
}

pub(crate) fn rev_walk(repo: &Repository, opts: &RevWalk) -> Result<Vec<WalkCommit>, GitError> {
    let mut walk = repo.revwalk()?;
    // Path limiting marks parents from children, so children must come first.
    walk.set_sorting(if opts.topo || !opts.paths.is_empty() {
        git2::Sort::TOPOLOGICAL | git2::Sort::TIME
    } else {
        git2::Sort::TIME
    })?;
    if opts.first_parent {
        walk.simplify_first_parent()?;
    }
    let mut tips: std::collections::HashSet<Oid> = std::collections::HashSet::new();
    let mut push = |walk: &mut git2::Revwalk, id: Oid| -> Result<(), GitError> {
        tips.insert(id);
        Ok(walk.push(id)?)
    };
    let mut globs = opts.globs.clone();
    if opts.all {
        globs.push("refs/*".to_owned());
        if let Ok(id) = commit_id(repo, "HEAD") {
            push(&mut walk, id)?;
        }
    }
    for glob in &globs {
        for r in repo.references_glob(glob)? {
            if let Ok(commit) = r?.peel_to_commit() {
                push(&mut walk, commit.id())?;
            }
        }
    }
    let or_head = |s: &str| {
        if s.is_empty() {
            "HEAD".to_owned()
        } else {
            s.to_owned()
        }
    };
    for rev in &opts.revs {
        if let Some(hidden) = rev.strip_prefix('^') {
            walk.hide(commit_id(repo, hidden)?)?;
        } else if let Some((a, b)) = rev.split_once("...") {
            let (a, b) = (commit_id(repo, &or_head(a))?, commit_id(repo, &or_head(b))?);
            push(&mut walk, a)?;
            push(&mut walk, b)?;
            if let Ok(bases) = repo.merge_bases(a, b) {
                for base in bases.iter() {
                    walk.hide(*base)?;
                }
            }
        } else if let Some((a, b)) = rev.split_once("..") {
            walk.hide(commit_id(repo, &or_head(a))?)?;
            push(&mut walk, commit_id(repo, &or_head(b))?)?;
        } else {
            push(&mut walk, commit_id(repo, rev)?)?;
        }
    }
    let mut diff_opts = git2::DiffOptions::new();
    for p in &opts.paths {
        diff_opts.pathspec(p);
    }
    // Whether `commit` matches its parent `n` (or the empty tree) on the paths.
    let mut same_as = |commit: &git2::Commit, n: Option<usize>| -> Result<bool, GitError> {
        let old = n.map(|n| commit.parent(n)?.tree()).transpose()?;
        let diff =
            repo.diff_tree_to_tree(old.as_ref(), Some(&commit.tree()?), Some(&mut diff_opts))?;
        Ok(diff.deltas().len() == 0)
    };
    // With paths, a commit is walked only from a starting commit or through
    // a parent its child follows.
    let mut wanted = tips;
    let (mut out, mut skipped) = (Vec::new(), 0);
    for oid in walk {
        if opts.max.is_some_and(|m| out.len() >= m) {
            break;
        }
        let commit = repo.find_commit(oid?)?;
        let merge = commit.parent_count() > 1;
        if !opts.paths.is_empty() {
            if !wanted.contains(&commit.id()) {
                continue;
            }
            let parents = if opts.first_parent {
                commit.parent_count().min(1)
            } else {
                commit.parent_count()
            };
            let same = (0..parents)
                .map(|n| same_as(&commit, Some(n)).map(|same| same.then_some(n)))
                .find_map(Result::transpose)
                .transpose()?;
            let show = match same {
                // A commit that matches a parent follows only that parent.
                Some(n) => {
                    wanted.insert(commit.parent_id(n)?);
                    false
                }
                None if parents == 0 => !same_as(&commit, None)?,
                None => {
                    wanted.extend(commit.parent_ids().take(parents));
                    true
                }
            };
            if !show {
                continue;
            }
        }
        if opts.merges && !merge || opts.no_merges && merge {
            continue;
        }
        if skipped < opts.skip {
            skipped += 1;
            continue;
        }
        out.push(WalkCommit {
            id: commit.id().to_string(),
            parents: commit.parent_ids().map(|p| p.to_string()).collect(),
            author: Ident::from(&commit.author()),
            committer: Ident::from(&commit.committer()),
            summary: commit
                .summary_bytes()
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .unwrap_or_default(),
        });
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
            message: e
                .message_bytes()
                .map(|m| String::from_utf8_lossy(m).into_owned())
                .unwrap_or_default(),
        })
        .collect())
}

/// A POSIX basic regex as the Rust regex syntax: `\+ \? \| \{ \} \( \)` are
/// operators and their bare forms are literal.
fn basic_to_extended(pattern: &str) -> String {
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

pub(crate) fn grep(
    repo: &Repository,
    workdir: &Path,
    q: &GitGrep,
) -> Result<Vec<GrepHit>, GitError> {
    use grep::regex::RegexMatcherBuilder;
    use grep::searcher::{BinaryDetection, SearcherBuilder};
    use rayon::prelude::*;

    let patterns: Vec<String> = q
        .patterns
        .iter()
        .map(|p| match q.syntax {
            GrepSyntax::Fixed => regex::escape(p),
            GrepSyntax::Basic => basic_to_extended(p),
            GrepSyntax::Extended => p.clone(),
        })
        .collect();
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(q.ignore_case)
        .word(q.word)
        .build_many(&patterns)
        .map_err(|e| GitError::Other(e.to_string()))?;
    let spec = (!q.paths.is_empty())
        .then(|| Pathspec::new(q.paths.iter()))
        .transpose()?;
    let keep = |p: &str| {
        spec.as_ref()
            .is_none_or(|s| s.matches_path(Path::new(p), PathspecFlags::DEFAULT))
    };

    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    if let Some(rev) = &q.rev {
        let tree = repo.revparse_single(rev)?.peel_to_tree()?;
        let mut blobs = Vec::new();
        tree.walk(git2::TreeWalkMode::PreOrder, |root, e| {
            if e.kind() == Some(ObjectType::Blob) {
                let path = format!("{root}{}", String::from_utf8_lossy(e.name_bytes()));
                if keep(&path) {
                    blobs.push((path, e.id()));
                }
            }
            git2::TreeWalkResult::Ok
        })?;
        for (path, id) in blobs {
            files.push((path, repo.find_blob(id)?.content().to_vec()));
        }
    } else {
        let mut entries: Vec<(String, Oid)> = Vec::new();
        for e in repo.index()?.iter() {
            let path = String::from_utf8_lossy(&e.path).into_owned();
            if e.mode != 0o160000
                && keep(&path)
                && entries.last().is_none_or(|(last, _)| *last != path)
            {
                entries.push((path, e.id));
            }
        }
        if q.cached {
            for (path, id) in entries {
                files.push((path, repo.find_blob(id)?.content().to_vec()));
            }
        } else {
            files = entries
                .into_par_iter()
                .filter_map(|(path, _)| {
                    let data = std::fs::read(workdir.join(&path)).ok()?;
                    Some((path, data))
                })
                .collect();
        }
    }

    Ok(files
        .par_iter()
        .flat_map_iter(|(path, data)| {
            let binary = data[..data.len().min(8000)].contains(&0);
            let mut sink = Hits {
                path,
                matcher: &matcher,
                only: q.only_matching,
                hits: Vec::new(),
            };
            if !(binary && q.skip_binary) {
                let _ = SearcherBuilder::new()
                    .line_number(true)
                    .invert_match(q.invert)
                    .before_context(q.before)
                    .after_context(q.after)
                    .max_matches(q.max_count)
                    .binary_detection(BinaryDetection::none())
                    .build()
                    .search_slice(&matcher, data, &mut sink);
            }
            let mut hits = sink.hits;
            let matched = hits.iter().any(|h| !h.context);
            let whole = |binary| GrepHit {
                path: path.clone(),
                line: 0,
                text: String::new(),
                binary,
                context: false,
                parts: Vec::new(),
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

/// Collects a file's matching and context lines for `grep`.
struct Hits<'a> {
    path: &'a str,
    matcher: &'a grep::regex::RegexMatcher,
    only: bool,
    hits: Vec<GrepHit>,
}

impl Hits<'_> {
    fn push(&mut self, line: Option<u64>, bytes: &[u8], context: bool) {
        use grep::matcher::Matcher;
        let mut parts = Vec::new();
        if self.only && !context {
            let _ = self.matcher.find_iter(bytes, |m| {
                if !m.is_empty() {
                    parts.push(String::from_utf8_lossy(&bytes[m]).into_owned());
                }
                true
            });
        }
        self.hits.push(GrepHit {
            path: self.path.to_owned(),
            line: line.unwrap_or(0),
            text: String::from_utf8_lossy(bytes)
                .trim_end_matches(['\n', '\r'])
                .to_owned(),
            binary: false,
            context,
            parts,
        });
    }
}

impl grep::searcher::Sink for &mut Hits<'_> {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _: &grep::searcher::Searcher,
        m: &grep::searcher::SinkMatch<'_>,
    ) -> Result<bool, Self::Error> {
        self.push(m.line_number(), m.bytes(), false);
        Ok(true)
    }

    fn context(
        &mut self,
        _: &grep::searcher::Searcher,
        c: &grep::searcher::SinkContext<'_>,
    ) -> Result<bool, Self::Error> {
        self.push(c.line_number(), c.bytes(), true);
        Ok(true)
    }
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
    if !repo.is_path_ignored(path)? {
        return Ok(None);
    }
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
            match gi.matched_path_or_any_parents(rel, is_dir) {
                ignore::Match::Ignore(_) => {
                    return Ok(Some(IgnoreRule {
                        source,
                        line: i + 1,
                        pattern: line.to_owned(),
                    }));
                }
                ignore::Match::Whitelist(_) => break,
                ignore::Match::None => {}
            }
        }
    }
    Ok(Some(IgnoreRule {
        source: String::new(),
        line: 0,
        pattern: String::new(),
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
