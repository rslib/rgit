//! git's read-only plumbing commands, done natively. The text is git's own
//! output format, byte for byte where scripts depend on it; the data is what the
//! agent modes print. `raw` is set for human text output: binary content goes
//! straight to stdout and "no" answers exit 1 silently, as in git.

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::Arc;

use rgit_git::{GitBackend, GitGrep, GrepSyntax, Ident, PathState, RefDetail, RevWalk, TreeWalk};

use crate::cli::{CliError, Plumbing};
use crate::output::Output;
use crate::toon::Obj;

/// Git's output as the text; one line reads as `result`, more as `lines`.
fn lines(text: String) -> Output {
    let mut out = Output::message(text.trim_end_matches('\n').to_owned());
    out.text = text;
    out
}

/// A table of `rows` with git's output as the text.
fn table(
    text: String,
    key: &str,
    rows: Vec<Obj>,
    cols: &'static [&'static str],
    empty: &str,
) -> Output {
    let mut out = Output::new(String::new()).list(key, rows, cols, empty);
    out.text = text;
    out
}

/// Exit 1; silently for human output when `quiet`, as git does.
fn fail(raw: bool, quiet: bool, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(CliError {
        message: if raw && quiet {
            String::new()
        } else {
            message.into()
        },
        help: None,
        code: 1,
    })
}

fn terminated(items: impl IntoIterator<Item = String>, z: bool) -> String {
    let end = if z { '\0' } else { '\n' };
    items.into_iter().map(|s| format!("{s}{end}")).collect()
}

/// A full ref name as git shortens it (`refs/heads/main` -> `main`).
fn shorten(name: &str) -> String {
    if let Some(rest) = name.strip_prefix("refs/remotes/") {
        return rest.strip_suffix("/HEAD").unwrap_or(rest).to_owned();
    }
    ["refs/heads/", "refs/tags/", "refs/"]
        .iter()
        .find_map(|p| name.strip_prefix(p))
        .unwrap_or(name)
        .to_owned()
}

