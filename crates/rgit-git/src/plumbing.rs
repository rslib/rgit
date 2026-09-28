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
}

/// A matching line, or a whole binary file that matches (`line` 0).
pub struct GrepHit {
    pub path: String,
    pub line: u64,
    pub text: String,
    pub binary: bool,
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

pub(crate) fn abbrev(repo: &Repository, id: &str, min: usize) -> Result<String, GitError> {
    let short = repo
        .find_object(Oid::from_str(id)?, None)
        .ok()
        .and_then(|obj| obj.short_id().ok()?.as_str().ok().map(str::to_owned))
        .unwrap_or_else(|| id[..7.min(id.len())].to_owned());
    Ok(if min > short.len() {
        id[..min.min(id.len())].to_owned()
    } else {
        short
    })
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
    walk.set_sorting(git2::Sort::TIME)?;
    if opts.first_parent {
        walk.simplify_first_parent()?;
    }
    if opts.all {
        walk.push_glob("*")?;
        let _ = walk.push_head();
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
            walk.push(a)?;
            walk.push(b)?;
            if let Ok(bases) = repo.merge_bases(a, b) {
                for base in bases.iter() {
                    walk.hide(*base)?;
                }
            }
        } else if let Some((a, b)) = rev.split_once("..") {
            walk.hide(commit_id(repo, &or_head(a))?)?;
            walk.push(commit_id(repo, &or_head(b))?)?;
        } else {
            walk.push(commit_id(repo, rev)?)?;
        }
    }
    let mut out = Vec::new();
    for oid in walk {
        if opts.max.is_some_and(|m| out.len() >= m) {
            break;
        }
        let commit = repo.find_commit(oid?)?;
        let merge = commit.parent_count() > 1;
        if opts.merges && !merge || opts.no_merges && merge {
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
    use grep::searcher::{BinaryDetection, SearcherBuilder, sinks::Lossy};
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
            let mut hits = Vec::new();
            let _ = SearcherBuilder::new()
                .line_number(true)
                .invert_match(q.invert)
                .binary_detection(BinaryDetection::none())
                .build()
                .search_slice(
                    &matcher,
                    data,
                    Lossy(|line, text| {
                        hits.push(GrepHit {
                            path: path.clone(),
                            line,
                            text: text.trim_end_matches(['\n', '\r']).to_owned(),
                            binary: false,
                        });
                        Ok(true)
                    }),
                );
            if !hits.is_empty() && data[..data.len().min(8000)].contains(&0) {
                hits = vec![GrepHit {
                    path: path.clone(),
                    line: 0,
                    text: String::new(),
                    binary: true,
                }];
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
    let when = git2::Signature::now(&name, &email)?.when();
    let offset = when.offset_minutes();
    let sign = if offset < 0 { '-' } else { '+' };
    Ok(format!(
        "{name} <{email}> {} {sign}{:02}{:02}",
        when.seconds(),
        offset.abs() / 60,
        offset.abs() % 60
    ))
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