pub fn run(backend: &Arc<dyn GitBackend>, command: Plumbing, raw: bool) -> anyhow::Result<Output> {
    Ok(match command {
        Plumbing::RevParse {
            short,
            abbrev_ref,
            symbolic_full_name,
            verify,
            quiet,
            show_toplevel,
            git_dir,
            absolute_git_dir,
            show_prefix,
            show_cdup,
            inside_work_tree,
            inside_git_dir,
            bare,
            revs,
        } => {
            let top = backend.workdir().canonicalize()?;
            let prefix = std::env::current_dir()?
                .canonicalize()
                .ok()
                .and_then(|cwd| cwd.strip_prefix(&top).ok().map(|p| p.to_path_buf()))
                .unwrap_or_default();
            let mut out: Vec<String> = Vec::new();
            if show_toplevel {
                out.push(top.display().to_string());
            }
            if git_dir || absolute_git_dir {
                let dir = backend.git_dir();
                let dir = dir.canonicalize().unwrap_or(dir);
                out.push(
                    if !absolute_git_dir && prefix.as_os_str().is_empty() && dir == top.join(".git")
                    {
                        ".git".to_owned()
                    } else {
                        dir.display().to_string()
                    },
                );
            }
            if inside_git_dir {
                out.push("false".to_owned());
            }
            if inside_work_tree {
                out.push("true".to_owned());
            }
            if bare {
                out.push("false".to_owned());
            }
            if show_prefix {
                out.push(if prefix.as_os_str().is_empty() {
                    String::new()
                } else {
                    format!("{}/", prefix.display())
                });
            }
            if show_cdup {
                out.push("../".repeat(prefix.components().count()));
            }
            let id = |rev: &str| -> anyhow::Result<String> {
                let id = backend.resolve_object(rev)?;
                Ok(match short {
                    Some(n) => backend.abbrev_id(&id, n)?,
                    None => id,
                })
            };
            if verify || short.is_some() && !abbrev_ref && !symbolic_full_name {
                let [rev] = revs.as_slice() else {
                    return Err(fail(raw, quiet, "needed a single revision"));
                };
                match id(rev) {
                    Ok(id) => out.push(id),
                    Err(_) => return Err(fail(raw, quiet, "needed a single revision")),
                }
            } else {
                for rev in &revs {
                    if abbrev_ref || symbolic_full_name {
                        if let Some(name) = backend.full_ref_name(rev)? {
                            out.push(if abbrev_ref { shorten(&name) } else { name });
                        }
                    } else if let Some((a, b)) = rev.split_once("...") {
                        let or_head = |s: &str| {
                            if s.is_empty() {
                                "HEAD".to_owned()
                            } else {
                                s.to_owned()
                            }
                        };
                        let (a, b) = (or_head(a), or_head(b));
                        out.push(id(&b)?);
                        out.push(id(&a)?);
                        for base in backend.merge_bases(&a, &b, true)? {
                            out.push(format!("^{base}"));
                        }
                    } else if let Some((a, b)) = rev.split_once("..") {
                        let or_head = |s: &str| {
                            if s.is_empty() {
                                "HEAD".to_owned()
                            } else {
                                s.to_owned()
                            }
                        };
                        out.push(id(&or_head(b))?);
                        out.push(format!("^{}", id(&or_head(a))?));
                    } else if let Some(rev) = rev.strip_prefix('^') {
                        out.push(format!("^{}", id(rev)?));
                    } else {
                        out.push(id(rev)?);
                    }
                }
            }
            lines(terminated(out, false))
        }
        Plumbing::LsFiles {
            cached,
            stage,
            others,
            ignored,
            exclude_standard,
            modified,
            deleted,
            unmerged,
            z,
            full_name: _,
            paths,
        } => {
            let keep = |p: &str| paths.is_empty() || rgit_git::pathspec_matches(&paths, p);
            let show_cached = cached || stage || !(others || modified || deleted || unmerged);
            let states = if others || modified || deleted {
                backend.path_states(others && (ignored || !exclude_standard))?
            } else {
                Vec::new()
            };
            let mut rows: Vec<Obj> = Vec::new();
            let mut out: Vec<String> = Vec::new();
            let mut push = |row: Obj, line: String| {
                rows.push(row);
                out.push(line);
            };
            if others {
                for (path, state) in &states {
                    let want = match state {
                        PathState::Ignored => ignored || !exclude_standard,
                        PathState::Untracked => !ignored,
                        _ => false,
                    };
                    if want && keep(path) {
                        push(
                            crate::obj! { "path" => path, "state" => if *state == PathState::Ignored { "ignored" } else { "untracked" } },
                            path.clone(),
                        );
                    }
                }
            }
            let index = if show_cached || modified || deleted {
                backend.index_entries()?
            } else {
                Vec::new()
            };
            if show_cached || unmerged {
                for e in &index {
                    if !keep(&e.path) || unmerged && e.stage == 0 {
                        continue;
                    }
                    let line = if stage || unmerged {
                        format!("{:06o} {} {}\t{}", e.mode, e.id, e.stage, e.path)
                    } else {
                        e.path.clone()
                    };
                    push(
                        crate::obj! { "path" => e.path, "mode" => format!("{:06o}", e.mode), "object" => e.id, "stage" => e.stage as usize },
                        line,
                    );
                }
            }
            if modified || deleted {
                let changed: std::collections::HashMap<&str, PathState> =
                    states.iter().map(|(p, s)| (p.as_str(), *s)).collect();
                let mut last = "";
                for e in &index {
                    if e.path == last || !keep(&e.path) {
                        continue;
                    }
                    last = &e.path;
                    let state = changed.get(e.path.as_str()).copied();
                    if deleted && state == Some(PathState::Deleted) {
                        push(
                            crate::obj! { "path" => e.path, "state" => "deleted" },
                            e.path.clone(),
                        );
                    }
                    if modified && matches!(state, Some(PathState::Modified | PathState::Deleted)) {
                        push(
                            crate::obj! { "path" => e.path, "state" => "modified" },
                            e.path.clone(),
                        );
                    }
                }
            }
            let cols: &'static [&'static str] = if stage || unmerged {
                &["mode", "object", "stage", "path"]
            } else {
                &["path"]
            };
            table(terminated(out, z), "files", rows, cols, "0 files")
        }
        Plumbing::LsTree {
            recursive,
            only_trees,
            show_trees,
            long,
            name_only,
            object_only,
            abbrev,
            z,
            rev,
            paths,
            ..
        } => {
            let walk = TreeWalk {
                recursive,
                show_trees,
                only_trees,
                sizes: long,
            };
            let items = backend.ls_tree(&rev, &paths, walk)?;
            let mut out = Vec::new();
            let mut rows = Vec::new();
            for i in &items {
                let id = match abbrev {
                    Some(n) => backend.abbrev_id(&i.id, n)?,
                    None => i.id.clone(),
                };
                let size = i.size.map_or("-".to_owned(), |s| s.to_string());
                out.push(if name_only {
                    i.path.clone()
                } else if object_only {
                    id.clone()
                } else if long {
                    format!("{:06o} {} {id} {size:>7}\t{}", i.mode, i.kind, i.path)
                } else {
                    format!("{:06o} {} {id}\t{}", i.mode, i.kind, i.path)
                });
                rows.push(crate::obj! {
                    "mode" => format!("{:06o}", i.mode),
                    "type" => i.kind,
                    "object" => id,
                    "size" => size,
                    "path" => i.path,
                });
            }
            let cols: &'static [&'static str] = if long {
                &["mode", "type", "object", "size", "path"]
            } else {
                &["mode", "type", "object", "path"]
            };
            table(
                terminated(out, z),
                "entries",
                rows,
                cols,
                &format!("0 entries in {rev}"),
            )
        }
        Plumbing::CatFile {
            kind,
            size,
            pretty,
            exists,
            args,
        } => {
            let (ty, spec) = match args.as_slice() {
                [spec] => (None, spec.clone()),
                [ty, spec] => (Some(ty.clone()), spec.clone()),
                _ => unreachable!("clap takes one or two"),
            };
            if exists {
                return match backend.resolve_object(&spec) {
                    Ok(_) => Ok(Output::new(String::new()).with("exists", true)),
                    Err(_) if raw => Err(fail(raw, true, "")),
                    Err(_) => Ok(Output::new(String::new()).with("exists", false)),
                };
            }
            if kind || size {
                let obj = backend.read_object(&spec)?;
                return Ok(if kind {
                    lines(format!("{}\n", obj.kind)).with("type", obj.kind)
                } else {
                    lines(format!("{}\n", obj.data.len())).with("size", obj.data.len())
                });
            }
            if !pretty && ty.is_none() {
                return Err(CliError::usage("cat-file needs -t, -s, -e, -p or a type"));
            }
            let mut obj = backend.read_object(&spec)?;
            if let Some(t) = ty.as_deref().filter(|t| *t != obj.kind) {
                obj = backend.read_object(&format!("{}^{{{t}}}", obj.id))?;
            }
            if pretty && obj.kind == "tree" {
                let items = backend.ls_tree(&obj.id, &[], TreeWalk::default())?;
                let text: String = items
                    .iter()
                    .map(|i| format!("{:06o} {} {}\t{}\n", i.mode, i.kind, i.id, i.path))
                    .collect();
                return Ok(Output::new(text.clone())
                    .with("type", "tree")
                    .with("id", obj.id)
                    .long("content", text));
            }
            if raw {
                let mut stdout = std::io::stdout();
                stdout.write_all(&obj.data)?;
                stdout.flush()?;
                return Ok(Output::new(String::new()));
            }
            let text = String::from_utf8_lossy(&obj.data).into_owned();
            Output::new(text.clone())
                .with("type", obj.kind)
                .with("id", obj.id)
                .with("size", obj.data.len())
                .long("content", text)
        }
        Plumbing::ShowRef {
            heads,
            tags,
            verify,
            dereference,
            hash,
            head,
            quiet,
            patterns,
        } => {
            let refs = backend.ref_details()?;
            let mut found: Vec<(String, String)> = Vec::new();
            if head && let Ok(id) = backend.resolve_object("HEAD") {
                found.push((id, "HEAD".to_owned()));
            }
            let add = |found: &mut Vec<(String, String)>, r: &RefDetail| {
                found.push((r.id.clone(), r.name.clone()));
                if dereference && let Some(p) = &r.peeled {
                    found.push((p.clone(), format!("{}^{{}}", r.name)));
                }
            };
            if verify {
                for p in &patterns {
                    match refs.iter().find(|r| r.name == *p) {
                        Some(r) => add(&mut found, r),
                        None if p == "HEAD" && !head => {
                            found.push((backend.resolve_object("HEAD")?, "HEAD".to_owned()))
                        }
                        None => return Err(fail(raw, quiet, format!("'{p}' - not a valid ref"))),
                    }
                }
            } else {
                for r in &refs {
                    let kind_ok = !(heads || tags)
                        || heads && r.name.starts_with("refs/heads/")
                        || tags && r.name.starts_with("refs/tags/");
                    let name_ok = patterns.is_empty()
                        || patterns
                            .iter()
                            .any(|p| r.name == *p || r.name.ends_with(&format!("/{p}")));
                    if kind_ok && name_ok {
                        add(&mut found, r);
                    }
                }
            }
            if found.is_empty() && raw {
                return Err(fail(raw, true, ""));
            }
            let mut out = Vec::new();
            let mut rows = Vec::new();
            for (id, name) in &found {
                let id = match hash {
                    Some(0) | None => id.clone(),
                    Some(n) => backend.abbrev_id(id, n)?,
                };
                out.push(if hash.is_some() {
                    id.clone()
                } else {
                    format!("{id} {name}")
                });
                rows.push(crate::obj! { "object" => id, "ref" => name });
            }
            let text = if quiet {
                String::new()
            } else {
                terminated(out, false)
            };
            table(text, "refs", rows, &["object", "ref"], "0 matching refs")
        }
        Plumbing::ForEachRef {
            format,
            sort,
            count,
            patterns,
        } => {
            let head = backend.symbolic_ref("HEAD").ok().flatten();
            let mut refs: Vec<RefDetail> = backend
                .ref_details()?
                .into_iter()
                .filter(|r| patterns.is_empty() || patterns.iter().any(|p| ref_matches(p, &r.name)))
                .collect();
            for key in &sort {
                let (desc, key) = match key.strip_prefix('-') {
                    Some(k) => (true, k),
                    None => (false, key.as_str()),
                };
                let mut keyed = Vec::new();
                for r in refs {
                    keyed.push((sort_key(&r, key, head.as_deref())?, r));
                }
                keyed.sort_by(|a, b| if desc { b.0.cmp(&a.0) } else { a.0.cmp(&b.0) });
                refs = keyed.into_iter().map(|(_, r)| r).collect();
            }
            if let Some(n) = count {
                refs.truncate(n);
            }
            let fmt = format
                .clone()
                .unwrap_or_else(|| "%(objectname) %(objecttype)\t%(refname)".to_owned());
            let mut out = Vec::new();
            for r in &refs {
                out.push(expand(&fmt, r, head.as_deref())?);
            }
            let text = terminated(out, false);
            if format.is_some() {
                lines(text)
            } else {
                let rows = refs
                    .iter()
                    .map(|r| crate::obj! { "refname" => r.name, "objectname" => r.id, "objecttype" => r.kind })
                    .collect();
                table(
                    text,
                    "refs",
                    rows,
                    &["refname", "objectname", "objecttype"],
                    "0 matching refs",
                )
            }
        }
        Plumbing::RevList {
            max_count,
            count,
            all,
            reverse,
            first_parent,
            merges,
            no_merges,
            parents,
            revs,
        } => {
            if revs.is_empty() && !all {
                return Err(CliError::usage("rev-list needs a revision, e.g. HEAD"));
            }
            let mut commits = backend.rev_walk(&RevWalk {
                revs,
                all,
                first_parent,
                merges,
                no_merges,
                max: max_count,
            })?;
            if count {
                return Ok(lines(format!("{}\n", commits.len())).with("count", commits.len()));
            }
            if reverse {
                commits.reverse();
            }
            lines(terminated(
                commits.iter().map(|c| {
                    if parents && !c.parents.is_empty() {
                        format!("{} {}", c.id, c.parents.join(" "))
                    } else {
                        c.id.clone()
                    }
                }),
                false,
            ))
        }
        Plumbing::MergeBase {
            all,
            is_ancestor,
            a,
            b,
        } => {
            if is_ancestor {
                let yes = backend.is_ancestor(&a, &b)?;
                if !yes && raw {
                    return Err(fail(raw, true, ""));
                }
                return Ok(Output::new(String::new()).with("ancestor", yes));
            }
            let bases = backend.merge_bases(&a, &b, all)?;
            if bases.is_empty() {
                return Err(fail(
                    raw,
                    true,
                    format!("{a} and {b} have no common ancestor"),
                ));
            }
            lines(terminated(bases, false))
        }
        Plumbing::Reflog { max_count, args } => {
            let args: Vec<&String> = args.iter().skip_while(|a| *a == "show").collect();
            let name = match args.as_slice() {
                [] => "HEAD",
                [name] => name.as_str(),
                _ => {
                    return Err(CliError::usage(
                        "rgit reflog only shows reflogs: rgit reflog [show] [REF]",
                    ));
                }
            };
            let mut items = backend.reflog(name)?;
            items.truncate(max_count.unwrap_or(usize::MAX));
            let mut out = Vec::new();
            let mut rows = Vec::new();
            for (i, item) in items.iter().enumerate() {
                let id = backend.abbrev_id(&item.id, 7)?;
                let selector = format!("{name}@{{{i}}}");
                out.push(format!("{id} {selector}: {}", item.message));
                rows.push(
                    crate::obj! { "id" => id, "selector" => selector, "message" => item.message },
                );
            }
            table(
                terminated(out, false),
                "entries",
                rows,
                &["id", "selector", "message"],
                &format!("0 reflog entries for {name}"),
            )
        }
        Plumbing::Shortlog {
            summary,
            numbered,
            email,
            committer,
            all,
            mut revs,
        } => {
            if revs.is_empty() && !all {
                revs.push("HEAD".to_owned());
            }
            let commits = backend.rev_walk(&RevWalk {
                revs,
                all,
                ..RevWalk::default()
            })?;
            let mut by: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for c in commits.iter().rev() {
                let who = if committer { &c.committer } else { &c.author };
                let key = if email {
                    format!("{} <{}>", who.name, who.email)
                } else {
                    who.name.clone()
                };
                by.entry(key).or_default().push(c.summary.clone());
            }
            let mut groups: Vec<(String, Vec<String>)> = by.into_iter().collect();
            if numbered {
                groups.sort_by_key(|g| std::cmp::Reverse(g.1.len()));
            }
            let mut text = String::new();
            for (who, subjects) in &groups {
                if summary {
                    text.push_str(&format!("{:>6}\t{who}\n", subjects.len()));
                } else {
                    text.push_str(&format!("{who} ({}):\n", subjects.len()));
                    for s in subjects {
                        text.push_str(&format!("      {s}\n"));
                    }
                    text.push('\n');
                }
            }
            let rows = groups
                .iter()
                .map(|(who, subjects)| crate::obj! { "author" => who, "count" => subjects.len(), "subjects" => subjects.clone() })
                .collect();
            table(text, "authors", rows, &["author", "count"], "0 commits")
        }
        Plumbing::Grep {
            ignore_case,
            word,
            invert,
            line_number,
            files,
            count,
            quiet,
            fixed,
            extended,
            perl,
            mut patterns,
            cached,
            mut args,
            mut paths,
        } => {
            if patterns.is_empty() {
                if args.is_empty() {
                    return Err(CliError::usage("grep needs a pattern"));
                }
                patterns.push(args.remove(0));
            }
            let mut revs = Vec::new();
            for a in args {
                if backend.resolve_object(&a).is_ok() {
                    revs.push(a);
                } else {
                    paths.push(a);
                }
            }
            let syntax = if fixed {
                GrepSyntax::Fixed
            } else if extended || perl {
                GrepSyntax::Extended
            } else {
                GrepSyntax::Basic
            };
            let sources: Vec<Option<String>> = if revs.is_empty() {
                vec![None]
            } else {
                revs.into_iter().map(Some).collect()
            };
            let mut out = Vec::new();
            let mut rows = Vec::new();
            for rev in sources {
                let prefix = rev.as_ref().map(|r| format!("{r}:")).unwrap_or_default();
                let hits = backend.git_grep(&GitGrep {
                    patterns: patterns.clone(),
                    syntax,
                    ignore_case,
                    word,
                    invert,
                    cached,
                    rev: rev.clone(),
                    paths: paths.clone(),
                })?;
                let mut per_file: Vec<(String, usize)> = Vec::new();
                for h in &hits {
                    let path = format!("{prefix}{}", h.path);
                    match per_file.last_mut() {
                        Some((p, n)) if *p == path => *n += 1,
                        _ => per_file.push((path.clone(), 1)),
                    }
                    if !files && !count {
                        out.push(if h.binary {
                            format!("Binary file {path} matches")
                        } else if line_number {
                            format!("{path}:{}:{}", h.line, h.text)
                        } else {
                            format!("{path}:{}", h.text)
                        });
                    }
                    let mut row = crate::obj! { "path" => h.path, "line" => h.line as usize, "text" => h.text };
                    if let Some(r) = &rev {
                        row.push(("rev".to_owned(), r.as_str().into()));
                    }
                    rows.push(row);
                }
                if files {
                    out.extend(per_file.iter().map(|(p, _)| p.clone()));
                } else if count {
                    out.extend(per_file.iter().map(|(p, n)| format!("{p}:{n}")));
                }
            }
            if rows.is_empty() && raw {
                return Err(fail(raw, true, ""));
            }
            let text = if quiet {
                String::new()
            } else {
                terminated(out, false)
            };
            table(
                text,
                "matches",
                rows,
                &["path", "line", "text"],
                &format!("0 matches for {:?}", patterns.join("|")),
            )
        }
        Plumbing::CheckIgnore {
            verbose,
            quiet,
            non_matching,
            no_index,
            paths,
        } => {
            let mut out = Vec::new();
            let mut rows = Vec::new();
            let mut any = false;
            for p in &paths {
                let rule = backend.check_ignore(p, no_index)?;
                any |= rule.is_some();
                match &rule {
                    Some(r) if verbose => {
                        out.push(format!("{}:{}:{}\t{p}", r.source, r.line, r.pattern))
                    }
                    Some(_) => out.push(p.clone()),
                    None if verbose && non_matching => out.push(format!("::\t{p}")),
                    None => {}
                }
                if let Some(r) = rule {
                    rows.push(crate::obj! { "path" => p, "source" => r.source, "line" => r.line, "pattern" => r.pattern });
                }
            }
            if !any && raw {
                return Err(fail(raw, true, ""));
            }
            let text = if quiet {
                String::new()
            } else {
                terminated(out, false)
            };
            table(
                text,
                "ignored",
                rows,
                &["path", "source", "line", "pattern"],
                "0 ignored paths",
            )
        }
        Plumbing::Var { name } => {
            let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
            let config = |k: &str| backend.config_get(k).ok().flatten();
            let editor = || {
                env("GIT_EDITOR")
                    .or_else(|| config("core.editor"))
                    .or_else(|| env("VISUAL"))
                    .or_else(|| env("EDITOR"))
                    .unwrap_or_else(|| "vi".to_owned())
            };
            let value = match name.as_str() {
                "GIT_AUTHOR_IDENT" => backend.ident(false)?,
                "GIT_COMMITTER_IDENT" => backend.ident(true)?,
                "GIT_EDITOR" => editor(),
                "GIT_SEQUENCE_EDITOR" => env("GIT_SEQUENCE_EDITOR")
                    .or_else(|| config("sequence.editor"))
                    .unwrap_or_else(editor),
                "GIT_PAGER" => env("GIT_PAGER")
                    .or_else(|| config("core.pager"))
                    .or_else(|| env("PAGER"))
                    .unwrap_or_else(|| "less".to_owned()),
                "GIT_DEFAULT_BRANCH" => {
                    config("init.defaultBranch").unwrap_or_else(|| "master".to_owned())
                }
                other => {
                    return Err(CliError::usage(format!(
                        "unknown variable {other}; use GIT_AUTHOR_IDENT, GIT_COMMITTER_IDENT, GIT_EDITOR, GIT_SEQUENCE_EDITOR, GIT_PAGER or GIT_DEFAULT_BRANCH"
                    )));
                }
            };
            lines(format!("{value}\n"))
        }
        Plumbing::SymbolicRef {
            short,
            quiet,
            name,
            target,
        } => {
            if target.is_some() {
                return Err(anyhow::Error::new(CliError {
                    message: "rgit symbolic-ref only reads symbolic refs".to_owned(),
                    help: Some("Run `rgit checkout <branch>` to move HEAD".to_owned()),
                    code: 2,
                }));
            }
            match backend.symbolic_ref(&name)? {
                Some(t) => lines(format!("{}\n", if short { shorten(&t) } else { t })),
                None => {
                    return Err(fail(
                        raw,
                        quiet,
                        format!("ref {name} is not a symbolic ref"),
                    ));
                }
            }
        }
        Plumbing::CountObjects { verbose } => {
            let c = backend.count_objects()?;
            lines(if verbose {
                format!(
                    "count: {}\nsize: {}\nin-pack: {}\npacks: {}\nsize-pack: {}\nprune-packable: {}\ngarbage: 0\nsize-garbage: 0\n",
                    c.count, c.size, c.in_pack, c.packs, c.size_pack, c.prune_packable
                )
            } else {
                format!("{} objects, {} kilobytes\n", c.count, c.size)
            })
        }
    })
}

/// A for-each-ref pattern: a glob, or a prefix ending at a `/`.
fn ref_matches(pattern: &str, name: &str) -> bool {
    if pattern.contains(['*', '?', '[']) {
        return glob(pattern.as_bytes(), name.as_bytes());
    }
    let p = pattern.trim_end_matches('/');
    name == p || name.starts_with(&format!("{p}/"))
}

/// fnmatch without FNM_PATHNAME: `*` and `?` also match `/`.
fn glob(p: &[u8], s: &[u8]) -> bool {
    match (p.first(), s.first()) {
        (None, None) => true,
        (Some(b'*'), _) => glob(&p[1..], s) || !s.is_empty() && glob(p, &s[1..]),
        (Some(b'?'), Some(_)) => glob(&p[1..], &s[1..]),
        (Some(a), Some(b)) if a == b => glob(&p[1..], &s[1..]),
        _ => false,
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Num(i64),
    Text(String),
}

fn date_of<'a>(r: &'a RefDetail, atom: &str) -> Option<Option<&'a Ident>> {
    Some(match atom {
        "authordate" => r.author.as_ref(),
        "committerdate" => r.committer.as_ref(),
        "taggerdate" => r.tagger.as_ref(),
        "creatordate" => r.tagger.as_ref().or(r.committer.as_ref()),
        _ => return None,
    })
}

fn sort_key(r: &RefDetail, key: &str, head: Option<&str>) -> anyhow::Result<Key> {
    let atom = key.split(':').next().unwrap_or(key);
    Ok(match date_of(r, atom) {
        Some(ident) => Key::Num(ident.map_or(0, |i| i.time)),
        None => Key::Text(atom_value(r, key, head)?),
    })
}

/// Expand `%(atom)`, `%%` and `%xx` in a for-each-ref format.
fn expand(fmt: &str, r: &RefDetail, head: Option<&str>) -> anyhow::Result<String> {
    let mut out = String::new();
    let mut rest = fmt;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        rest = &rest[i + 1..];
        if let Some(r2) = rest.strip_prefix('%') {
            out.push('%');
            rest = r2;
        } else if let Some(inner) = rest.strip_prefix('(') {
            let end = inner
                .find(')')
                .ok_or_else(|| CliError::usage(format!("unterminated %( in format {fmt:?}")))?;
            out.push_str(&atom_value(r, &inner[..end], head)?);
            rest = &inner[end + 1..];
        } else if let Some(byte) = rest.get(..2).and_then(|h| u8::from_str_radix(h, 16).ok()) {
            out.push(byte as char);
            rest = &rest[2..];
        } else {
            out.push('%');
        }
    }
    out.push_str(rest);
    Ok(out)
}

fn atom_value(r: &RefDetail, spec: &str, head: Option<&str>) -> anyhow::Result<String> {
    let (deref, spec) = match spec.strip_prefix('*') {
        Some(s) => (true, s),
        None => (false, spec),
    };
    let (atom, arg) = spec.split_once(':').unwrap_or((spec, ""));
    if deref {
        return Ok(match (atom, &r.peeled) {
            ("objectname", Some(p)) => p.clone(),
            ("objecttype", Some(_)) => "commit".to_owned(),
            _ => String::new(),
        });
    }
    let subject = || {
        r.message
            .split("\n\n")
            .next()
            .unwrap_or_default()
            .lines()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let body = || {
        r.message
            .split_once("\n\n")
            .map(|(_, b)| b.to_owned())
            .unwrap_or_default()
    };
    let who = |ident: Option<&Ident>, part: &str| -> String {
        let Some(i) = ident else { return String::new() };
        match part {
            "name" => i.name.clone(),
            "email" => match arg {
                "trim" => i.email.clone(),
                "localpart" => i.email.split('@').next().unwrap_or_default().to_owned(),
                _ => format!("<{}>", i.email),
            },
            "date" => format_date(i.time, i.offset, arg),
            _ => format!(
                "{} <{}> {}",
                i.name,
                i.email,
                format_date(i.time, i.offset, "raw")
            ),
        }
    };
    Ok(match atom {
        "refname" => match arg {
            "short" => shorten(&r.name),
            "" => r.name.clone(),
            _ => strip(&r.name, arg)?,
        },
        "objectname" => match arg {
            "" => r.id.clone(),
            "short" => r.id[..7].to_owned(),
            a => {
                let n: usize = a
                    .strip_prefix("short=")
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(7);
                r.id[..n.clamp(4, 40)].to_owned()
            }
        },
        "objecttype" => r.kind.to_owned(),
        "subject" | "contents:subject" => subject(),
        "body" | "contents:body" => body(),
        "contents" => r.message.clone(),
        "HEAD" => if head == Some(r.name.as_str()) {
            "*"
        } else {
            " "
        }
        .to_owned(),
        "symref" => r
            .symref
            .as_deref()
            .map(|s| {
                if arg == "short" {
                    shorten(s)
                } else {
                    s.to_owned()
                }
            })
            .unwrap_or_default(),
        "upstream" => r
            .upstream
            .as_deref()
            .map(|s| {
                if arg == "short" {
                    shorten(s)
                } else {
                    s.to_owned()
                }
            })
            .unwrap_or_default(),
        "color" | "align" | "end" => String::new(),
        _ => {
            for (role, ident) in [
                ("author", r.author.as_ref()),
                ("committer", r.committer.as_ref()),
                ("tagger", r.tagger.as_ref()),
                ("creator", r.tagger.as_ref().or(r.committer.as_ref())),
            ] {
                if let Some(part) = atom.strip_prefix(role)
                    && matches!(part, "" | "name" | "email" | "date")
                {
                    return Ok(who(ident, part));
                }
            }
            return Err(CliError::usage(format!("unknown field name: {atom}")));
        }
    })
}

/// `refname:lstrip=N` / `strip=N` / `rstrip=N`; negative N keeps N components.
fn strip(name: &str, arg: &str) -> anyhow::Result<String> {
    let bad = || CliError::usage(format!("unknown refname modifier {arg:?}"));
    let (how, n) = arg.split_once('=').ok_or_else(bad)?;
    let n: i64 = n.parse().map_err(|_| bad())?;
    let parts: Vec<&str> = name.split('/').collect();
    let len = parts.len() as i64;
    let drop = if n < 0 { (len + n).max(0) } else { n.min(len) } as usize;
    Ok(match how {
        "lstrip" | "strip" => parts[drop..].join("/"),
        "rstrip" => parts[..parts.len() - drop].join("/"),
        _ => return Err(bad()),
    })
}

/// A date as git prints it: `default`, `short`, `iso`, `iso-strict`, `rfc`,
/// `unix`, `raw` or `relative`.
fn format_date(time: i64, offset: i32, style: &str) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let sign = if offset < 0 { '-' } else { '+' };
    let (oh, om) = (offset.abs() / 60, offset.abs() % 60);
    let local = time + i64::from(offset) * 60;
    let days = local.div_euclid(86400);
    let secs = local.rem_euclid(86400);
    let (h, mi, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    // civil_from_days (Howard Hinnant, public domain).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let wd = DAYS[(days + 4).rem_euclid(7) as usize];
    let mon = MONTHS[(m - 1) as usize];
    match style {
        "short" => format!("{y:04}-{m:02}-{d:02}"),
        "iso" | "iso8601" => {
            format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02} {sign}{oh:02}{om:02}")
        }
        "iso-strict" | "iso8601-strict" => {
            format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}{sign}{oh:02}:{om:02}")
        }
        "rfc" | "rfc2822" => {
            format!("{wd}, {d} {mon} {y} {h:02}:{mi:02}:{s:02} {sign}{oh:02}{om:02}")
        }
        "unix" => time.to_string(),
        "raw" => format!("{time} {sign}{oh:02}{om:02}"),
        "relative" => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64);
            let ago = (now - time).max(0);
            let (n, unit) = match ago {
                a if a < 90 => (a, "second"),
                a if a < 90 * 60 => (a / 60, "minute"),
                a if a < 36 * 3600 => (a / 3600, "hour"),
                a if a < 14 * 86400 => (a / 86400, "day"),
                a if a < 70 * 86400 => (a / (7 * 86400), "week"),
                a if a < 365 * 86400 => (a / (30 * 86400), "month"),
                a => (a / (365 * 86400), "year"),
            };
            format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
        }
        _ => format!("{wd} {mon} {d} {h:02}:{mi:02}:{s:02} {y} {sign}{oh:02}{om:02}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_format_like_git() {
        // 2026-09-27 14:17:29 -0500
        let t = 1790536649;
        assert_eq!(format_date(t, -300, ""), "Sun Sep 27 14:17:29 2026 -0500");
        assert_eq!(format_date(t, -300, "iso"), "2026-09-27 14:17:29 -0500");
        assert_eq!(
            format_date(t, -300, "iso-strict"),
            "2026-09-27T14:17:29-05:00"
        );
        assert_eq!(format_date(0, 0, "short"), "1970-01-01");
    }

    #[test]
    fn refs_shorten_and_match() {
        assert_eq!(shorten("refs/heads/main"), "main");
        assert_eq!(shorten("refs/remotes/origin/HEAD"), "origin");
        assert_eq!(strip("refs/heads/a/b", "lstrip=2").unwrap(), "a/b");
        assert_eq!(strip("refs/heads/a/b", "lstrip=-1").unwrap(), "b");
        assert!(ref_matches("refs/heads", "refs/heads/main"));
        assert!(!ref_matches("refs/head", "refs/heads/main"));
        assert!(ref_matches("refs/*/ma*", "refs/heads/main"));
    }
}
