//! git's read-only plumbing commands, done natively. The text is git's own
//! output format, byte for byte where scripts depend on it; the data is what the
//! agent modes print. `raw` is set for human text output: binary content goes
//! straight to stdout and "no" answers exit 1 silently, as in git.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rgit_git::{GitBackend, GitGrep, GrepExpr, GrepSyntax, Ident, PathState, RefDetail, TreeWalk};

use crate::cli::{CliError, Plumbing, RawDiffArgs};
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

/// `git reflog expire`, `delete` and `exists`.
fn reflog_change(
    backend: &Arc<dyn GitBackend>,
    args: &[String],
    raw: bool,
) -> anyhow::Result<Output> {
    let mut o = rgit_git::ReflogExpire::default();
    let mut rest = args[1..].iter();
    let mut refs = Vec::new();
    while let Some(a) = rest.next() {
        let mut value = |name: &str| -> anyhow::Result<Option<String>> {
            if let Some(v) = a.strip_prefix(&format!("{name}=")) {
                return Ok(Some(v.to_owned()));
            }
            if a == name {
                return Ok(Some(rest.next().cloned().ok_or_else(|| {
                    CliError::usage(format!("option '{}' requires a value", &name[2..]))
                })?));
            }
            Ok(None)
        };
        if let Some(v) = value("--expire-unreachable")? {
            o.expire_unreachable = Some(v);
        } else if let Some(v) = value("--expire")? {
            o.expire = Some(v);
        } else {
            match a.as_str() {
                "--all" => o.all = true,
                "--single-worktree" => o.single_worktree = true,
                "--rewrite" => o.rewrite = true,
                "--updateref" => o.updateref = true,
                "--stale-fix" => o.stale_fix = true,
                "-n" | "--dry-run" => o.dry_run = true,
                "--verbose" => o.verbose = true,
                "--" => refs.extend(rest.by_ref().cloned()),
                f if f.starts_with('-') => {
                    return Err(CliError::usage(format!("unknown option '{f}'")));
                }
                r => refs.push(r.to_owned()),
            }
        }
    }
    match args[0].as_str() {
        "exists" => {
            let [name] = refs.as_slice() else {
                return Err(CliError::usage("reflog exists takes one ref"));
            };
            if backend.reflog_exists(name) {
                Ok(Output::new(String::new()).with("exists", true))
            } else if raw {
                Err(fail(raw, true, ""))
            } else {
                Ok(Output::new(String::new()).with("exists", false))
            }
        }
        "delete" => {
            if refs.is_empty() {
                return Err(CliError::usage("no reflog specified to delete"));
            }
            backend.reflog_delete(&refs, &o)?;
            Ok(Output::new(String::new()))
        }
        _ => {
            if !o.all && refs.is_empty() {
                return Err(CliError::usage("no reflog specified to expire"));
            }
            o.refs = refs;
            Ok(lines(terminated(backend.reflog_expire(&o)?, false)))
        }
    }
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

/// git's editor: GIT_EDITOR, core.editor, VISUAL, EDITOR, then vi.
pub(crate) fn editor(backend: &Arc<dyn GitBackend>) -> String {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    env("GIT_EDITOR")
        .or_else(|| backend.config_get("core.editor").ok().flatten())
        .or_else(|| env("VISUAL"))
        .or_else(|| env("EDITOR"))
        .unwrap_or_else(|| "vi".to_owned())
}

/// `git ls-remote`, from the repository at `dir` or from none. `raw` is set
/// for human output, which alone gets git's `From <url>` on stderr.
pub fn ls_remote(
    dir: Option<&std::path::Path>,
    command: crate::cli::Command,
    raw: bool,
) -> anyhow::Result<Output> {
    let crate::cli::Command::LsRemote {
        repository,
        patterns,
        heads,
        tags,
        refs,
        symref,
        quiet,
        exit_code,
        get_url,
    } = command
    else {
        unreachable!("ls-remote only")
    };
    let (url, list) = rgit_git::ls_remote(dir, repository.as_deref(), !get_url)?;
    if get_url {
        return Ok(lines(format!("{url}\n")));
    }
    if raw && !quiet && repository.is_none() {
        eprintln!("From {url}");
    }
    let tails: Vec<String> = patterns.iter().map(|p| format!("*/{p}")).collect();
    let mut text = String::new();
    let mut rows = Vec::new();
    for (name, id, target) in &list {
        let kind_ok = !(heads || tags)
            || heads && name.starts_with("refs/heads/")
            || tags && name.starts_with("refs/tags/");
        let name_ok = tails.is_empty()
            || tails
                .iter()
                .any(|t| glob(t.as_bytes(), format!("/{name}").as_bytes()));
        if !kind_ok || !name_ok || refs && (name.ends_with("^{}") || !name.starts_with("refs/")) {
            continue;
        }
        if symref && let Some(target) = target {
            text.push_str(&format!("ref: {target}\t{name}\n"));
        }
        text.push_str(&format!("{id}\t{name}\n"));
        rows.push(crate::obj! {
            "object" => id,
            "ref" => name,
            "symref" => target.as_deref().unwrap_or(""),
        });
    }
    if rows.is_empty() && exit_code {
        return Err(anyhow::Error::new(CliError {
            message: String::new(),
            help: None,
            code: 2,
        }));
    }
    let cols: &'static [&'static str] = if symref {
        &["object", "ref", "symref"]
    } else {
        &["object", "ref"]
    };
    Ok(table(text, "refs", rows, cols, "0 refs"))
}

pub fn run(backend: &Arc<dyn GitBackend>, command: Plumbing, raw: bool) -> anyhow::Result<Output> {
    Ok(match command {
        Plumbing::RevParse { args } => rev_parse(backend, args, raw)?,
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
            full_name,
            error_unmatch,
            tags,
            valid_tags,
            mut paths,
        } => {
            let tags = tags || valid_tags;
            let tag = |t: &str, lower: bool| match (tags, lower && valid_tags) {
                (false, _) => String::new(),
                (true, false) => format!("{t} "),
                (true, true) => format!("{} ", t.to_ascii_lowercase()),
            };
            if ignored && !others && !cached {
                return Err(CliError::usage(
                    "ls-files -i must be used with either -o or -c",
                ));
            }
            let (_, prefix) = top_and_prefix(backend)?;
            let typed = paths.clone();
            rgit_git::check_pathspecs(&paths)?;
            if (paths.is_empty() || rgit_git::only_excludes(&paths)) && !prefix.is_empty() {
                paths.push(prefix.clone());
            }
            let keep = |p: &str| paths.is_empty() || rgit_git::pathspec_matches(&paths, p);
            let name = |p: &str| {
                if full_name {
                    p.to_owned()
                } else {
                    relative(p, &prefix)
                }
            };
            let show_cached = cached || stage || !(others || modified || deleted || unmerged);
            let states = if others || modified || deleted {
                backend.path_states(others && (ignored || !exclude_standard))?
            } else {
                Vec::new()
            };
            let mut rows: Vec<Obj> = Vec::new();
            let mut out: Vec<String> = Vec::new();
            let mut shown: Vec<String> = Vec::new();
            let mut push = |row: Obj, path: &str, line: String| {
                rows.push(row);
                out.push(line);
                shown.push(path.to_owned());
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
                            path,
                            format!("{}{}", tag("?", false), name(path)),
                        );
                    }
                }
            }
            let index = if show_cached || modified || deleted || unmerged {
                backend.index_entries()?
            } else {
                Vec::new()
            };
            let changed: std::collections::HashMap<&str, PathState> =
                states.iter().map(|(p, s)| (p.as_str(), *s)).collect();
            let mut last = "";
            for e in &index {
                if !keep(&e.path)
                    || cached
                        && ignored
                        && backend
                            .check_ignore(&e.path, true)?
                            .is_none_or(|r| r.negated)
                {
                    continue;
                }
                if show_cached && !unmerged || unmerged && e.stage != 0 {
                    let t = match (e.stage, e.skip_worktree) {
                        (0, false) => "H",
                        (0, true) => "S",
                        _ => "M",
                    };
                    let t = tag(t, e.assume_unchanged);
                    let line = if stage || unmerged {
                        format!("{t}{:06o} {} {}\t{}", e.mode, e.id, e.stage, name(&e.path))
                    } else {
                        format!("{t}{}", name(&e.path))
                    };
                    push(
                        crate::obj! { "path" => e.path, "mode" => format!("{:06o}", e.mode), "object" => e.id, "stage" => e.stage as usize },
                        &e.path,
                        line,
                    );
                }
                if e.path == last {
                    continue;
                }
                last = &e.path;
                // A skip-worktree file is left out on purpose, not deleted.
                let state = changed
                    .get(e.path.as_str())
                    .copied()
                    .filter(|_| !e.skip_worktree);
                if deleted && state == Some(PathState::Deleted) {
                    push(
                        crate::obj! { "path" => e.path, "state" => "deleted" },
                        &e.path,
                        format!("{}{}", tag("R", e.assume_unchanged), name(&e.path)),
                    );
                }
                if modified && matches!(state, Some(PathState::Modified | PathState::Deleted)) {
                    push(
                        crate::obj! { "path" => e.path, "state" => "modified" },
                        &e.path,
                        format!("{}{}", tag("C", e.assume_unchanged), name(&e.path)),
                    );
                }
            }
            if error_unmatch {
                for spec in &typed {
                    let one = std::slice::from_ref(spec);
                    if !shown.iter().any(|p| rgit_git::pathspec_matches(one, p)) {
                        if raw {
                            print!("{}", terminated(out, z));
                        }
                        return Err(anyhow::Error::new(CliError {
                            message: format!(
                                "pathspec '{}' did not match any file(s) known to git",
                                relative(spec, &prefix)
                            ),
                            help: Some("Did you forget to 'git add'?".to_owned()),
                            code: 1,
                        }));
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
            full_name,
            full_tree,
            format,
            rev,
            mut paths,
        } => {
            let walk = TreeWalk {
                recursive,
                show_trees,
                only_trees,
                sizes: long
                    || format
                        .as_deref()
                        .is_some_and(|f| f.contains("%(objectsize")),
            };
            let prefix = if full_tree {
                String::new()
            } else {
                top_and_prefix(backend)?.1
            };
            if paths.is_empty() && !prefix.is_empty() {
                paths.push(prefix.clone());
            }
            let items = backend.ls_tree(&rev, &paths, walk)?;
            let mut out = Vec::new();
            let mut rows = Vec::new();
            for i in &items {
                let id = match abbrev {
                    Some(n) => backend.abbrev_id(&i.id, n)?,
                    None => i.id.clone(),
                };
                let size = i.size.map_or("-".to_owned(), |s| s.to_string());
                let path = if full_name {
                    i.path.clone()
                } else {
                    relative(&i.path, &prefix)
                };
                let mode = format!("{:06o}", i.mode);
                out.push(if let Some(f) = &format {
                    f.replace("%(objectmode)", &mode)
                        .replace("%(objecttype)", i.kind)
                        .replace("%(objectname)", &id)
                        .replace("%(objectsize:padded)", &format!("{size:>7}"))
                        .replace("%(objectsize)", &size)
                        .replace("%(path)", &path)
                } else if name_only {
                    path
                } else if object_only {
                    id.clone()
                } else if long {
                    format!("{mode} {} {id} {size:>7}\t{path}", i.kind)
                } else {
                    format!("{mode} {} {id}\t{path}", i.kind)
                });
                rows.push(crate::obj! {
                    "mode" => mode,
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
            batch,
            batch_check,
            batch_all_objects,
            batch_command,
            buffer,
            nul,
            nul_input,
            follow_symlinks,
            textconv,
            filters,
            path,
            args,
        } => {
            let convert = (textconv || filters).then_some(textconv);
            let mode = match (&batch, &batch_check, &batch_command) {
                (Some(f), _, _) => Some((f, BatchMode::Contents)),
                (_, Some(f), _) => Some((f, BatchMode::Info)),
                (_, _, Some(f)) => Some((f, BatchMode::Command)),
                _ => None,
            };
            if let Some((fmt, mode)) = mode {
                return cat_file_batch(
                    backend,
                    &CatBatch {
                        fmt,
                        mode,
                        all: batch_all_objects,
                        buffer,
                        input: if nul || nul_input { b'\0' } else { b'\n' },
                        output: if nul { b'\0' } else { b'\n' },
                        follow: follow_symlinks,
                        convert,
                    },
                    raw,
                );
            }
            if let Some(textconv) = convert {
                let [spec] = args.as_slice() else {
                    return Err(CliError::usage(
                        "cat-file --textconv/--filters takes one object",
                    ));
                };
                let (rev, file) = match (&path, spec.split_once(':')) {
                    (Some(p), _) => (None, p.clone()),
                    (None, Some((rev, file))) => (Some(rev), file.to_owned()),
                    (None, None) => {
                        return Err(fatal(format!(
                            "<object>:<path> required, only <object> '{spec}' given"
                        )));
                    }
                };
                let obj = backend
                    .read_object(spec)
                    .map_err(|e| fatal(e.to_string()))?;
                // Symlinks and trees print as they are, as in git.
                let link = rev.is_some_and(|rev| {
                    backend
                        .ls_tree(
                            if rev.is_empty() { "HEAD" } else { rev },
                            std::slice::from_ref(&file),
                            TreeWalk::default(),
                        )
                        .is_ok_and(|items| {
                            items.iter().any(|i| i.path == file && i.mode == 0o120000)
                        })
                });
                let data = if obj.kind == "blob" && !link {
                    backend.convert_blob(&file, &obj.data, textconv)?
                } else {
                    obj.data
                };
                if raw {
                    let mut stdout = std::io::stdout();
                    stdout.write_all(&data)?;
                    stdout.flush()?;
                    return Ok(Output::new(String::new()));
                }
                let text = String::from_utf8_lossy(&data).into_owned();
                return Ok(Output::new(text.clone())
                    .with("id", obj.id)
                    .long("content", text));
            }
            let (ty, spec) = match args.as_slice() {
                [spec] => (None, spec.clone()),
                [ty, spec] => (Some(ty.clone()), spec.clone()),
                _ => {
                    return Err(CliError::usage(
                        "cat-file needs an object, or --batch / --batch-check",
                    ));
                }
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
            abbrev,
            exists,
            head,
            quiet,
            patterns,
        } => {
            if exists {
                let [name] = patterns.as_slice() else {
                    return Err(CliError::usage("--exists requires exactly one reference"));
                };
                let found = name == "HEAD"
                    || backend.symbolic_ref(name).is_ok_and(|t| t.is_some())
                    || backend.full_ref_name(name).ok().flatten().as_ref() == Some(name);
                return if found {
                    Ok(Output::new(String::new()).with("exists", true))
                } else {
                    Err(anyhow::Error::new(CliError {
                        message: "reference does not exist".to_owned(),
                        help: None,
                        code: 2,
                    }))
                };
            }
            let abbrev = abbrev.or(hash.filter(|n| *n > 0));
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
                let id = match abbrev {
                    None => id.clone(),
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
            merged,
            no_merged,
            contains,
            no_contains,
            points_at,
            exclude,
            omit_empty,
            patterns,
        } => {
            let ctx = RefFormat::new(backend);
            let points_at = points_at
                .iter()
                .map(|p| backend.resolve_object(p))
                .collect::<Result<Vec<_>, _>>()?;
            // A ref's commit reaches `rev`, or with `into` is reached from it;
            // refs that are not commits match neither way.
            let reaches = |r: &RefDetail, rev: &str, into: bool| -> Option<bool> {
                let tip = r.peeled.as_deref().unwrap_or(&r.id);
                if into {
                    backend.is_ancestor(tip, rev).ok()
                } else {
                    backend.is_ancestor(rev, tip).ok()
                }
            };
            let mut refs: Vec<RefDetail> = backend
                .ref_details()?
                .into_iter()
                .filter(|r| patterns.is_empty() || patterns.iter().any(|p| ref_matches(p, &r.name)))
                .filter(|r| !exclude.iter().any(|p| ref_matches(p, &r.name)))
                .filter(|r| {
                    points_at.is_empty()
                        || points_at
                            .iter()
                            .any(|p| *p == r.id || r.peeled.as_ref() == Some(p))
                })
                .filter(|r| {
                    let any = |revs: &[String], into| {
                        revs.iter()
                            .map(|m| reaches(r, m, into))
                            .collect::<Option<Vec<_>>>()
                    };
                    let all_ok = |revs: &[String], into, want: bool| {
                        revs.is_empty()
                            || any(revs, into).is_some_and(|v| v.contains(&true) == want)
                    };
                    all_ok(&merged, true, true)
                        && all_ok(&no_merged, true, false)
                        && all_ok(&contains, false, true)
                        && all_ok(&no_contains, false, false)
                })
                .collect();
            refs = ctx.sort(refs, &sort, false)?;
            if let Some(n) = count {
                refs.truncate(n);
            }
            let fmt = parse_format(
                format
                    .as_deref()
                    .unwrap_or("%(objectname) %(objecttype)\t%(refname)"),
            )?;
            let mut out = Vec::new();
            for r in &refs {
                let line = ctx.render(&fmt, r)?;
                if !(omit_empty && line.is_empty()) {
                    out.push(line);
                }
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
        c @ Plumbing::RevList { .. } => rev_list(backend, c)?,
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
        Plumbing::Reflog { args, .. }
            if matches!(
                args.first().map(String::as_str),
                Some("expire" | "delete" | "exists")
            ) =>
        {
            reflog_change(backend, &args, raw)?
        }
        Plumbing::Reflog { max_count, args } => {
            let args: Vec<&String> = args.iter().skip_while(|a| *a == "show").collect();
            if args.len() > 1 || args.iter().any(|a| a.starts_with('-') || a.contains("@{")) {
                // `git reflog show` is `git log -g --abbrev-commit --pretty=oneline`.
                let mut argv: Vec<String> = ["rgit", "log", "-g", "--abbrev-commit"]
                    .map(str::to_owned)
                    .into();
                if !args.iter().any(|a| {
                    a.starts_with("--pretty") || a.starts_with("--format") || *a == "--oneline"
                }) {
                    argv.push("--pretty=oneline".to_owned());
                }
                if let Some(n) = max_count {
                    argv.push(format!("-n{n}"));
                }
                argv.extend(args.into_iter().map(|a| match a.strip_prefix('-') {
                    Some(n) if n.parse::<usize>().is_ok() => format!("-n{n}"),
                    _ => a.clone(),
                }));
                let cli = <crate::cli::Cli as clap::Parser>::try_parse_from(argv)
                    .map_err(|e| CliError::usage(e.to_string()))?;
                let Some(command) = cli.command else {
                    unreachable!("log parses to a command")
                };
                return Ok(lines(crate::cli::reflog_log(backend, &command)?));
            }
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
                let id = backend.abbrev_id(&item.id, 0)?;
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
            let commits = backend.rev_walk(&rgit_git::LogOptions {
                revs,
                all,
                limit: usize::MAX,
                ..Default::default()
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
        command @ Plumbing::Grep { .. } => grep(Some(backend), command, raw)?,
        Plumbing::CheckIgnore {
            verbose,
            quiet,
            non_matching,
            no_index,
            stdin,
            z,
            mut paths,
        } => {
            if non_matching && !verbose {
                return Err(CliError::usage(
                    "--non-matching is only valid with --verbose",
                ));
            }
            if z && !stdin {
                return Err(CliError::usage("-z only makes sense with --stdin"));
            }
            if !stdin && paths.is_empty() {
                return Err(CliError::usage("no path specified"));
            }
            // With --stdin each answer goes out before the next line is read,
            // so a caller can drive it as a coprocess.
            let input: Box<dyn Iterator<Item = std::io::Result<Vec<u8>>>> = if stdin {
                Box::new(std::io::BufRead::split(
                    std::io::stdin().lock(),
                    if z { b'\0' } else { b'\n' },
                ))
            } else {
                Box::new(
                    std::mem::take(&mut paths)
                        .into_iter()
                        .map(|p| Ok(p.into_bytes())),
                )
            };
            let stream = stdin && raw && !quiet;
            let mut stdout = std::io::stdout();
            let (top, prefix) = top_and_prefix(backend)?;
            let (sep, tab) = if z { ("\0", "\0") } else { (":", "\t") };
            let mut out = Vec::new();
            let mut rows = Vec::new();
            let mut any = false;
            for p in input {
                let p = String::from_utf8_lossy(&p?).into_owned();
                if p.is_empty() {
                    continue;
                }
                let full = crate::cli::repo_path(&top, std::path::Path::new(&prefix), &p);
                // Only -v shows (and counts) the `!pattern` that matched.
                let rule = backend
                    .check_ignore(&full, no_index)?
                    .filter(|r| verbose || !r.negated);
                any |= rule.is_some();
                let line = match &rule {
                    Some(r) if verbose => Some(format!(
                        "{}{sep}{}{sep}{}{tab}{p}",
                        r.source, r.line, r.pattern
                    )),
                    Some(_) => Some(p.clone()),
                    None if non_matching => Some(format!("{sep}{sep}{tab}{p}")),
                    None => None,
                };
                if let Some(r) = rule.filter(|r| !r.negated) {
                    rows.push(crate::obj! { "path" => full, "source" => r.source, "line" => r.line, "pattern" => r.pattern });
                }
                match line {
                    Some(line) if stream => {
                        write!(stdout, "{line}{}", if z { '\0' } else { '\n' })?;
                        stdout.flush()?;
                    }
                    Some(line) => out.push(line),
                    None => {}
                }
            }
            if !any && raw {
                return Err(fail(raw, true, ""));
            }
            let text = if quiet {
                String::new()
            } else {
                terminated(out, z)
            };
            table(
                text,
                "ignored",
                rows,
                &["path", "source", "line", "pattern"],
                "0 ignored paths",
            )
        }
        Plumbing::Var { list, name } => var(backend, list, name)?,
        Plumbing::SymbolicRef {
            short,
            quiet,
            delete,
            message,
            name,
            target,
        } => {
            if let Some(target) = target {
                if name == "HEAD" && !target.starts_with("refs/") {
                    return Err(fatal("Refusing to point HEAD outside of refs/"));
                }
                backend.set_symbolic_ref(&name, &target, message.as_deref())?;
                return Ok(Output::new(String::new()).with("result", format!("{name} -> {target}")));
            }
            if delete {
                if name == "HEAD" {
                    return Err(fatal("deleting 'HEAD' is not allowed"));
                }
                if backend.symbolic_ref(&name).ok().flatten().is_none() {
                    let message = format!("Cannot delete {name}, not a symbolic ref");
                    return Err(if quiet {
                        fail(raw, true, message)
                    } else {
                        fatal(message)
                    });
                }
                backend.update_ref(&name, None, None, true, message.as_deref())?;
                return Ok(Output::new(String::new()).with("result", format!("deleted {name}")));
            }
            match backend.symbolic_ref(&name).ok().flatten() {
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
        Plumbing::GetTarCommitId => get_tar_commit_id()?,
        Plumbing::FmtMergeMsg {
            message,
            log,
            no_log,
            into_name,
            file,
            input,
        } => {
            let input = match (input, file.as_deref()) {
                (Some(text), _) => text,
                (None, Some(f)) if f != "-" => std::fs::read_to_string(f)
                    .map_err(|e| fatal(format!("cannot open '{f}': {e}")))?,
                _ => String::from_utf8_lossy(&stdin_or(None)?).into_owned(),
            };
            let o = rgit_git::FmtMergeMsgOpts {
                message,
                log: if no_log { Some(0) } else { log },
                into_name,
            };
            lines(rgit_git::fmt_merge_msg(&backend.git_dir(), &input, &o)?)
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
        Plumbing::NameRev {
            name_only,
            tags,
            refs,
            exclude,
            all,
            annotate_stdin,
            no_undefined,
            always,
            peel_tag,
            revs,
            input,
        } => {
            if !all && !annotate_stdin && revs.is_empty() {
                return Err(CliError::usage(
                    "name-rev needs a commit, --all or --annotate-stdin",
                ));
            }
            let annotate = if annotate_stdin {
                Some(String::from_utf8_lossy(&stdin_or(input)?).into_owned())
            } else {
                None
            };
            let o = rgit_git::NameRevOpts {
                name_only,
                tags,
                refs,
                exclude,
                all,
                annotate_stdin: annotate,
                undefined: !no_undefined,
                always,
                peel_tag,
            };
            let (text, error) = rgit_git::name_rev(&backend.git_dir(), &o, &revs)?;
            if let Some(e) = error {
                if raw {
                    print!("{text}");
                    std::io::stdout().flush()?;
                }
                return Err(fatal(e));
            }
            lines(text)
        }
        Plumbing::CheckAttr {
            all,
            cached,
            stdin,
            z,
            items,
            paths,
        } => {
            let (attrs, mut paths) = if all {
                (Vec::new(), items.into_iter().chain(paths).collect())
            } else if !paths.is_empty() {
                (items, paths)
            } else if stdin {
                (items, Vec::new())
            } else {
                let mut it = items.into_iter();
                (it.next().into_iter().collect(), it.collect())
            };
            if attrs.is_empty() && !all {
                return Err(CliError::usage("check-attr needs an attribute or -a"));
            }
            if stdin {
                let input = String::from_utf8_lossy(&stdin_or(None)?).into_owned();
                let end = if z { '\0' } else { '\n' };
                paths.extend(
                    input
                        .split(end)
                        .filter(|p| !p.is_empty())
                        .map(str::to_owned),
                );
            }
            let (top, prefix) = top_and_prefix(backend)?;
            let full: Vec<String> = paths
                .iter()
                .map(|p| crate::cli::repo_path(&top, std::path::Path::new(&prefix), p))
                .collect();
            let found = rgit_git::check_attr(&backend.git_dir(), &attrs, &full, cached)?;
            let mut text = String::new();
            let mut rows = Vec::new();
            for (i, name, value) in found {
                let p = &paths[i];
                if z {
                    text.push_str(&format!("{p}\0{name}\0{value}\0"));
                } else {
                    text.push_str(&format!("{}: {name}: {value}\n", rgit_git::quote_path(p)));
                }
                rows.push(crate::obj! { "path" => p.as_str(), "attr" => name, "value" => value });
            }
            table(
                text,
                "attributes",
                rows,
                &["path", "attr", "value"],
                "0 attributes",
            )
        }
        Plumbing::DiffTree {
            format,
            root,
            no_commit_id,
            stdin,
            combined,
            dense_combined,
            mut args,
            mut paths,
        } => {
            let mut revs = Vec::new();
            if !stdin {
                while revs.len() < 2 && !args.is_empty() && backend.resolve_object(&args[0]).is_ok()
                {
                    revs.push(args.remove(0));
                }
                if revs.is_empty() {
                    return Err(match args.first() {
                        Some(a) => ambiguous(a),
                        None => CliError::usage(
                            "usage: git diff-tree [<options>] <tree-ish> [<tree-ish>] [<path>...]",
                        ),
                    });
                }
                on_disk(backend, &args)?;
            }
            args.append(&mut paths);
            let input = if stdin {
                let mut s = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
                Some(s)
            } else {
                None
            };
            let o = rgit_git::DiffTreeOpts {
                fmt: raw_diff_fmt(backend, &format, args, dense_combined)?,
                revs,
                root,
                no_commit_id,
                combined: (combined || dense_combined).then_some(dense_combined),
            };
            let got = rgit_git::diff_tree(&backend.git_dir(), &o, input.as_deref())?;
            raw_diff_done(&format, got)
        }
        Plumbing::DiffIndex {
            format,
            cached,
            mut args,
            mut paths,
        } => {
            let rev = args.remove(0);
            if backend.resolve_object(&rev).is_err() {
                return Err(ambiguous(&rev));
            }
            on_disk(backend, &args)?;
            args.append(&mut paths);
            let fmt = raw_diff_fmt(backend, &format, args, false)?;
            let got = rgit_git::diff_index(&backend.git_dir(), &rev, cached, &fmt)?;
            raw_diff_done(&format, got)
        }
        Plumbing::DiffFiles { format, paths } => {
            let fmt = raw_diff_fmt(backend, &format, paths, false)?;
            raw_diff_done(&format, rgit_git::diff_files(&backend.git_dir(), &fmt)?)
        }
        m @ Plumbing::MergeTree { .. } => merge_tree(backend, m, raw)?,
        Plumbing::FastExport { args } => {
            if raw {
                let mut out = std::io::BufWriter::new(std::io::stdout().lock());
                rgit_git::fast_export(&backend.git_dir(), &args, &mut out)
                    .map_err(|e| fatal(e.to_string()))?;
                return Ok(Output::new(String::new()));
            }
            let mut out = Vec::new();
            rgit_git::fast_export(&backend.git_dir(), &args, &mut out)
                .map_err(|e| fatal(e.to_string()))?;
            let text = String::from_utf8_lossy(&out).into_owned();
            Output::new(text.clone())
                .with("bytes", out.len())
                .long("stream", text)
        }
        Plumbing::FastImport { args } => {
            let quiet = args.iter().any(|a| a == "--quiet");
            let mut stdin = std::io::stdin().lock();
            let mut replies = Vec::new();
            let report = if raw {
                let mut out = std::io::stdout().lock();
                rgit_git::fast_import(&backend.git_dir(), &args, &mut stdin, &mut out)
            } else {
                rgit_git::fast_import(&backend.git_dir(), &args, &mut stdin, &mut replies)
            }
            .map_err(|e| fatal(e.to_string()))?;
            if raw && !quiet {
                eprint!("{}", report.stats);
            }
            crate::cli::set_exit(!report.ok);
            let text = String::from_utf8_lossy(&replies).into_owned();
            Output::new(text.clone())
                .with("ok", report.ok)
                .with("replies", text.lines().collect::<Vec<_>>())
                .long("stats", report.stats)
        }
        Plumbing::Replay {
            onto,
            advance,
            contained,
            revs,
            ref_action,
        } => {
            match (&onto, &advance) {
                (None, None) => {
                    return Err(CliError::usage("option --onto or --advance is mandatory"));
                }
                (Some(_), Some(_)) => {
                    return Err(fatal(
                        "options '--onto' and '--advance' cannot be used together",
                    ));
                }
                (None, Some(_)) if contained => {
                    return Err(fatal(
                        "options '--advance' and '--contained' cannot be used together",
                    ));
                }
                _ => {}
            }
            let ref_action = match ref_action.as_str() {
                "update" => rgit_git::RefAction::Update,
                "print" => rgit_git::RefAction::Print,
                other => return Err(CliError::usage(format!("invalid --ref-action: {other}"))),
            };
            let o = rgit_git::ReplayOpts {
                onto,
                advance,
                contained,
                revs,
                ref_action,
            };
            let (text, clean) =
                rgit_git::replay(&backend.git_dir(), &o).map_err(|e| fatal(e.to_string()))?;
            crate::cli::set_exit(!clean);
            let updates: Vec<String> = text.lines().map(str::to_owned).collect();
            lines(text).with("updates", updates).with("clean", clean)
        }
        m @ Plumbing::MergeFile { .. } => merge_file(Some(backend), m, raw)?,
        Plumbing::InterpretTrailers { args, input } => {
            interpret_trailers(Some(&backend.git_dir()), args, input)?
        }
        c @ Plumbing::ShowBranch { .. } => show_branch(backend, c, raw, true)?,
        Plumbing::FetchPack {
            all,
            stdin,
            depth,
            repository,
            mut refs,
            ..
        } => {
            if stdin {
                let mut input = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)?;
                refs.extend(input.lines().filter(|l| !l.is_empty()).map(str::to_owned));
            }
            let (got, absent) = backend.fetch_pack(&repository, &refs, all, depth)?;
            let text = terminated(got.iter().map(|(id, name)| format!("{id} {name}")), false);
            if !absent.is_empty() {
                print!("{text}");
                for name in &absent {
                    eprintln!("error: no such remote ref {name}");
                }
                return Err(fail(raw, true, ""));
            }
            let rows = got
                .iter()
                .map(|(id, name)| crate::obj! { "ref" => name, "id" => id })
                .collect();
            table(text, "refs", rows, &["ref", "id"], "0 refs fetched")
        }
        Plumbing::DiffPairs {
            z,
            patch,
            no_patch,
            stat,
            shortstat,
            numstat,
            name_only,
            name_status,
            unified,
        } => {
            if !z {
                return Err(anyhow::Error::new(CliError {
                    message: "usage: working without -z is not supported".to_owned(),
                    help: None,
                    code: 129,
                }));
            }
            let mut input = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut input)?;
            let render = |batch: &[rgit_git::RawPair], text: &mut String| -> anyhow::Result<()> {
                if batch.is_empty() {
                    return Ok(());
                }
                let files = batch
                    .iter()
                    .map(|p| backend.diff_pair(p, unified))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| fatal(e.to_string()))?;
                for (p, f) in batch.iter().zip(&files) {
                    let paths = if matches!(p.status, 'R' | 'C') {
                        format!("{}\0{}\0", p.old_path, p.new_path)
                    } else {
                        format!("{}\0", p.new_path)
                    };
                    if name_only {
                        text.push_str(&format!("{}\0", p.new_path));
                    } else if name_status {
                        let score = if p.score > 0 {
                            format!("{:03}", p.score)
                        } else {
                            String::new()
                        };
                        text.push_str(&format!("{}{score}\0{paths}", p.status));
                    } else if numstat {
                        let (add, del) = crate::axi::line_counts(f);
                        let counts = if f.binary {
                            "-\t-".to_owned()
                        } else {
                            format!("{add}\t{del}")
                        };
                        let sep = if matches!(p.status, 'R' | 'C') {
                            "\t\0"
                        } else {
                            "\t"
                        };
                        text.push_str(&format!("{counts}{sep}{paths}"));
                    }
                }
                if name_only || name_status || numstat || no_patch {
                    return Ok(());
                }
                if shortstat {
                    text.push_str(&crate::render::stat_summary(&files));
                    text.push('\n');
                    return Ok(());
                }
                if stat {
                    text.push_str(&crate::render::stat(&files, 0));
                    text.push('\n');
                    if !patch {
                        return Ok(());
                    }
                    text.push('\0');
                }
                text.push_str(&crate::render::patch(&files));
                Ok(())
            };
            let mut text = String::new();
            let mut batch = Vec::new();
            let mut fields = input
                .split(|b| *b == 0)
                .map(|f| String::from_utf8_lossy(f).into_owned())
                .peekable();
            while let Some(meta) = fields.next() {
                if meta.is_empty() {
                    // An empty record flushes the batch and is echoed.
                    if fields.peek().is_some() {
                        render(&batch, &mut text)?;
                        batch.clear();
                        text.push('\0');
                    }
                    continue;
                }
                let bad = || fatal(format!("unable to parse raw diff: {meta}"));
                let words: Vec<&str> = meta.strip_prefix(':').ok_or_else(bad)?.split(' ').collect();
                let [old_mode, new_mode, old_id, new_id, status] = words[..] else {
                    return Err(bad());
                };
                let mode = |m: &str| u32::from_str_radix(m, 8).map_err(|_| bad());
                let letter = status.chars().next().ok_or_else(bad)?;
                let path = fields.next().ok_or_else(bad)?;
                let new_path = if matches!(letter, 'R' | 'C') {
                    fields.next().ok_or_else(bad)?
                } else {
                    path.clone()
                };
                batch.push(rgit_git::RawPair {
                    old_mode: mode(old_mode)?,
                    new_mode: mode(new_mode)?,
                    old_id: old_id.to_owned(),
                    new_id: new_id.to_owned(),
                    status: letter,
                    score: status[1..].parse().unwrap_or(0),
                    old_path: path,
                    new_path,
                });
            }
            render(&batch, &mut text)?;
            lines(text)
        }
        Plumbing::SendPack {
            all,
            mirror,
            dry_run,
            force,
            atomic,
            verbose,
            repository,
            refs,
            ..
        } => {
            let args = rgit_git::PushArgs {
                all,
                mirror,
                dry_run,
                force,
                atomic,
                verbose,
                ..Default::default()
            };
            let lines_out = std::sync::Mutex::new(Vec::new());
            let pushed = backend.send_pack(&repository, &refs, &args, &|p| {
                if let rgit_git::OpProgress::Line(l) = p {
                    lines_out.lock().expect("lines").push(l);
                }
            });
            let report = lines_out.into_inner().expect("lines");
            match pushed {
                Ok(()) => {
                    for l in &report {
                        eprintln!("{l}");
                    }
                    Output::new(String::new()).with("report", report.join("\n"))
                }
                Err(rgit_git::GitError::PushFailed(text)) => {
                    // send-pack reports the refs without push's advice.
                    for l in text
                        .lines()
                        .take_while(|l| !l.starts_with("error: failed to push some refs"))
                    {
                        eprintln!("{l}");
                    }
                    return Err(fail(raw, true, text));
                }
                Err(e) => return Err(e.into()),
            }
        }
        command => index_plumbing(backend, command, raw)?,
    })
}

impl Plumbing {
    /// Whether the command also runs outside a repository, as in git.
    pub fn runs_without_repo(&self) -> bool {
        matches!(
            self,
            Plumbing::Stripspace { .. }
                | Plumbing::Column { .. }
                | Plumbing::CheckRefFormat { .. }
                | Plumbing::PatchId { .. }
                | Plumbing::Mailsplit { .. }
                | Plumbing::Mailinfo { .. }
        )
    }
}

/// `input`, or all of stdin (MCP passes the text; the CLI reads it).
fn stdin_or(input: Option<String>) -> anyhow::Result<Vec<u8>> {
    Ok(match input {
        Some(s) => s.into_bytes(),
        None => {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)?;
            buf
        }
    })
}

/// The commands that work with or without a repository; `backend` supplies
/// config when there is one.
pub(crate) fn repoless(
    backend: Option<&Arc<dyn GitBackend>>,
    command: Plumbing,
    raw: bool,
) -> anyhow::Result<Output> {
    let config = |key: &str| backend.and_then(|b| b.config_get(key).ok().flatten());
    let comment = || {
        config("core.commentChar")
            .or_else(|| config("core.commentString"))
            .filter(|c| c != "auto" && !c.is_empty())
            .unwrap_or_else(|| "#".to_owned())
    };
    let text = |b: Vec<u8>| lines(String::from_utf8_lossy(&b).into_owned());
    Ok(match command {
        Plumbing::Stripspace {
            strip_comments,
            comment_lines,
            input,
        } => {
            let input = stdin_or(input)?;
            text(if comment_lines {
                rgit_git::comment_lines(&input, &comment())
            } else {
                rgit_git::stripspace(&input, strip_comments.then(comment).as_deref())
            })
        }
        Plumbing::Column {
            command,
            mode,
            raw_mode,
            width,
            indent,
            nl,
            padding,
            input,
        } => {
            let mut opts = 0;
            let bad = |e: String| CliError::usage(e);
            if let Some(ui) = config("column.ui") {
                rgit_git::column_mode(&mut opts, &ui).map_err(bad)?;
            }
            if let Some(v) = command.and_then(|c| config(&format!("column.{c}"))) {
                rgit_git::column_mode(&mut opts, &v).map_err(bad)?;
            }
            if let Some(m) = mode {
                opts = opts & !0x30 | 0x10;
                rgit_git::column_mode(&mut opts, &m).map_err(bad)?;
            }
            if let Some(r) = raw_mode {
                opts = r;
            }
            rgit_git::column_finalize(
                &mut opts,
                std::io::IsTerminal::is_terminal(&std::io::stdout()),
            );
            let input = String::from_utf8_lossy(&stdin_or(input)?).into_owned();
            let items: Vec<String> = input.lines().map(str::to_owned).collect();
            let width = width.filter(|&w| w > 0).unwrap_or_else(|| {
                std::env::var("COLUMNS")
                    .ok()
                    .and_then(|c| c.parse::<usize>().ok())
                    .filter(|&c| c > 0)
                    .unwrap_or(80)
                    - 1
            });
            let o = rgit_git::ColumnOpts {
                width,
                indent: indent.as_deref().unwrap_or(""),
                nl: nl.as_deref().unwrap_or("\n"),
                padding,
            };
            let out = lines(rgit_git::columns(&items, opts, &o));
            if !out.text.is_empty() && !out.text.ends_with('\n') {
                crate::cli::print_as_is();
            }
            out
        }
        Plumbing::CheckRefFormat {
            normalize,
            allow_onelevel,
            no_allow_onelevel: _,
            refspec_pattern,
            branch,
            name,
        } => {
            if branch {
                let full = match name.strip_prefix("@{-").and_then(|n| n.strip_suffix('}')) {
                    Some("1") => backend
                        .and_then(|b| b.previous_checkout().ok())
                        .filter(|b| !b.is_empty()),
                    Some(_) => None,
                    None => Some(name.clone()),
                };
                return match full {
                    Some(b)
                        if !b.starts_with('-')
                            && b != "HEAD"
                            && rgit_git::check_ref_format(
                                &format!("refs/heads/{b}"),
                                false,
                                false,
                            ) =>
                    {
                        Ok(lines(format!("{b}\n")))
                    }
                    _ => Err(fatal(format!("'{name}' is not a valid branch name"))),
                };
            }
            let name = if normalize {
                rgit_git::collapse_slashes(&name)
            } else {
                name
            };
            if !rgit_git::check_ref_format(&name, allow_onelevel, refspec_pattern) {
                return Err(fail(raw, true, format!("'{name}' is not a valid ref name")));
            }
            if normalize {
                lines(format!("{name}\n"))
            } else {
                let mut out = Output::message(format!("{name} is valid"));
                out.text = String::new();
                out
            }
        }
        Plumbing::Mailsplit {
            dir,
            bare,
            start,
            prec,
            keep_cr,
            mboxrd,
            mboxes,
        } => {
            let sources: Vec<Option<PathBuf>> = if mboxes.is_empty() {
                vec![None]
            } else {
                mboxes
                    .iter()
                    .map(|m| (m != "-").then(|| PathBuf::from(m)))
                    .collect()
            };
            let stdin = if sources.iter().any(Option::is_none) {
                stdin_or(None)?
            } else {
                Vec::new()
            };
            let o = rgit_git::MailsplitOpts {
                dir: PathBuf::from(dir),
                start,
                prec: usize::from(prec),
                bare,
                keep_cr,
                mboxrd,
            };
            let n = rgit_git::mailsplit(&sources, &stdin, &o)
                .map_err(|e| fail(false, false, e.to_string()))?;
            lines(format!("{n}\n"))
        }
        Plumbing::Mailinfo {
            keep_subject,
            keep_non_patch,
            utf8: _,
            message_id,
            scissors,
            no_scissors,
            msg,
            patch,
            input,
        } => {
            let scissors = scissors
                || !no_scissors
                    && config("mailinfo.scissors")
                        .is_some_and(|v| matches!(v.as_str(), "true" | "yes" | "on" | "1"));
            let o = rgit_git::MailinfoOpts {
                keep_subject,
                keep_non_patch,
                message_id,
                scissors,
            };
            let (info, body, diff) = rgit_git::mailinfo(&stdin_or(input)?, &o);
            std::fs::write(&msg, body).map_err(|e| fatal(format!("could not open {msg}: {e}")))?;
            std::fs::write(&patch, diff)
                .map_err(|e| fatal(format!("could not open {patch}: {e}")))?;
            lines(info)
        }
        Plumbing::PatchId {
            stable,
            unstable,
            verbatim,
            input,
        } => {
            let flag = |k: &str| {
                config(k).is_some_and(|v| matches!(v.as_str(), "true" | "yes" | "on" | "1"))
            };
            let verbatim = verbatim || !unstable && !stable && flag("patchid.verbatim");
            let stable = verbatim || stable || !unstable && flag("patchid.stable");
            lines(rgit_git::patch_ids(&stdin_or(input)?, stable, verbatim))
        }
        _ => return Err(CliError::not_a_repo()),
    })
}

/// The object and index writers: commit-tree, write-tree, read-tree,
/// update-index, checkout-index, mktree and mktag.
fn index_plumbing(
    backend: &Arc<dyn GitBackend>,
    command: Plumbing,
    raw: bool,
) -> anyhow::Result<Output> {
    let git_dir = backend.git_dir();
    let git_dir = git_dir.as_path();
    let stdin = || -> std::io::Result<Vec<u8>> {
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)?;
        Ok(buf)
    };
    let fatal_of = |e: rgit_git::GitError| match e {
        rgit_git::GitError::Other(m) => fatal(m),
        e => e.into(),
    };
    // stderr as git prints it, stdout as the text; exit 1 when git does.
    let done = |r: rgit_git::Report| {
        if raw {
            eprint!("{}", r.err);
        }
        crate::cli::set_exit(r.failed);
        let mut out = lines(r.out);
        if !raw && !r.err.is_empty() {
            out = out.with("errors", r.err.trim_end().to_owned());
        }
        out
    };
    let (_, cwd) = top_and_prefix(backend)?;
    Ok(match command {
        Plumbing::CommitTree {
            parents,
            message,
            file,
            tree,
        } => {
            let mut msg = Vec::new();
            for m in &message {
                if !msg.is_empty() {
                    msg.push(b'\n');
                }
                msg.extend_from_slice(m.as_bytes());
                if !msg.ends_with(b"\n") {
                    msg.push(b'\n');
                }
            }
            for f in &file {
                if !msg.is_empty() {
                    msg.push(b'\n');
                }
                msg.extend(if f == "-" {
                    stdin()?
                } else {
                    std::fs::read(f).map_err(|e| fatal(format!("could not read {f}: {e}")))?
                });
            }
            if message.is_empty() && file.is_empty() {
                msg = stdin()?;
            }
            done(rgit_git::commit_tree(git_dir, &tree, &parents, &msg).map_err(fatal_of)?)
        }
        Plumbing::WriteTree { missing_ok, prefix } => lines(format!(
            "{}\n",
            rgit_git::write_tree(git_dir, missing_ok, prefix.as_deref()).map_err(fatal_of)?
        )),
        Plumbing::ReadTree {
            merge,
            reset,
            update,
            index_only,
            dry_run,
            aggressive,
            trivial,
            index_output,
            no_sparse_checkout: _,
            prefix,
            empty,
            verbose: _,
            trees,
        } => {
            if trees.is_empty() && !empty && !merge && !reset && prefix.is_none() && raw {
                eprintln!(
                    "warning: read-tree: emptying the index with no arguments is deprecated; use --empty"
                );
            }
            let opts = rgit_git::ReadTreeOpts {
                merge,
                reset,
                update,
                index_only,
                dry_run,
                aggressive,
                trivial,
                prefix,
                empty,
                index_output,
            };
            rgit_git::read_tree(git_dir, &trees, &opts).map_err(fatal_of)?;
            Output::new(String::new()).with(
                "result",
                if dry_run {
                    "merge checked"
                } else {
                    "index updated"
                },
            )
        }
        Plumbing::UpdateIndex { args } => {
            let mut input = stdin;
            done(rgit_git::update_index(git_dir, &cwd, &args, &mut input).map_err(fatal_of)?)
        }
        Plumbing::SparseCheckout { args } => {
            let mut input = stdin;
            let usage = |e: rgit_git::GitError| match e {
                rgit_git::GitError::Other(m) if m.contains("\nusage: ") => {
                    anyhow::Error::new(CliError {
                        message: m,
                        help: None,
                        code: 129,
                    })
                }
                e => fatal_of(e),
            };
            done(rgit_git::sparse_checkout(git_dir, &cwd, &args, &mut input).map_err(usage)?)
        }
        Plumbing::CheckoutIndex {
            all,
            force,
            index,
            quiet,
            no_create,
            prefix,
            stage,
            temp,
            stdin: from_stdin,
            z,
            mut paths,
        } => {
            if from_stdin {
                let input = stdin()?;
                let end = if z { 0 } else { b'\n' };
                paths.extend(
                    input
                        .split(|b| *b == end)
                        .filter(|p| !p.is_empty())
                        .map(|p| String::from_utf8_lossy(p).into_owned()),
                );
            }
            let opts = rgit_git::CheckoutIndexOpts {
                all,
                force,
                update_index: index,
                quiet,
                no_create,
                prefix,
                stage: match stage.as_deref() {
                    Some("all") => rgit_git::ALL_STAGES,
                    Some(n) => n.parse().unwrap_or(0),
                    None => 0,
                },
                temp,
                z,
            };
            done(rgit_git::checkout_index(git_dir, &cwd, &paths, &opts).map_err(fatal_of)?)
        }
        Plumbing::Mktree { z, missing, batch } => {
            lines(rgit_git::mktree(git_dir, &stdin()?, z, missing, batch).map_err(fatal_of)?)
        }
        Plumbing::Mktag { no_strict, .. } => {
            done(rgit_git::mktag(git_dir, &stdin()?, !no_strict).map_err(fatal_of)?)
        }
        p => return repoless(Some(backend), p, raw),
    })
}

/// `git interpret-trailers`: options apply in order, so they are parsed here
/// rather than by clap. `git_dir` supplies config when in a repository.
pub(crate) fn interpret_trailers(
    git_dir: Option<&std::path::Path>,
    args: Vec<String>,
    input: Option<String>,
) -> anyhow::Result<Output> {
    use rgit_git::{NewTrailer, parse_if_exists, parse_if_missing, parse_where};
    let mut o = rgit_git::TrailerOpts::default();
    let (mut where_, mut if_exists, mut if_missing) = (None, None, None);
    let mut in_place = false;
    let mut files = Vec::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        if !a.starts_with("--") || a == "--" {
            if a != "--" {
                files.push(a);
            }
            files.extend(it.by_ref());
            break;
        }
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) => (n.to_owned(), Some(v.to_owned())),
            None => (a.clone(), None),
        };
        let mut value = |what: &str| -> anyhow::Result<String> {
            inline
                .clone()
                .or_else(|| it.next())
                .ok_or_else(|| CliError::usage(format!("option `{what}' requires a value")))
        };
        let bad = |what: &str, v: &str| CliError::usage(format!("unknown value '{v}' for {what}"));
        match name.as_str() {
            "--in-place" => in_place = true,
            "--trim-empty" => o.trim_empty = true,
            "--only-trailers" => o.only_trailers = true,
            "--only-input" => o.only_input = true,
            "--unfold" => o.unfold = true,
            "--no-divider" => o.no_divider = true,
            "--parse" => {
                o.only_trailers = true;
                o.only_input = true;
                o.unfold = true;
            }
            "--where" => {
                let v = value("where")?;
                where_ = Some(parse_where(&v).ok_or_else(|| bad("--where", &v))?);
            }
            "--if-exists" => {
                let v = value("if-exists")?;
                if_exists = Some(parse_if_exists(&v).ok_or_else(|| bad("--if-exists", &v))?);
            }
            "--if-missing" => {
                let v = value("if-missing")?;
                if_missing = Some(parse_if_missing(&v).ok_or_else(|| bad("--if-missing", &v))?);
            }
            "--no-where" => where_ = None,
            "--no-if-exists" => if_exists = None,
            "--no-if-missing" => if_missing = None,
            "--trailer" => o.trailers.push(NewTrailer {
                text: value("trailer")?,
                where_,
                if_exists,
                if_missing,
            }),
            "--no-trailer" => o.trailers.clear(),
            _ => return Err(CliError::usage(format!("unknown option `{}'", &name[2..]))),
        }
    }
    if o.only_input && !o.trailers.is_empty() {
        return Err(CliError::usage(
            "--trailer with --only-input does not make sense",
        ));
    }
    if in_place && files.is_empty() {
        return Err(fatal("no input file given for in-place editing"));
    }
    let config: Vec<(String, Option<String>)> =
        rgit_git::config_list(git_dir, &rgit_git::ConfigScope::Any, true)
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.name, e.value))
            .collect();
    let comment = config
        .iter()
        .rev()
        .find(|(k, _)| k.eq_ignore_ascii_case("core.commentchar"))
        .and_then(|(_, v)| v.clone())
        .filter(|c| !c.is_empty() && c != "auto")
        .unwrap_or_else(|| "#".to_owned());
    let mut text = String::new();
    if files.is_empty() {
        let msg = String::from_utf8_lossy(&stdin_or(input)?).into_owned();
        text = rgit_git::interpret_trailers(&msg, &o, &config, &comment);
    }
    for f in &files {
        let msg =
            std::fs::read(f).map_err(|e| fatal(format!("could not read input file '{f}': {e}")))?;
        let out =
            rgit_git::interpret_trailers(&String::from_utf8_lossy(&msg), &o, &config, &comment);
        if in_place {
            std::fs::write(f, out)?;
        } else {
            text.push_str(&out);
        }
    }
    Ok(lines(text))
}

/// `git show-branch`; with no arguments at all, showbranch.default's when
/// `defaults` is set.
fn show_branch(
    backend: &Arc<dyn GitBackend>,
    command: Plumbing,
    raw: bool,
    defaults: bool,
) -> anyhow::Result<Output> {
    let Plumbing::ShowBranch {
        all,
        remotes,
        current,
        topo_order,
        date_order,
        sparse,
        more,
        list,
        merge_base,
        independent,
        no_name,
        sha1_name,
        topics,
        reflog,
        color,
        revs,
    } = command
    else {
        unreachable!("show-branch only")
    };
    let git_dir = backend.git_dir();
    let bare = !(all
        || remotes
        || current
        || topo_order
        || date_order
        || sparse
        || list
        || merge_base
        || independent
        || no_name
        || sha1_name
        || topics)
        && more.is_none()
        && reflog.is_none()
        && color.is_none()
        && revs.is_empty();
    if bare && defaults {
        let args = rgit_git::show_branch_defaults(&git_dir);
        if !args.is_empty() {
            let argv = ["rgit", "show-branch"]
                .into_iter()
                .map(str::to_owned)
                .chain(args);
            let cli = <crate::cli::Cli as clap::Parser>::try_parse_from(argv)
                .map_err(|e| CliError::usage(e.to_string()))?;
            if let Some(crate::cli::Command::Plumbing(c @ Plumbing::ShowBranch { .. })) =
                cli.command
            {
                return show_branch(backend, c, raw, false);
            }
        }
    }
    let reflog = reflog.map(|r| {
        let (n, base) = match r.split_once(',') {
            Some((n, b)) => (n.to_owned(), Some(b.to_owned())),
            None => (r, None),
        };
        (
            n.parse::<usize>().ok().filter(|&n| n > 0).unwrap_or(4),
            base,
        )
    });
    let when = color
        .or_else(|| backend.config_get("color.showbranch").ok().flatten())
        .or_else(|| backend.config_get("color.ui").ok().flatten())
        .unwrap_or_else(|| "auto".to_owned());
    let color = match when.as_str() {
        "always" | "true" | "yes" | "on" => true,
        "auto" => raw && std::io::IsTerminal::is_terminal(&std::io::stdout()),
        _ => false,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let reflog_date = reflog
        .as_ref()
        .and_then(|r| r.1.as_deref())
        .filter(|b| !b.bytes().all(|c| c.is_ascii_digit()))
        .map(|b| crate::date::approxidate_at(b, now).unwrap_or(now));
    let o = rgit_git::ShowBranchOpts {
        revs,
        reflog_date,
        all,
        remotes,
        current,
        date_order: date_order && !topo_order,
        sparse,
        extra: if list { -1 } else { more.unwrap_or(0) },
        merge_base,
        independent,
        no_name,
        sha1_name,
        topics,
        reflog,
        color,
    };
    let relative = |t: i64, off: i32| crate::pretty::format_date(t, off, "relative");
    let (text, status) =
        rgit_git::show_branch(&git_dir, &o, &relative).map_err(|e| fatal(e.to_string()))?;
    crate::cli::set_exit(status != 0);
    Ok(lines(text))
}

#[derive(Clone, Copy, PartialEq)]
enum BatchMode {
    Contents,
    Info,
    Command,
}

/// How `cat-file --batch`, `--batch-check` or `--batch-command` answers.
struct CatBatch<'a> {
    fmt: &'a str,
    mode: BatchMode,
    all: bool,
    buffer: bool,
    /// What ends each input line and each answer (`-z`, `-Z`).
    input: u8,
    output: u8,
    follow: bool,
    /// `Some(true)` for --textconv, `Some(false)` for --filters.
    convert: Option<bool>,
}

/// Where `--follow-symlinks` leads a `<tree-ish>:<path>` name.
enum Followed {
    Found(String),
    Missing,
    /// A link out of the tree, with its target.
    Outside(String),
    Dangling,
    Loop,
    NotDir,
}

/// Resolve `path` in `rev`'s tree, following symlinks inside it as git's
/// get_tree_entry_follow_symlinks does.
fn follow_symlinks(backend: &Arc<dyn GitBackend>, rev: &str, path: &str) -> Followed {
    let Ok(tree) = backend.resolve_object(&format!("{rev}^{{tree}}")) else {
        return Followed::Missing;
    };
    let mut todo: std::collections::VecDeque<String> = path.split('/').map(str::to_owned).collect();
    let mut cur: Vec<String> = Vec::new();
    let mut hops = 0;
    while let Some(part) = todo.pop_front() {
        match part.as_str() {
            "" | "." => continue,
            ".." => {
                if cur.pop().is_none() {
                    todo.push_front(part);
                    return Followed::Outside(Vec::from(todo).join("/"));
                }
                continue;
            }
            _ => {}
        }
        cur.push(part);
        let here = cur.join("/");
        let item = backend
            .ls_tree(&tree, std::slice::from_ref(&here), TreeWalk::default())
            .ok()
            .and_then(|items| items.into_iter().find(|i| i.path == here));
        let Some(item) = item else {
            return if hops == 0 {
                Followed::Missing
            } else {
                Followed::Dangling
            };
        };
        if item.mode == 0o120000 {
            hops += 1;
            if hops > 40 {
                return Followed::Loop;
            }
            let target = backend
                .read_object(&item.id)
                .map(|o| String::from_utf8_lossy(&o.data).into_owned())
                .unwrap_or_default();
            if target.starts_with('/') {
                return Followed::Outside(target);
            }
            cur.pop();
            for p in target.split('/').rev() {
                todo.push_front(p.to_owned());
            }
        } else if item.kind != "tree" && !todo.is_empty() {
            return Followed::NotDir;
        } else if todo.is_empty() {
            return Followed::Found(item.id);
        }
    }
    match cur.is_empty() {
        true => Followed::Found(tree),
        false => Followed::Missing,
    }
}

/// `git cat-file --batch[-check|-command][=FORMAT]`: one answer per object
/// name (or command) on stdin, or per object with `all`, written as it is
/// read so a caller can drive it as a coprocess.
fn cat_file_batch(
    backend: &Arc<dyn GitBackend>,
    opts: &CatBatch,
    raw: bool,
) -> anyhow::Result<Output> {
    let fmt = if opts.fmt.is_empty() {
        "%(objectname) %(objecttype) %(objectsize)"
    } else {
        opts.fmt
    };
    let split_rest = fmt.contains("%(rest)") || opts.convert.is_some();
    let names: Box<dyn Iterator<Item = std::io::Result<Vec<u8>>>> = if opts.all {
        Box::new(
            backend
                .all_objects()?
                .into_iter()
                .map(|n| Ok(n.into_bytes())),
        )
    } else {
        Box::new(std::io::BufRead::split(std::io::stdin().lock(), opts.input))
    };
    let mut stdout = std::io::stdout().lock();
    // --batch-command --buffer holds its answers until `flush` or the end.
    let mut held: Vec<u8> = Vec::new();
    let mut rows = Vec::new();
    let delim = opts.output as char;
    for line in names {
        let line = String::from_utf8_lossy(&line?).into_owned();
        let line = line
            .strip_suffix('\r')
            .filter(|_| opts.input == b'\n')
            .unwrap_or(&line);
        let (contents, line) = match opts.mode {
            BatchMode::Command => match line.split_once(' ') {
                _ if line.is_empty() => return Err(fatal("empty command in input")),
                Some(("contents", arg)) => (true, arg),
                Some(("info", arg)) => (false, arg),
                None if line == "flush" => {
                    if !opts.buffer {
                        return Err(fatal("flush is only for --buffer mode"));
                    }
                    stdout.write_all(&std::mem::take(&mut held))?;
                    stdout.flush()?;
                    continue;
                }
                _ => return Err(fatal(format!("unknown command: '{line}'"))),
            },
            mode => (mode == BatchMode::Contents, line),
        };
        let (name, rest) = if split_rest {
            let line = line.trim_start();
            line.split_once(char::is_whitespace)
                .map_or((line, ""), |(n, r)| (n, r.trim_start()))
        } else {
            (line, "")
        };
        let mut out = Vec::new();
        let followed = match name.split_once(':') {
            Some((rev, path)) if opts.follow && !rev.is_empty() => {
                follow_symlinks(backend, rev, path)
            }
            _ => Followed::Found(name.to_owned()),
        };
        let obj = match &followed {
            Followed::Found(id) => backend.read_object(id).ok(),
            _ => None,
        };
        match (obj, followed) {
            (Some(obj), _) => {
                let mut header = String::new();
                let mut text = fmt;
                while let Some(i) = text.find("%(") {
                    header.push_str(&text[..i]);
                    let Some(end) = text[i..].find(')') else {
                        header.push_str(&text[i..]);
                        text = "";
                        break;
                    };
                    match &text[i + 2..i + end] {
                        "objectname" => header.push_str(&obj.id),
                        "objecttype" => header.push_str(obj.kind),
                        "objectsize" => header.push_str(&obj.data.len().to_string()),
                        "objectsize:disk" => {
                            header.push_str(&backend.object_disk(&obj.id)?.0.to_string())
                        }
                        "deltabase" => header.push_str(
                            &backend
                                .object_disk(&obj.id)?
                                .1
                                .unwrap_or_else(|| "0".repeat(obj.id.len())),
                        ),
                        "rest" => header.push_str(rest),
                        atom => return Err(fatal(format!("unknown format element: %({atom})"))),
                    }
                    text = &text[i + end + 1..];
                }
                header.push_str(text);
                header.push(delim);
                out.extend_from_slice(header.as_bytes());
                if contents {
                    match opts.convert {
                        Some(textconv) if obj.kind == "blob" => {
                            if rest.is_empty() {
                                stdout.write_all(&out)?;
                                stdout.flush()?;
                                return Err(fatal(format!("missing path for '{}'", obj.id)));
                            }
                            out.extend(backend.convert_blob(rest, &obj.data, textconv)?);
                        }
                        _ => out.extend_from_slice(&obj.data),
                    }
                    out.push(opts.output);
                }
                rows.push(crate::obj! { "object" => obj.id, "type" => obj.kind, "size" => obj.data.len() });
            }
            (None, Followed::Outside(target)) => {
                out.extend(format!("symlink {}{delim}{target}{delim}", target.len()).bytes());
            }
            (None, f @ (Followed::Dangling | Followed::Loop | Followed::NotDir)) => {
                let what = match f {
                    Followed::Dangling => "dangling",
                    Followed::Loop => "loop",
                    _ => "notdir",
                };
                out.extend(format!("{what} {}{delim}{name}{delim}", name.len()).bytes());
            }
            (None, _) => {
                out.extend(format!("{name} missing{delim}").bytes());
                rows.push(crate::obj! { "object" => name, "type" => "missing", "size" => 0usize });
            }
        }
        if !raw {
            continue;
        }
        if opts.mode == BatchMode::Command && opts.buffer {
            held.extend(out);
            continue;
        }
        stdout.write_all(&out)?;
        if !opts.buffer {
            stdout.flush()?;
        }
    }
    stdout.write_all(&held)?;
    stdout.flush()?;
    Ok(if raw {
        Output::new(String::new())
    } else {
        table(
            String::new(),
            "objects",
            rows,
            &["object", "type", "size"],
            "0 objects",
        )
    })
}

const GIT_VARS: [&str; 11] = [
    "GIT_COMMITTER_IDENT",
    "GIT_AUTHOR_IDENT",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "GIT_PAGER",
    "GIT_DEFAULT_BRANCH",
    "GIT_SHELL_PATH",
    "GIT_ATTR_SYSTEM",
    "GIT_ATTR_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_GLOBAL",
];

/// `git var NAME`, or with `list` the config then every variable as `NAME=value`.
fn var(backend: &Arc<dyn GitBackend>, list: bool, name: Option<String>) -> anyhow::Result<Output> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let config = |k: &str| backend.config_get(k).ok().flatten();
    let home = || env("HOME").unwrap_or_default();
    let xdg = |file: &str| match env("XDG_CONFIG_HOME") {
        Some(x) => format!("{x}/git/{file}"),
        None => format!("{}/.config/git/{file}", home()),
    };
    let editor = || {
        env("GIT_EDITOR")
            .or_else(|| config("core.editor"))
            .or_else(|| env("VISUAL"))
            .or_else(|| env("EDITOR"))
            .unwrap_or_else(|| "vi".to_owned())
    };
    // Each variable's values; None when git has none to print.
    let value = |name: &str| -> Option<Vec<String>> {
        let one = |v: String| Some(vec![v]);
        match name {
            "GIT_AUTHOR_IDENT" => backend.ident(false).ok().and_then(one),
            "GIT_COMMITTER_IDENT" => backend.ident(true).ok().and_then(one),
            "GIT_EDITOR" => one(editor()),
            "GIT_SEQUENCE_EDITOR" => one(env("GIT_SEQUENCE_EDITOR")
                .or_else(|| config("sequence.editor"))
                .unwrap_or_else(editor)),
            "GIT_PAGER" => one(env("GIT_PAGER")
                .or_else(|| config("core.pager"))
                .or_else(|| env("PAGER"))
                .unwrap_or_else(|| "less".to_owned())),
            "GIT_DEFAULT_BRANCH" => {
                one(config("init.defaultBranch").unwrap_or_else(|| "master".to_owned()))
            }
            "GIT_SHELL_PATH" => one("/bin/sh".to_owned()),
            "GIT_ATTR_SYSTEM" if env("GIT_ATTR_NOSYSTEM").is_none() => {
                one("/etc/gitattributes".to_owned())
            }
            "GIT_ATTR_GLOBAL" => {
                one(config("core.attributesFile").unwrap_or_else(|| xdg("attributes")))
            }
            "GIT_CONFIG_SYSTEM" if env("GIT_CONFIG_NOSYSTEM").is_none() => {
                one(env("GIT_CONFIG_SYSTEM").unwrap_or_else(|| "/etc/gitconfig".to_owned()))
            }
            "GIT_CONFIG_GLOBAL" => Some(match env("GIT_CONFIG_GLOBAL") {
                Some(g) => vec![g],
                None => vec![xdg("config"), format!("{}/.gitconfig", home())],
            }),
            _ => None,
        }
    };
    if list {
        let mut out: Vec<String> = backend
            .config_entries(rgit_git::ConfigScope::Any, None)?
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        for name in GIT_VARS {
            for v in value(name).unwrap_or_default() {
                out.push(format!("{name}={v}"));
            }
        }
        return Ok(lines(terminated(out, false)));
    }
    let Some(name) = name else {
        return Err(CliError::usage("var needs a variable name, or -l"));
    };
    if !GIT_VARS.contains(&name.as_str()) {
        return Err(CliError::usage(format!(
            "unknown variable {name}; use one of {}",
            GIT_VARS.join(", ")
        )));
    }
    match value(&name) {
        Some(values) => Ok(lines(terminated(values, false))),
        None if name.ends_with("_IDENT") => Err(fatal(
            backend
                .ident(name.starts_with("GIT_COMMITTER"))
                .err()
                .map_or_else(String::new, |e| e.to_string()),
        )),
        None => Err(fail(true, true, "")),
    }
}

/// `git get-tar-commit-id`: the commit id in the pax global header
/// (`<len> comment=<id>\n`) that opens a tar on stdin.
pub fn get_tar_commit_id() -> anyhow::Result<Output> {
    use std::io::Read;
    let mut buf = [0u8; 1024];
    let mut n = 0;
    let mut stdin = std::io::stdin().lock();
    while n < buf.len() {
        match stdin.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    if n != buf.len() {
        return Err(fatal(
            "git get-tar-commit-id: EOF before reading tar header",
        ));
    }
    let none = || fail(true, true, "");
    if buf[156] != b'g' {
        return Err(none());
    }
    let content = &buf[512..];
    let digits = content.iter().take_while(|b| b.is_ascii_digit()).count();
    let len: usize = std::str::from_utf8(&content[..digits])
        .ok()
        .and_then(|d| d.parse().ok())
        .ok_or_else(none)?;
    let Some(rest) = content[digits..].strip_prefix(b" comment=") else {
        return Err(none());
    };
    let len = len
        .checked_sub(digits + " comment=".len())
        .ok_or_else(none)?;
    if len < 1 || len % 2 == 0 || ![20, 32].contains(&((len - 1) / 2)) || len > rest.len() {
        return Err(none());
    }
    Ok(lines(String::from_utf8_lossy(&rest[..len]).into_owned()))
}

/// `git merge-tree`: a real merge of two commits, a batch of them from
/// stdin, or the old trivial merge of three trees.
fn merge_tree(backend: &Arc<dyn GitBackend>, m: Plumbing, raw: bool) -> anyhow::Result<Output> {
    let Plumbing::MergeTree {
        write_tree,
        trivial_merge,
        quiet,
        stdin,
        xopts,
        name_only,
        messages,
        no_messages,
        z,
        allow_unrelated_histories,
        merge_base,
        args,
    } = m
    else {
        unreachable!()
    };
    let usage = || {
        CliError::usage(
            "usage: git merge-tree [--write-tree] [<options>] <branch1> <branch2>\n   \
             or: git merge-tree [--trivial-merge] <base-tree> <branch1> <branch2>",
        )
    };
    let incompatible = || fatal("--trivial-merge is incompatible with all other options");
    // An unknown branch exits 1, as git's help_unknown_ref does.
    let merge_error = |e: rgit_git::GitError| {
        let message = e.to_string();
        let code = if message.ends_with("not something we can merge") {
            1
        } else {
            128
        };
        anyhow::Error::new(CliError {
            message,
            help: None,
            code,
        })
    };
    if quiet {
        for (on, name) in [
            (messages, "--messages"),
            (name_only, "--name-only"),
            (stdin, "--stdin"),
            (z, "-z"),
        ] {
            if on {
                return Err(fatal(format!(
                    "options '--quiet' and '{name}' cannot be used together"
                )));
            }
        }
    }
    if trivial_merge && !xopts.is_empty() {
        return Err(incompatible());
    }
    let git_dir = backend.git_dir();
    let mut o = rgit_git::MergeTreeOpts {
        merge_base,
        allow_unrelated: allow_unrelated_histories,
        name_only,
        messages: if quiet {
            Some(false)
        } else {
            (messages || no_messages).then_some(messages)
        },
        z,
        xopts,
        prefix: top_and_prefix(backend).map(|(_, p)| p).unwrap_or_default(),
        ..Default::default()
    };
    if stdin {
        if trivial_merge {
            return Err(incompatible());
        }
        if o.merge_base.is_some() {
            return Err(fatal(
                "options '--merge-base' and '--stdin' cannot be used together",
            ));
        }
        (o.z, o.batch) = (true, true);
        let mut text = String::new();
        let mut stdout = std::io::stdout();
        for line in std::io::BufRead::split(std::io::stdin().lock(), b'\n') {
            let line = String::from_utf8_lossy(&line?).into_owned();
            let parts: Vec<&str> = line.split_inclusive(' ').map(str::trim_end).collect();
            let malformed = || fatal(format!("malformed input line: '{line}'."));
            let (base, b1, b2) = match parts[..] {
                [base, "--", b1, b2] => (Some(base), b1, b2),
                [b1, b2] if b2 != "--" => (None, b1, b2),
                _ => return Err(malformed()),
            };
            o.merge_base = base.map(str::to_owned);
            (o.branch1, o.branch2) = (b1.to_owned(), b2.to_owned());
            let (out, _) = rgit_git::merge_tree(&git_dir, &o).map_err(merge_error)?;
            if raw {
                stdout.write_all(out.as_bytes())?;
                stdout.flush()?;
            } else {
                text.push_str(&out);
            }
        }
        return Ok(lines(text));
    }
    if trivial_merge || (!write_tree && args.len() == 3) {
        if args.len() != 3 {
            return Err(usage());
        }
        if quiet
            || name_only
            || messages
            || no_messages
            || z
            || allow_unrelated_histories
            || o.merge_base.is_some()
            || !o.xopts.is_empty()
        {
            return Err(incompatible());
        }
        let (text, warnings) =
            rgit_git::merge_tree_trivial(&git_dir, [&args[0], &args[1], &args[2]])
                .map_err(|e| fatal(e.to_string()))?;
        eprint!("{warnings}");
        return Ok(lines(text));
    }
    let [b1, b2] = <[String; 2]>::try_from(args).map_err(|_| usage())?;
    (o.branch1, o.branch2) = (b1, b2);
    let (text, clean) = rgit_git::merge_tree(&git_dir, &o).map_err(merge_error)?;
    crate::cli::set_exit(!clean);
    Ok(lines(if quiet { String::new() } else { text }))
}

/// Exit 128 with `message`, as git's `fatal:` does.
fn fatal(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(CliError {
        message: message.into(),
        help: None,
        code: 128,
    })
}

fn ambiguous(arg: &str) -> anyhow::Error {
    fatal(format!(
        "ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'"
    ))
}

/// Errors of `@{upstream}` and `@{push}` that git reports as they are.
const GIT_ONLY: [&str; 6] = [
    "no upstream configured",
    "not stored as a remote-tracking",
    "no such branch:",
    "has no local tracking branch",
    "cannot resolve 'simple'",
    "push has no destination",
];

/// Whether a revision error is one git dies with while parsing (a missing
/// upstream), before it would call the argument ambiguous.
pub(crate) fn rev_dies(message: &str) -> bool {
    GIT_ONLY.iter().any(|m| message.contains(m))
}

/// The parents of the commit `rev` names.
fn parent_ids(backend: &Arc<dyn GitBackend>, rev: &str) -> anyhow::Result<Vec<String>> {
    let obj = backend.read_object(&backend.rev_parse(rev)?)?;
    Ok(crate::pretty::parse(&obj).parents)
}

/// Paths given before `--` must exist, as git checks.
fn on_disk(backend: &Arc<dyn GitBackend>, paths: &[String]) -> anyhow::Result<()> {
    let (top, prefix) = top_and_prefix(backend)?;
    match paths.iter().find(|p| !top.join(&prefix).join(p).exists()) {
        Some(p) => Err(ambiguous(p)),
        None => Ok(()),
    }
}

/// The format of a diff plumbing command, with its paths from the top level.
fn raw_diff_fmt(
    backend: &Arc<dyn GitBackend>,
    f: &RawDiffArgs,
    paths: Vec<String>,
    default_patch: bool,
) -> anyhow::Result<rgit_git::DiffFmt> {
    use rgit_git::DiffMode;
    let (top, prefix) = top_and_prefix(backend)?;
    let mode = if f.quiet || f.no_patch {
        DiffMode::Nothing
    } else if f.name_only {
        DiffMode::NameOnly
    } else if f.name_status {
        DiffMode::NameStatus
    } else {
        DiffMode::Raw
    };
    let stat = f.stat.is_some();
    let patch = f.patch || (default_patch && !f.raw && !stat && !f.summary);
    let score = f
        .find_copies
        .iter()
        .rev()
        .chain(&f.find_renames)
        .find(|s| !s.is_empty())
        .cloned();
    Ok(rgit_git::DiffFmt {
        mode,
        z: f.z,
        recursive: f.recursive
            || f.trees
            || (mode == DiffMode::Raw && (patch || stat || f.summary)),
        trees: f.trees,
        paths: paths
            .iter()
            .map(|p| crate::cli::repo_path(&top, Path::new(&prefix), p))
            .filter(|p| p != ".")
            .collect(),
        raw: f.raw || !(patch || stat || f.summary),
        stat,
        summary: f.summary,
        patch,
        detect: if f.find_copies_harder || f.find_copies.len() > 1 {
            3
        } else if !f.find_copies.is_empty() {
            2
        } else {
            u8::from(f.find_renames.is_some())
        },
        score,
        abbrev: f.abbrev,
        stat_width: f
            .stat
            .as_deref()
            .and_then(|s| s.split(',').next()?.parse().ok())
            .unwrap_or_else(crate::render::term_columns),
    })
}

fn raw_diff_done(f: &RawDiffArgs, (text, changed): (String, bool)) -> Output {
    if f.quiet || f.exit_code {
        crate::cli::set_exit(changed);
    }
    lines(text)
}

/// `git merge-file`: merge into the current file (or stdout, or a new blob
/// with --object-id) and exit with the number of conflicts.
pub(crate) fn merge_file(
    backend: Option<&Arc<dyn GitBackend>>,
    command: Plumbing,
    raw: bool,
) -> anyhow::Result<Output> {
    let Plumbing::MergeFile {
        labels,
        stdout,
        ours,
        theirs,
        union,
        diff3,
        zdiff3,
        marker_size,
        object_id,
        current,
        base,
        other,
        ..
    } = command
    else {
        unreachable!("merge_file takes merge-file");
    };
    if labels.len() > 3 {
        return Err(CliError::usage("too many labels on the command line"));
    }
    let read = |name: &str| -> anyhow::Result<Vec<u8>> {
        match backend.filter(|_| object_id) {
            Some(b) => Ok(b.read_object(name)?.data),
            None => std::fs::read(name)
                .map_err(|e| fatal(format!("could not open '{name}' for reading: {e}"))),
        }
    };
    let (a, b, c) = (read(&current)?, read(&base)?, read(&other)?);
    let mut names = [current.clone(), base, other];
    for (slot, label) in names.iter_mut().zip(labels) {
        *slot = label;
    }
    let o = rgit_git::MergeFileOpts {
        labels: names,
        favor: [(ours, "ours"), (theirs, "theirs"), (union, "union")]
            .iter()
            .find(|(on, _)| *on)
            .map(|(_, f)| (*f).to_owned()),
        style: (diff3 || zdiff3).then(|| if zdiff3 { "zdiff3" } else { "diff3" }.to_owned()),
        marker_size,
        alnum: true,
        ..Default::default()
    };
    let (merged, conflicts) = rgit_git::merge_file(&a, &b, &c, &o)?;
    crate::cli::set_exit_code(conflicts.min(127) as i32);
    if stdout {
        if raw {
            let mut out = std::io::stdout();
            out.write_all(&merged)?;
            out.flush()?;
            return Ok(Output::new(String::new()));
        }
        return Ok(lines(String::from_utf8_lossy(&merged).into_owned()));
    }
    if let Some(b) = backend.filter(|_| object_id) {
        let id = rgit_git::hash_object(Some(&b.git_dir()), "blob", &merged, None, true, false)?;
        return Ok(lines(format!("{id}\n")));
    }
    std::fs::write(&current, &merged)?;
    Ok(Output::new(String::new()).with("conflicts", conflicts))
}

/// The top-level folder and the current folder under it as git's prefix:
/// `dir/sub/`, empty at the top.
pub(crate) fn top_and_prefix(backend: &Arc<dyn GitBackend>) -> anyhow::Result<(PathBuf, String)> {
    let top = backend.workdir().canonicalize()?;
    let prefix = std::env::current_dir()?
        .canonicalize()
        .ok()
        .and_then(|cwd| {
            let rel = cwd.strip_prefix(&top).ok()?;
            Some(
                rel.components()
                    .map(|c| format!("{}/", c.as_os_str().to_string_lossy()))
                    .collect(),
            )
        })
        .unwrap_or_default();
    Ok((top, prefix))
}

/// A path from the top level as git prints it from the folder `prefix`
/// (`../c.txt`; the folder itself is `./`).
fn relative(path: &str, prefix: &str) -> String {
    if prefix.is_empty() {
        return path.to_owned();
    }
    let base: Vec<&str> = prefix.split('/').filter(|s| !s.is_empty()).collect();
    let parts: Vec<&str> = path.split('/').collect();
    let common = base.iter().zip(&parts).take_while(|(a, b)| a == b).count();
    let rel = format!(
        "{}{}",
        "../".repeat(base.len() - common),
        parts[common..].join("/")
    );
    if rel.is_empty() { "./".to_owned() } else { rel }
}

/// `s` in single quotes for a POSIX shell, as git's sq_quote writes it.
fn sq_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''").replace('!', "'\\!'"))
}

const LOCAL_ENV_VARS: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

#[derive(Clone, Copy, PartialEq)]
enum Show {
    Id,
    Symbolic,
    Full,
    Short,
}

/// `git rev-parse`: options and revisions answer in the order given; with
/// --verify (or --short) the one revision prints last.
fn rev_parse(
    backend: &Arc<dyn GitBackend>,
    args: Vec<String>,
    raw: bool,
) -> anyhow::Result<Output> {
    let (top, prefix) = top_and_prefix(backend)?;
    let cdup = "../".repeat(prefix.matches('/').count());
    let canon = |p: PathBuf| p.canonicalize().unwrap_or(p);
    let git_dir = canon(backend.git_dir());
    let common_dir = std::fs::read_to_string(git_dir.join("commondir"))
        .map(|c| canon(git_dir.join(c.trim())))
        .unwrap_or_else(|_| git_dir.clone());
    let dot_git = top.join(".git");
    // A revision as (name, negated) items: `A..B` is B and ^A.
    let expand = |rev: &str, not: bool| -> anyhow::Result<Vec<(String, bool)>> {
        let or_head = |s: &str| if s.is_empty() { "HEAD" } else { s }.to_owned();
        let items = if !rev.starts_with('^') && backend.resolve_object(rev).is_ok() {
            vec![(rev.to_owned(), not)]
        } else if let Some((a, b)) = rev.split_once("...") {
            let (a, b) = (or_head(a), or_head(b));
            let bases = backend.merge_bases(&a, &b, true)?;
            let mut items = vec![(b, not), (a, not)];
            items.extend(bases.into_iter().map(|id| (id, !not)));
            items
        } else if let Some((a, b)) = rev.split_once("..") {
            vec![(or_head(b), not), (or_head(a), !not)]
        } else if let Some(r) = rev.strip_prefix('^') {
            vec![(r.to_owned(), !not)]
        } else if let Some(base) = rev.strip_suffix("^@") {
            (1..=parent_ids(backend, base)?.len())
                .map(|i| (format!("{base}^{i}"), not))
                .collect()
        } else if let Some(base) = rev.strip_suffix("^!") {
            let mut items = vec![(base.to_owned(), not)];
            let n = parent_ids(backend, base)?.len();
            items.extend((1..=n).map(|i| (format!("{base}^{i}"), !not)));
            items
        } else if let Some((base, n)) = rev
            .rsplit_once("^-")
            .filter(|(_, n)| n.is_empty() || n.bytes().all(|b| b.is_ascii_digit()))
        {
            let n = if n.is_empty() { "1" } else { n };
            vec![(base.to_owned(), not), (format!("{base}^{n}"), !not)]
        } else {
            vec![(rev.to_owned(), not)]
        };
        for (r, _) in &items {
            backend.resolve_object(r)?;
        }
        Ok(items)
    };
    let one = |rev: &str,
               neg: bool,
               show: Show,
               short: Option<usize>|
     -> anyhow::Result<Option<String>> {
        let caret = if neg { "^" } else { "" };
        Ok(match show {
            Show::Id => {
                let id = backend.resolve_object(rev)?;
                let id = match short {
                    Some(n) => backend.abbrev_id(&id, n)?,
                    None => id,
                };
                Some(format!("{caret}{id}"))
            }
            Show::Symbolic => Some(format!("{caret}{rev}")),
            Show::Full | Show::Short => backend.full_ref_name(rev)?.map(|n| {
                let n = if show == Show::Short { shorten(&n) } else { n };
                format!("{caret}{n}")
            }),
        })
    };
    let (mut verify, mut quiet, mut short, mut show, mut not) =
        (false, false, None, Show::Id, false);
    let (mut default, mut seen_rev, mut paths) = (None, false, false);
    let mut verified: Vec<(String, bool)> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    let mut it = args.into_iter();
    while let Some(arg) = it
        .next()
        .or_else(|| (!seen_rev).then(|| default.take()).flatten())
    {
        if paths {
            out.push(arg);
            continue;
        }
        match arg.as_str() {
            "--" => {
                if !verify {
                    out.push(arg);
                }
                paths = true;
            }
            "--verify" => verify = true,
            "-q" | "--quiet" => quiet = true,
            "--short" => (verify, short) = (true, Some(0)),
            "--symbolic" => show = Show::Symbolic,
            "--symbolic-full-name" => show = Show::Full,
            "--abbrev-ref" | "--abbrev-ref=strict" | "--abbrev-ref=loose" => show = Show::Short,
            "--not" => not = !not,
            "--default" => default = it.next(),
            "--show-toplevel" => out.push(top.display().to_string()),
            "--show-prefix" => out.push(prefix.clone()),
            "--show-cdup" => out.push(cdup.clone()),
            "--git-dir" => out.push(if prefix.is_empty() && git_dir == dot_git {
                ".git".to_owned()
            } else {
                git_dir.display().to_string()
            }),
            "--absolute-git-dir" => out.push(git_dir.display().to_string()),
            "--git-common-dir" => out.push(if common_dir == dot_git {
                format!("{cdup}.git")
            } else {
                common_dir.display().to_string()
            }),
            "--is-inside-work-tree" => out.push("true".to_owned()),
            "--is-inside-git-dir" | "--is-bare-repository" => out.push("false".to_owned()),
            "--is-shallow-repository" => {
                out.push(common_dir.join("shallow").exists().to_string());
            }
            "--show-object-format"
            | "--show-object-format=storage"
            | "--show-object-format=input"
            | "--show-object-format=output" => {
                out.push(
                    backend
                        .config_get("extensions.objectformat")
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| "sha1".to_owned()),
                );
            }
            "--local-env-vars" => out.extend(LOCAL_ENV_VARS.iter().map(|v| v.to_string())),
            "--sq-quote" => out.push(it.by_ref().map(|a| format!(" {}", sq_quote(&a))).collect()),
            a @ ("--all" | "--branches" | "--tags" | "--remotes") => {
                seen_rev = true;
                let want = match a {
                    "--branches" => "refs/heads/",
                    "--tags" => "refs/tags/",
                    "--remotes" => "refs/remotes/",
                    _ => "refs/",
                };
                let caret = if not { "^" } else { "" };
                for r in backend.ref_details()? {
                    if r.name.starts_with(want) {
                        out.push(format!("{caret}{}", r.id));
                    }
                }
            }
            a if a.starts_with("--short=") => {
                verify = true;
                short =
                    Some(a["--short=".len()..].parse().map_err(|_| {
                        CliError::usage(format!("--short takes a number, not {a:?}"))
                    })?);
            }
            a if a.starts_with('-') && a.len() > 1 => out.push(arg),
            a => {
                seen_rev = true;
                match expand(a, not) {
                    Ok(items) if verify => verified.extend(items),
                    Ok(items) => {
                        for (r, neg) in items {
                            out.extend(one(&r, neg, show, short)?);
                        }
                    }
                    Err(_) if verify => verified.push((a.to_owned(), false)),
                    Err(_) if top.join(&prefix).join(a).exists() => {
                        out.push(arg);
                        paths = true;
                    }
                    Err(e) => {
                        let e = e.to_string();
                        return Err(fatal(
                            if a.contains("@{") && GIT_ONLY.iter().any(|m| e.contains(m)) {
                                e
                            } else {
                                format!(
                                    "ambiguous argument '{a}': unknown revision or path not in the working tree."
                                )
                            },
                        ));
                    }
                }
            }
        }
    }
    if verify {
        let found = match verified.as_slice() {
            [(r, neg)] => match one(r, *neg, show, short) {
                Err(e) if !quiet && rev_dies(&e.to_string()) => {
                    return Err(fatal(e.to_string()));
                }
                found => found.ok(),
            },
            _ => None,
        };
        match found {
            Some(line) => out.extend(line),
            None if quiet => return Err(fail(raw, true, "Needed a single revision")),
            None => return Err(fatal("Needed a single revision")),
        }
    }
    Ok(lines(terminated(out, false)))
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
pub(crate) fn glob(p: &[u8], s: &[u8]) -> bool {
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
    Version(Vec<Chunk>),
}

/// A run of digits or of other characters, for `version:` sorting.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Chunk {
    Num(u64),
    Text(String),
}

fn version_chunks(s: &str) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = Vec::new();
    for c in s.chars() {
        match (out.last_mut(), c.to_digit(10)) {
            (Some(Chunk::Num(n)), Some(d)) => *n = n.saturating_mul(10).saturating_add(d.into()),
            (Some(Chunk::Text(t)), None) => t.push(c),
            (_, Some(d)) => out.push(Chunk::Num(d.into())),
            (_, None) => out.push(Chunk::Text(c.to_string())),
        }
    }
    out
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

/// What for-each-ref formats and sorts refs with.
struct RefFormat<'a> {
    backend: &'a Arc<dyn GitBackend>,
    head: Option<String>,
    color: bool,
}

/// Order refs by git's `--sort` keys (for-each-ref, branch and tag), the
/// last one the main key; `icase` compares text keys case-insensitively.
pub(crate) fn sort_refs(
    backend: &Arc<dyn GitBackend>,
    refs: Vec<RefDetail>,
    sort: &[String],
    icase: bool,
) -> anyhow::Result<Vec<RefDetail>> {
    RefFormat::new(backend).sort(refs, sort, icase)
}

/// Each ref in a for-each-ref `format` (branch and tag `--format`), with
/// `%(color)` on when color is.
pub(crate) fn format_refs<'r>(
    backend: &Arc<dyn GitBackend>,
    refs: impl IntoIterator<Item = &'r RefDetail>,
    format: &str,
) -> anyhow::Result<Vec<String>> {
    let mut ctx = RefFormat::new(backend);
    ctx.color = crate::render::color_on();
    let fmt = parse_format(format)?;
    // Like ref-filter, end each line in a reset when the last color set is
    // not one.
    let reset = ctx.color
        && format.contains("%(color:")
        && !format
            .rsplit("%(color:")
            .next()
            .is_some_and(|c| c.starts_with("reset)"));
    refs.into_iter()
        .map(|r| {
            let line = ctx.render(&fmt, r)?;
            Ok(if reset { line + "\x1b[m" } else { line })
        })
        .collect()
}

impl<'a> RefFormat<'a> {
    fn new(backend: &'a Arc<dyn GitBackend>) -> Self {
        RefFormat {
            backend,
            head: backend.symbolic_ref("HEAD").ok().flatten(),
            color: false,
        }
    }

    /// `refs` by `sort`'s keys, applied in turn so the last is the main one.
    fn sort(
        &self,
        mut refs: Vec<RefDetail>,
        sort: &[String],
        icase: bool,
    ) -> anyhow::Result<Vec<RefDetail>> {
        for key in sort {
            let (desc, key) = match key.strip_prefix('-') {
                Some(k) => (true, k),
                None => (false, key.as_str()),
            };
            let mut keyed = Vec::new();
            for r in refs {
                let k = match self.sort_key(&r, key)? {
                    Key::Text(t) if icase => Key::Text(t.to_lowercase()),
                    k => k,
                };
                keyed.push((k, r));
            }
            keyed.sort_by(|a, b| if desc { b.0.cmp(&a.0) } else { a.0.cmp(&b.0) });
            refs = keyed.into_iter().map(|(_, r)| r).collect();
        }
        Ok(refs)
    }

    fn sort_key(&self, r: &RefDetail, key: &str) -> anyhow::Result<Key> {
        let (version, key) = match key
            .strip_prefix("version:")
            .or_else(|| key.strip_prefix("v:"))
        {
            Some(k) => (true, k),
            None => (false, key),
        };
        let atom = key.split(':').next().unwrap_or(key);
        Ok(match date_of(r, atom) {
            Some(ident) => Key::Num(ident.map_or(0, |i| i.time)),
            None if version => Key::Version(version_chunks(&self.atom(r, key)?)),
            None => Key::Text(self.atom(r, key)?),
        })
    }

    fn render(&self, nodes: &[Node], r: &RefDetail) -> anyhow::Result<String> {
        let mut out = String::new();
        for node in nodes {
            match node {
                Node::Text(t) => out.push_str(t),
                Node::Atom(a) => out.push_str(&self.atom(r, a)?),
                Node::Align(spec, body) => out.push_str(&align(spec, &self.render(body, r)?)?),
                Node::If(spec, cond, then, els) => {
                    let value = self.render(cond, r)?;
                    let yes = match spec.split_once('=') {
                        Some(("equals", s)) => value == *s,
                        Some(("notequals", s)) => value != *s,
                        _ => !value.trim().is_empty(),
                    };
                    out.push_str(&self.render(if yes { then } else { els }, r)?);
                }
            }
        }
        Ok(out)
    }

    fn atom(&self, r: &RefDetail, spec: &str) -> anyhow::Result<String> {
        let (deref, spec) = match spec.strip_prefix('*') {
            Some(s) => (true, s),
            None => (false, spec),
        };
        let (atom, arg) = spec.split_once(':').unwrap_or((spec, ""));
        if deref {
            if atom == "refname" {
                return Ok(format!("{}^{{}}", self.atom(r, spec)?));
            }
            return match &r.deref {
                Some(d) => self.atom(d, spec),
                None => Ok(String::new()),
            };
        }
        // A signed tag's signature ends its message; only %(contents:body)
        // leaves it out, as in git.
        let message = r.message.as_str();
        let signature = match (r.kind, signature_start(message)) {
            ("tag", Some(at)) => &message[at..],
            _ => "",
        };
        let subject = || {
            message
                .split("\n\n")
                .next()
                .unwrap_or_default()
                .lines()
                .collect::<Vec<_>>()
                .join(" ")
        };
        let body = || {
            message
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
                "date" => crate::pretty::format_date(i.time, i.offset, arg),
                _ => format!(
                    "{} <{}> {}",
                    i.name,
                    i.email,
                    crate::pretty::format_date(i.time, i.offset, "raw")
                ),
            }
        };
        let short = |s: &str| {
            if arg == "short" {
                shorten(s)
            } else {
                s.to_owned()
            }
        };
        let parents = || {
            (1..)
                .map_while(|n| self.backend.resolve_object(&format!("{}^{n}", r.id)).ok())
                .collect::<Vec<_>>()
        };
        Ok(match (atom, arg) {
            ("refname", "short") => shorten(&r.name),
            ("refname", "") => r.name.clone(),
            ("refname", _) => strip(&r.name, arg)?,
            ("objectname", "") => r.id.clone(),
            ("objectname", "short") => self.backend.abbrev_id(&r.id, 0)?,
            ("objectname", a) => {
                let n = a
                    .strip_prefix("short=")
                    .and_then(|n| n.parse().ok())
                    .ok_or_else(|| CliError::usage(format!("unknown objectname modifier {a:?}")))?;
                self.backend.abbrev_id(&r.id, n)?
            }
            ("objecttype", _) => r.kind.to_owned(),
            ("objectsize", _) => self.backend.read_object(&r.id)?.data.len().to_string(),
            ("tree", _) if r.kind == "commit" => {
                self.backend.resolve_object(&format!("{}^{{tree}}", r.id))?
            }
            ("parent", _) if r.kind == "commit" => parents().join(" "),
            ("numparent", _) if r.kind == "commit" => parents().len().to_string(),
            ("tree" | "parent" | "numparent", _) => String::new(),
            ("subject", "sanitize") => crate::pretty::sanitize(&subject()),
            ("subject", _) | ("contents", "subject") => subject(),
            ("body", _) => body(),
            ("contents", "body") => body()
                .strip_suffix(signature)
                .unwrap_or_default()
                .to_owned(),
            ("contents", "signature") => signature.to_owned(),
            ("contents", "size") => message.len().to_string(),
            ("contents", "") => message.to_owned(),
            ("trailers", _) => trailers(message.strip_suffix(signature).unwrap_or(message), arg)?,
            ("contents", a) if a == "trailers" || a.starts_with("trailers:") => trailers(
                message.strip_suffix(signature).unwrap_or(message),
                a.trim_start_matches("trailers").trim_start_matches(':'),
            )?,
            ("contents", a) => {
                let n: usize = a
                    .strip_prefix("lines=")
                    .and_then(|n| n.parse().ok())
                    .ok_or_else(|| anyhow::anyhow!("unrecognized %(contents) argument: {a}"))?;
                let contents = message.strip_suffix(signature).unwrap_or(message);
                let mut out = Vec::new();
                let mut rest = contents;
                while out.len() < n && !rest.is_empty() {
                    let (line, next) = rest.split_once('\n').unwrap_or((rest, ""));
                    out.push(line);
                    rest = next;
                }
                out.join("\n    ")
            }
            ("HEAD", _) => if self.head.as_deref() == Some(r.name.as_str()) {
                "*"
            } else {
                " "
            }
            .to_owned(),
            ("symref", _) => r.symref.as_deref().map(short).unwrap_or_default(),
            ("upstream", "track" | "trackshort" | "track,nobracket") => {
                let Some((_, ahead, behind)) = r
                    .upstream
                    .as_ref()
                    .and_then(|_| self.backend.branch_upstream(&shorten(&r.name)).ok())
                    .flatten()
                else {
                    return Ok(String::new());
                };
                let text = match (arg, ahead, behind) {
                    ("trackshort", 0, 0) => "=".to_owned(),
                    ("trackshort", _, 0) => ">".to_owned(),
                    ("trackshort", 0, _) => "<".to_owned(),
                    ("trackshort", _, _) => "<>".to_owned(),
                    (_, 0, 0) => String::new(),
                    (_, a, 0) => format!("ahead {a}"),
                    (_, 0, b) => format!("behind {b}"),
                    (_, a, b) => format!("ahead {a}, behind {b}"),
                };
                if arg == "track" && !text.is_empty() {
                    format!("[{text}]")
                } else {
                    text
                }
            }
            ("upstream", _) => r.upstream.as_deref().map(short).unwrap_or_default(),
            ("color", _) => {
                let code = crate::render::ansi(arg)
                    .ok_or_else(|| anyhow::anyhow!("unrecognized color: %(color:{arg})"))?;
                if self.color { code } else { String::new() }
            }
            ("describe", _) => {
                let (mut tags, mut abbrev, mut pattern) = (false, None, None);
                for opt in arg.split(',').filter(|o| !o.is_empty()) {
                    match opt.split_once('=') {
                        None if opt == "tags" => tags = true,
                        Some(("tags", v)) => tags = v != "false",
                        Some(("abbrev", n)) => abbrev = n.parse().ok(),
                        Some(("match", m)) => pattern = Some(m),
                        _ => {
                            return Err(CliError::usage(format!(
                                "unrecognized %(describe) argument: {opt}"
                            )));
                        }
                    }
                }
                let opts = rgit_git::DescribeOptions {
                    tags,
                    abbrev,
                    matches: pattern.into_iter().map(str::to_owned).collect(),
                    ..Default::default()
                };
                self.backend.describe(&r.id, &opts).unwrap_or_default()
            }
            ("ahead-behind", base) if !base.is_empty() => {
                let count = |range: String| {
                    self.backend
                        .rev_walk(&rgit_git::LogOptions {
                            revs: vec![range],
                            ..Default::default()
                        })
                        .map(|c| c.len())
                };
                let tip = r.peeled.as_deref().unwrap_or(&r.id);
                format!(
                    "{} {}",
                    count(format!("{base}..{tip}"))?,
                    count(format!("{tip}..{base}"))?
                )
            }
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
}

/// Where a tag message's signature starts (git's parse_signature).
/// git's `%(trailers[:<options>])`: the trailer block ending `message`,
/// filtered and joined as `only`, `unfold`, `key=`, `valueonly`, `separator=`
/// and `key_value_separator=` ask.
fn trailers(message: &str, opts: &str) -> anyhow::Result<String> {
    let (mut only, mut unfold, mut valueonly) = (false, false, false);
    let (mut keys, mut sep, mut kv_sep) = (Vec::new(), None, None);
    let flag = |v: Option<&str>| !matches!(v, Some("false" | "no" | "off" | "0"));
    for o in opts.split(',').filter(|o| !o.is_empty()) {
        let (k, v) = match o.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (o, None),
        };
        match k {
            "only" => only = flag(v),
            "unfold" => unfold = flag(v),
            "valueonly" => valueonly = flag(v),
            "key" => keys.push(v.unwrap_or_default().trim_end_matches(':').to_lowercase()),
            "separator" => sep = v.map(expand_literal),
            "key_value_separator" => kv_sep = v.map(expand_literal),
            _ => anyhow::bail!("unknown %(trailers) argument: {o}"),
        }
    }
    let text = message.trim_end_matches('\n');
    let Some((_, block)) = text.split_once("\n\n").and(text.rsplit_once("\n\n")) else {
        return Ok(String::new());
    };
    let mut items: Vec<String> = Vec::new();
    for line in block.lines() {
        match items.last_mut() {
            Some(last) if line.starts_with([' ', '\t']) => {
                last.push('\n');
                last.push_str(line);
            }
            _ => items.push(line.to_owned()),
        }
    }
    let token = |l: &str| {
        l.split_once(':')
            .map(|(k, v)| (k.to_owned(), v.trim_start().to_owned()))
            .filter(|(k, _)| !k.is_empty() && !k.contains(char::is_whitespace))
    };
    if items.is_empty() || !items.iter().all(|l| token(l).is_some()) {
        return Ok(String::new());
    }
    let raw =
        !only && !unfold && !valueonly && keys.is_empty() && sep.is_none() && kv_sep.is_none();
    if raw {
        return Ok(format!("{block}\n"));
    }
    let out: Vec<String> = items
        .iter()
        .filter_map(|l| {
            let (k, mut v) = token(l)?;
            if !keys.is_empty() && !keys.contains(&k.to_lowercase()) {
                return None;
            }
            if unfold {
                v = v.split_whitespace().collect::<Vec<_>>().join(" ");
            }
            Some(if valueonly {
                v
            } else {
                format!("{k}{}{v}", kv_sep.as_deref().unwrap_or(": "))
            })
        })
        .collect();
    Ok(match sep {
        Some(sep) => out.join(&sep),
        None => out.iter().map(|l| format!("{l}\n")).collect(),
    })
}

/// `%n` and `%xNN` in a format option expanded, as git's
/// strbuf_expand_literal does.
fn expand_literal(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        rest = &rest[i + 1..];
        if let Some(r) = rest.strip_prefix('n') {
            out.push('\n');
            rest = r;
        } else if let Some(b) = rest
            .strip_prefix('x')
            .and_then(|r| r.get(..2))
            .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b as char);
            rest = &rest[3..];
        } else {
            out.push('%');
        }
    }
    out + rest
}

fn signature_start(message: &str) -> Option<usize> {
    const STARTS: [&str; 4] = [
        "-----BEGIN PGP SIGNATURE-----",
        "-----BEGIN PGP MESSAGE-----",
        "-----BEGIN SSH SIGNATURE-----",
        "-----BEGIN SIGNED MESSAGE-----",
    ];
    let mut at = 0;
    let mut found = None;
    for line in message.split_inclusive('\n') {
        if found.is_none() && STARTS.iter().any(|s| line.starts_with(s)) {
            found = Some(at);
        }
        at += line.len();
    }
    found
}

/// A parsed for-each-ref format: text, atoms, and `%(align)` / `%(if)`
/// blocks up to their `%(end)`.
enum Node {
    Text(String),
    Atom(String),
    Align(String, Vec<Node>),
    If(String, Vec<Node>, Vec<Node>, Vec<Node>),
}

/// Parse `%(atom)`, `%%` and `%xx` in a for-each-ref format.
fn parse_format(fmt: &str) -> anyhow::Result<Vec<Node>> {
    let mut tokens = Vec::new();
    let mut rest = fmt;
    let mut text = String::new();
    while let Some(i) = rest.find('%') {
        text.push_str(&rest[..i]);
        rest = &rest[i + 1..];
        if let Some(r2) = rest.strip_prefix('%') {
            text.push('%');
            rest = r2;
        } else if let Some(inner) = rest.strip_prefix('(') {
            let end = inner
                .find(')')
                .ok_or_else(|| CliError::usage(format!("unterminated %( in format {fmt:?}")))?;
            tokens.push(Node::Text(std::mem::take(&mut text)));
            tokens.push(Node::Atom(inner[..end].to_owned()));
            rest = &inner[end + 1..];
        } else if let Some(byte) = rest.get(..2).and_then(|h| u8::from_str_radix(h, 16).ok()) {
            text.push(byte as char);
            rest = &rest[2..];
        } else {
            text.push('%');
        }
    }
    text.push_str(rest);
    tokens.push(Node::Text(text));
    let mut it = tokens.into_iter();
    match block(&mut it)? {
        (nodes, None) => Ok(nodes),
        (_, Some(stop)) => Err(CliError::usage(format!(
            "format: %({stop}) atom used without a %(if) or %(align)"
        ))),
    }
}

/// Nodes up to a `then`, `else` or `end` atom, which is returned too.
fn block(it: &mut impl Iterator<Item = Node>) -> anyhow::Result<(Vec<Node>, Option<String>)> {
    let mut nodes = Vec::new();
    let expect = |got: Option<String>, want: &str| {
        if got.as_deref() == Some(want) {
            Ok(())
        } else {
            Err(CliError::usage(format!("format: missing %({want})")))
        }
    };
    while let Some(node) = it.next() {
        let Node::Atom(a) = node else {
            nodes.push(node);
            continue;
        };
        let (name, arg) = a.split_once(':').unwrap_or((&a, ""));
        match name {
            "then" | "else" | "end" => return Ok((nodes, Some(name.to_owned()))),
            "align" => {
                let (body, stop) = block(it)?;
                expect(stop, "end")?;
                nodes.push(Node::Align(arg.to_owned(), body));
            }
            "if" => {
                let (cond, stop) = block(it)?;
                expect(stop, "then")?;
                let (then, stop) = block(it)?;
                let els = if stop.as_deref() == Some("else") {
                    let (els, stop) = block(it)?;
                    expect(stop, "end")?;
                    els
                } else {
                    expect(stop, "end")?;
                    Vec::new()
                };
                nodes.push(Node::If(arg.to_owned(), cond, then, els));
            }
            _ => nodes.push(Node::Atom(a)),
        }
    }
    Ok((nodes, None))
}

/// `%(align:N[,left|middle|right])` or `%(align:width=N,position=P)`.
fn align(spec: &str, text: &str) -> anyhow::Result<String> {
    let (mut width, mut position) = (None, "left");
    for part in spec.split(',').filter(|p| !p.is_empty()) {
        let value = part
            .strip_prefix("width=")
            .or_else(|| part.strip_prefix("position="))
            .unwrap_or(part);
        match value.parse::<usize>() {
            Ok(n) => width = Some(n),
            Err(_) if matches!(value, "left" | "middle" | "right") => position = value,
            Err(_) => {
                return Err(CliError::usage(format!(
                    "unrecognized %(align) argument: {part}"
                )));
            }
        }
    }
    let width =
        width.ok_or_else(|| CliError::usage("positive width expected with the %(align) atom"))?;
    let pad = width.saturating_sub(text.chars().count());
    Ok(match position {
        "right" => format!("{}{text}", " ".repeat(pad)),
        "middle" => format!("{}{text}{}", " ".repeat(pad / 2), " ".repeat(pad - pad / 2)),
        _ => format!("{text}{}", " ".repeat(pad)),
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

/// The raw `--and`, `--or`, `--not`, `(` and `)` of `grep`, which
/// [`grep_tokens`] passes as `-e` values behind this mark to keep their order
/// among the patterns.
pub(crate) const GREP_OP: char = '\0';

/// `grep`'s arguments with its expression operators turned into marked `-e`
/// values, so they stay in order among the patterns as git reads them.
pub(crate) fn grep_tokens(args: &[String]) -> Vec<String> {
    let mut skip = false;
    let mut sub = None;
    for (i, a) in args.iter().enumerate() {
        if skip {
            skip = false;
        } else if a == "--fields" {
            skip = true;
        } else if !a.starts_with('-') {
            sub = Some(i);
            break;
        }
    }
    let Some(sub) = sub.filter(|&i| args[i] == "grep") else {
        return args.to_vec();
    };
    let mut out = args[..=sub].to_vec();
    let mut rest = args[sub + 1..].iter();
    let mut value = false;
    for a in rest.by_ref() {
        if a == "--" {
            out.push(a.clone());
            break;
        }
        let op = matches!(a.as_str(), "--and" | "--or" | "--not" | "(" | ")");
        if op && !value {
            out.push("-e".to_owned());
            out.push(format!("{GREP_OP}{}", a.trim_start_matches('-')));
        } else {
            out.push(a.clone());
        }
        value = !value && matches!(a.as_str(), "-e" | "--regexp");
    }
    out.extend(rest.cloned());
    out
}

/// git's grep expression over `tokens` (patterns and marked operators): the
/// patterns alone and how they combine, `None` when no operator is given.
fn grep_expr(tokens: Vec<String>) -> anyhow::Result<(Vec<String>, Option<GrepExpr>)> {
    enum Tok {
        Pat(usize),
        And,
        Not,
        Open,
        Close,
    }
    let mut patterns = Vec::new();
    let mut toks = Vec::new();
    for t in tokens {
        match t.strip_prefix(GREP_OP) {
            // git takes `--or` as the default and ignores it.
            Some("or") => {}
            Some("and") => toks.push(Tok::And),
            Some("not") => toks.push(Tok::Not),
            Some("(") => toks.push(Tok::Open),
            Some(_) => toks.push(Tok::Close),
            None => {
                toks.push(Tok::Pat(patterns.len()));
                patterns.push(t);
            }
        }
    }
    if toks.iter().all(|t| matches!(t, Tok::Pat(_))) {
        return Ok((patterns, None));
    }
    type Parsed = anyhow::Result<Option<GrepExpr>>;
    fn or(t: &[Tok], i: &mut usize) -> Parsed {
        let Some(x) = and(t, i)? else {
            return Ok(None);
        };
        if *i < t.len() && !matches!(t[*i], Tok::Close) {
            let Some(y) = or(t, i)? else {
                return Err(fatal("not a pattern expression"));
            };
            return Ok(Some(GrepExpr::Or(Box::new(x), Box::new(y))));
        }
        Ok(Some(x))
    }
    fn and(t: &[Tok], i: &mut usize) -> Parsed {
        let x = not(t, i)?;
        if *i < t.len() && matches!(t[*i], Tok::And) {
            let Some(x) = x else {
                return Err(fatal("--and not preceded by pattern expression"));
            };
            *i += 1;
            let Some(y) = and(t, i)? else {
                return Err(fatal("--and not followed by pattern expression"));
            };
            return Ok(Some(GrepExpr::And(Box::new(x), Box::new(y))));
        }
        Ok(x)
    }
    fn not(t: &[Tok], i: &mut usize) -> Parsed {
        if *i < t.len() && matches!(t[*i], Tok::Not) {
            *i += 1;
            if *i == t.len() {
                return Err(fatal("--not not followed by pattern expression"));
            }
            let Some(x) = not(t, i)? else {
                return Err(fatal("--not followed by non pattern expression"));
            };
            return Ok(Some(GrepExpr::Not(Box::new(x))));
        }
        atom(t, i)
    }
    fn atom(t: &[Tok], i: &mut usize) -> Parsed {
        match t.get(*i) {
            Some(Tok::Pat(n)) => {
                *i += 1;
                Ok(Some(GrepExpr::Atom(*n)))
            }
            Some(Tok::Open) => {
                *i += 1;
                let x = or(t, i)?;
                if !matches!(t.get(*i), Some(Tok::Close)) {
                    return Err(fatal("unmatched ( for expression group"));
                }
                *i += 1;
                Ok(x)
            }
            _ => Ok(None),
        }
    }
    let mut i = 0;
    let expr = or(&toks, &mut i)?;
    if i < toks.len() {
        return Err(fatal("incomplete pattern expression group: )"));
    }
    Ok((patterns, expr))
}

/// `git grep`; without a backend (`--no-index` outside a repository) it
/// searches the current folder.
pub(crate) fn grep(
    backend: Option<&Arc<dyn GitBackend>>,
    command: Plumbing,
    raw: bool,
) -> anyhow::Result<Output> {
    let Plumbing::Grep {
        help: _,
        after,
        before,
        context,
        only_matching,
        files_without_match,
        heading,
        break_,
        no_filename,
        with_filename: _,
        max_count,
        skip_binary,
        null,
        full_name,
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
        all_match,
        show_function,
        function_context,
        cached,
        untracked,
        no_index,
        exclude_standard,
        no_exclude_standard,
        recurse_submodules,
        threads: _,
        mut args,
        mut paths,
    } = command
    else {
        anyhow::bail!("not a grep command");
    };
    if patterns.is_empty() {
        if args.is_empty() {
            return Err(CliError::usage("grep needs a pattern"));
        }
        patterns.push(args.remove(0));
    }
    let (patterns, expr) = grep_expr(patterns)?;
    let backend = backend.filter(|_| !no_index);
    let (top, prefix) = match backend {
        Some(b) => top_and_prefix(b)?,
        None => (std::env::current_dir()?, String::new()),
    };
    let mut revs = Vec::new();
    for a in args {
        match backend {
            Some(b) if b.resolve_object(&a).is_ok() => revs.push(a),
            Some(_) => paths.push(crate::cli::repo_path(
                &top,
                std::path::Path::new(&prefix),
                &a,
            )),
            None => paths.push(a),
        }
    }
    if (paths.is_empty() || rgit_git::only_excludes(&paths)) && !prefix.is_empty() {
        paths.push(prefix.clone());
    }
    let syntax = if fixed {
        GrepSyntax::Fixed
    } else if perl {
        GrepSyntax::Perl
    } else if extended {
        GrepSyntax::Extended
    } else {
        GrepSyntax::Basic
    };
    let sources: Vec<Option<String>> = if revs.is_empty() {
        vec![None]
    } else {
        revs.into_iter().map(Some).collect()
    };
    let (sep, ctx_sep, fn_sep, end) = if null {
        ('\0', '\0', '\0', '\0')
    } else {
        (':', '-', '=', '\n')
    };
    let before = before.or(context).unwrap_or(0);
    let after = after.or(context).unwrap_or(0);
    let show_name = !no_filename && !heading;
    // git's default grep colors.
    let color = crate::render::color_on();
    let paint = |s: &str, code: &str| {
        if color && !s.is_empty() {
            format!("\x1b[{code}m{s}\x1b[m")
        } else {
            s.to_owned()
        }
    };
    let psep = |c: char| {
        if c == '\0' {
            c.to_string()
        } else {
            paint(&c.to_string(), "36")
        }
    };
    let mut text = String::new();
    let mut rows = Vec::new();
    let mut last: Option<(String, u64)> = None;
    for rev in sources {
        let q = GitGrep {
            patterns: patterns.clone(),
            syntax,
            ignore_case,
            word,
            invert,
            cached,
            rev: rev.clone(),
            paths: paths.clone(),
            before,
            after,
            max_count,
            only_matching,
            files_without_match,
            skip_binary,
            expr: expr.clone(),
            all_match,
            show_function,
            function_context,
            untracked,
            no_exclude: no_exclude_standard,
            recurse_submodules,
            spans: color,
        };
        let hits = match backend {
            Some(b) => b.git_grep(&q)?,
            None => rgit_git::grep_dir(&top, &q, exclude_standard)?,
        };
        let mut per_file: Vec<(String, usize)> = Vec::new();
        for h in &hits {
            let name = if full_name {
                h.path.clone()
            } else {
                relative(&h.path, &prefix)
            };
            let file = match &rev {
                Some(r) => format!("{r}:{name}"),
                None => name,
            };
            let n = usize::from(!h.context);
            match per_file.last_mut() {
                Some((p, c)) if *p == file => *c += n,
                _ => per_file.push((file.clone(), n)),
            }
            if !h.context {
                let mut row =
                    crate::obj! { "path" => h.path, "line" => h.line as usize, "text" => h.text };
                if let Some(r) = &rev {
                    row.push(("rev".to_owned(), r.as_str().into()));
                }
                rows.push(row);
            }
            if files || files_without_match || count {
                continue;
            }
            let hunks = before + after > 0 || function_context;
            let new_file = last.as_ref().is_none_or(|(f, _)| *f != file);
            match &last {
                Some(_) if new_file && break_ => text.push('\n'),
                Some((_, l)) if hunks && (new_file || h.line != l + 1) => {
                    text.push_str(&format!("{}\n", paint("--", "36")))
                }
                _ => {}
            }
            last = Some((file.clone(), h.line));
            // git prints nothing for a binary match with context or
            // --break, though it still separates it from the next file.
            if h.binary {
                if !hunks && !break_ {
                    text.push_str(&format!("Binary file {file} matches\n"));
                }
                continue;
            }
            if heading && new_file {
                text.push_str(&format!("{}\n", paint(&file, "35")));
            }
            let s = psep(if h.function {
                fn_sep
            } else if h.context {
                ctx_sep
            } else {
                sep
            });
            let mut lead = String::new();
            if show_name {
                lead.push_str(&format!("{}{s}", paint(&file, "35")));
            }
            if line_number {
                lead.push_str(&format!("{}{s}", paint(&h.line.to_string(), "32")));
            }
            if only_matching && !h.function {
                for part in &h.parts {
                    text.push_str(&format!("{lead}{}\n", paint(part, "1;31")));
                }
            } else if color && !h.spans.is_empty() {
                let mut line = String::new();
                let mut at = 0;
                for &(a, b) in &h.spans {
                    if a < at || b > h.text.len() {
                        continue;
                    }
                    line.push_str(&h.text[at..a]);
                    line.push_str(&paint(&h.text[a..b], "1;31"));
                    at = b;
                }
                line.push_str(&h.text[at..]);
                text.push_str(&format!("{lead}{line}\n"));
            } else {
                text.push_str(&format!("{lead}{}\n", h.text));
            }
        }
        for (p, n) in per_file {
            if files && n > 0 || files_without_match {
                text.push_str(&format!("{}{end}", paint(&p, "35")));
            } else if count && n > 0 {
                if no_filename {
                    text.push_str(&format!("{n}\n"));
                } else {
                    text.push_str(&format!("{}{}{n}\n", paint(&p, "35"), psep(sep)));
                }
            }
        }
    }
    if rows.is_empty() && raw {
        return Err(fail(raw, true, ""));
    }
    if quiet {
        text.clear();
    }
    Ok(table(
        text,
        "matches",
        rows,
        &["path", "line", "text"],
        &format!("0 matches for {:?}", patterns.join("|")),
    ))
}

/// `%XX` escapes decoded, as combine: filter parts are written.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = b
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok());
        match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
            Some(v) if b[i] == b'%' => {
                out.push(v);
                i += 3;
            }
            _ => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A `--filter` spec of rev-list's object walk.
enum ObjectFilter {
    BlobNone,
    BlobLimit(u64),
    Tree(usize),
    Type(String),
    Combine(Vec<ObjectFilter>),
}

impl ObjectFilter {
    fn parse(spec: &str) -> anyhow::Result<ObjectFilter> {
        let bad = || fatal(format!("invalid filter-spec '{spec}'"));
        if spec == "blob:none" {
            return Ok(ObjectFilter::BlobNone);
        }
        if let Some(n) = spec.strip_prefix("blob:limit=") {
            let (digits, unit) =
                n.split_at(n.find(|c: char| !c.is_ascii_digit()).unwrap_or(n.len()));
            let scale = match unit.to_ascii_lowercase().as_str() {
                "" => 1,
                "k" => 1 << 10,
                "m" => 1 << 20,
                "g" => 1 << 30,
                _ => return Err(bad()),
            };
            return Ok(ObjectFilter::BlobLimit(
                digits.parse::<u64>().map_err(|_| bad())? * scale,
            ));
        }
        if let Some(n) = spec.strip_prefix("tree:") {
            return Ok(ObjectFilter::Tree(n.parse().map_err(|_| bad())?));
        }
        if let Some(t) = spec.strip_prefix("object:type=") {
            if !["blob", "tree", "commit", "tag"].contains(&t) {
                return Err(bad());
            }
            return Ok(ObjectFilter::Type(t.to_owned()));
        }
        if let Some(list) = spec.strip_prefix("combine:") {
            return Ok(ObjectFilter::Combine(
                list.split('+')
                    .map(|s| ObjectFilter::parse(&percent_decode(s)))
                    .collect::<anyhow::Result<_>>()?,
            ));
        }
        Err(bad())
    }

    /// Whether commits the walk reaches (not those named) are listed.
    fn keeps_commits(&self) -> bool {
        match self {
            ObjectFilter::Type(t) => t == "commit",
            ObjectFilter::Combine(all) => all.iter().all(ObjectFilter::keeps_commits),
            _ => true,
        }
    }

    /// Whether an object of `kind` and `size` at `depth` (0 for a commit's
    /// tree) is listed.
    fn keeps(&self, kind: &str, size: u64, depth: usize) -> bool {
        match self {
            ObjectFilter::BlobNone => kind != "blob",
            ObjectFilter::BlobLimit(n) => kind != "blob" || size < *n,
            ObjectFilter::Tree(n) => depth < *n,
            ObjectFilter::Type(t) => kind == t,
            ObjectFilter::Combine(all) => all.iter().all(|f| f.keeps(kind, size, depth)),
        }
    }
}

/// The order git's oidset (a khash set) iterates `ids` inserted in this
/// order: bucket order, as klib places and rehashes them.
fn oidset_order(ids: &[String]) -> Vec<String> {
    fn hash(id: &str) -> u32 {
        let b: Vec<u8> = (0..4)
            .map(|i| u8::from_str_radix(id.get(2 * i..2 * i + 2).unwrap_or("0"), 16).unwrap_or(0))
            .collect();
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    }
    // rev-list sizes its sets for 16k objects up front (DEFAULT_OIDSET_SIZE).
    let mut slots: Vec<Option<String>> = vec![None; 16 * 1024];
    let mut size = 0usize;
    let mut upper = (16.0 * 1024.0 * 0.77 + 0.5) as usize;
    let mut seen = std::collections::HashSet::new();
    for id in ids {
        if !seen.insert(id) {
            continue;
        }
        if size >= upper {
            let old = slots.len();
            let n = (old + 1).next_power_of_two().max(4);
            // kh_resize in place: each old key in bucket order goes to its
            // new bucket, kicking out an old key not yet moved from there.
            let mut unmoved: Vec<bool> = slots.iter().map(Option::is_some).collect();
            let mut taken = vec![false; n];
            slots.resize(n, None);
            for j in 0..old {
                if !unmoved[j] {
                    continue;
                }
                unmoved[j] = false;
                let mut key = slots[j].take().unwrap_or_default();
                loop {
                    let mut i = hash(&key) as usize & (n - 1);
                    let mut step = 0;
                    while taken[i] {
                        step += 1;
                        i = (i + step) & (n - 1);
                    }
                    taken[i] = true;
                    if i < old && unmoved[i] {
                        unmoved[i] = false;
                        key = slots[i].replace(key).unwrap_or_default();
                    } else {
                        slots[i] = Some(key);
                        break;
                    }
                }
            }
            upper = (n as f64 * 0.77 + 0.5) as usize;
        }
        let mask = slots.len() - 1;
        let mut i = hash(id) as usize & mask;
        let mut step = 0;
        while slots[i].is_some() {
            step += 1;
            i = (i + step) & mask;
        }
        slots[i] = Some(id.clone());
        size += 1;
    }
    slots.into_iter().flatten().collect()
}

/// What the other worktrees' HEADs name (a ref, or a detached commit id),
/// which git's `--all` walks too.
fn other_worktree_heads(git_dir: &Path) -> Vec<String> {
    let common = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(c) => git_dir.join(c.trim()),
        Err(_) => git_dir.to_path_buf(),
    };
    let mut dirs = vec![common.clone()];
    if let Ok(entries) = std::fs::read_dir(common.join("worktrees")) {
        let mut linked: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        linked.sort();
        dirs.extend(linked);
    }
    let here = git_dir
        .canonicalize()
        .unwrap_or_else(|_| git_dir.to_path_buf());
    dirs.into_iter()
        .filter(|d| d.canonicalize().unwrap_or_else(|_| d.clone()) != here)
        .filter_map(|d| {
            let head = std::fs::read_to_string(d.join("HEAD")).ok()?;
            let head = head.trim();
            Some(head.strip_prefix("ref: ").unwrap_or(head).to_owned())
        })
        .collect()
}

/// git's strbuf_humanise_bytes.
fn humanise_bytes(bytes: u64) -> String {
    if bytes > 1 << 30 {
        format!(
            "{}.{:02} GiB",
            bytes >> 30,
            (bytes & ((1 << 30) - 1)) / 10737419
        )
    } else if bytes > 1 << 20 {
        let x = bytes + 5243;
        format!("{}.{:02} MiB", x >> 20, ((x & ((1 << 20) - 1)) * 100) >> 20)
    } else if bytes > 1 << 10 {
        let x = bytes + 5;
        format!("{}.{:02} KiB", x >> 10, ((x & ((1 << 10) - 1)) * 100) >> 10)
    } else {
        format!("{bytes} {}", if bytes == 1 { "byte" } else { "bytes" })
    }
}

/// `git rev-list`.
fn rev_list(backend: &Arc<dyn GitBackend>, command: Plumbing) -> anyhow::Result<Output> {
    let Plumbing::RevList {
        max_count,
        count,
        mut all,
        reverse,
        first_parent,
        merges,
        no_merges,
        parents,
        skip,
        branches,
        tags,
        remotes,
        abbrev_commit,
        since,
        until,
        author,
        committer,
        grep,
        ignore_case,
        objects,
        objects_edge,
        missing,
        walk,
        more,
        mut revs,
        mut paths,
    } = command
    else {
        unreachable!()
    };
    if more.stdin {
        let mut input = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)?;
        let mut lines = input.lines();
        for line in lines.by_ref() {
            match line {
                "--" => break,
                "" => {}
                l => revs.push(l.to_owned()),
            }
        }
        paths.extend(lines.filter(|l| !l.is_empty()).map(str::to_owned));
    }
    let mut globs = Vec::new();
    for (pattern, base) in [
        (branches, "refs/heads/"),
        (tags, "refs/tags/"),
        (remotes, "refs/remotes/"),
    ] {
        if let Some(p) = pattern {
            let p = if p.is_empty() { "*" } else { p.as_str() };
            let slash = if p.contains(['*', '?', '[']) {
                ""
            } else {
                "/*"
            };
            globs.push((base, format!("{base}{p}{slash}")));
        }
    }
    // --exclude: expand the ref sets here, leaving out what it names (the
    // name below the set's base, or the full name for --all).
    if !more.exclude.is_empty() && (all || !globs.is_empty()) {
        let names: Vec<String> = backend.ref_details()?.into_iter().map(|r| r.name).collect();
        let kept = |base: &str, name: &str| {
            let short = name.strip_prefix(base).unwrap_or(name);
            !more
                .exclude
                .iter()
                .any(|e| glob(e.as_bytes(), short.as_bytes()))
        };
        let mut expanded: Vec<String> = Vec::new();
        if all {
            expanded.extend(names.iter().filter(|n| kept("", n)).cloned());
            expanded.push("HEAD".to_owned());
            expanded.extend(other_worktree_heads(&backend.git_dir()));
        }
        for (base, g) in &globs {
            expanded.extend(
                names
                    .iter()
                    .filter(|n| glob(g.as_bytes(), n.as_bytes()) && kept(base, n))
                    .cloned(),
            );
        }
        revs.splice(0..0, expanded);
        all = false;
        globs.clear();
    }
    if revs.is_empty() && !all && globs.is_empty() && !walk.merge {
        if more.stdin {
            return Ok(lines(if count { "0\n" } else { "" }.to_owned()));
        }
        return Err(CliError::usage("rev-list needs a revision, e.g. HEAD"));
    }
    let missing = missing.as_deref().unwrap_or("error");
    if !["error", "allow-any", "allow-promisor", "print"].contains(&missing) {
        return Err(fatal(format!("invalid argument to --missing: '{missing}'")));
    }
    let filters: Vec<ObjectFilter> = if more.no_filter {
        Vec::new()
    } else {
        more.filter
            .iter()
            .map(|f| ObjectFilter::parse(f))
            .collect::<anyhow::Result<_>>()?
    };
    let objects = objects || objects_edge;
    if !filters.is_empty() && !objects {
        return Err(fatal("object filtering requires --objects"));
    }
    if parents && more.children {
        return Err(fatal(
            "options '--parents' and '--children' cannot be used together",
        ));
    }
    // The commits named on the command line show even when a filter
    // leaves commits out.
    let mut given = std::collections::HashSet::new();
    if !filters.iter().all(ObjectFilter::keeps_commits) {
        for rev in &revs {
            let or_head = |s: &str| if s.is_empty() { "HEAD" } else { s }.to_owned();
            let named: Vec<String> = if rev.starts_with('^') {
                Vec::new()
            } else if let Some((a, b)) = rev.split_once("...") {
                vec![or_head(a), or_head(b)]
            } else if let Some((_, b)) = rev.split_once("..") {
                vec![or_head(b)]
            } else {
                vec![rev.clone()]
            };
            for n in named {
                if let Ok(id) = backend.rev_parse(&format!("{n}^{{commit}}")) {
                    given.insert(id);
                }
            }
        }
        if all || !globs.is_empty() {
            for r in backend.ref_details()? {
                let hit = all
                    || globs
                        .iter()
                        .any(|(_, g)| glob(g.as_bytes(), r.name.as_bytes()));
                if hit && let Ok(id) = backend.rev_parse(&format!("{}^{{commit}}", r.name)) {
                    given.insert(id);
                }
            }
        }
    }
    let mut opts = rgit_git::LogOptions {
        limit: max_count.unwrap_or(usize::MAX),
        offset: skip.unwrap_or(0),
        all,
        revs: revs.clone(),
        paths,
        first_parent,
        merges: (merges || no_merges).then_some(merges),
        reverse,
        globs: globs.into_iter().map(|(_, g)| g).collect(),
        since: since.as_deref().map(crate::cli::parse_date).transpose()?,
        until: until.as_deref().map(crate::cli::parse_date).transpose()?,
        author,
        committer,
        grep,
        grep_ignore_case: ignore_case,
        rewrite_parents: parents || more.children,
        ..Default::default()
    };
    walk.apply(&mut opts);
    if more.bisect || more.bisect_vars || more.bisect_all {
        return rev_list_bisect(backend, &opts, &more);
    }
    opts.boundary |= objects;
    let mut commits = backend.rev_walk(&opts)?;
    if objects && !walk.boundary {
        commits.retain(|c| c.mark != Some('-'));
    }
    // The edges are the excluded parents of the whole range, not only
    // of the commits -n and --skip leave; the children are among them too.
    let whole = if objects || more.children {
        backend.rev_walk(&rgit_git::LogOptions {
            limit: usize::MAX,
            offset: 0,
            boundary: false,
            ..opts.clone()
        })?
    } else {
        Vec::new()
    };
    let mut edges: Vec<String> = Vec::new();
    if objects {
        let ids: std::collections::HashSet<&str> = whole.iter().map(|c| c.id.as_str()).collect();
        for p in whole.iter().flat_map(|c| &c.parents) {
            if !ids.contains(p.as_str()) && !edges.contains(p) {
                edges.push(p.clone());
            }
        }
    }
    if count {
        let (mut left, mut right, mut same) = (0usize, 0usize, 0usize);
        for c in &commits {
            match c.mark {
                Some('=') => same += 1,
                Some('<') => left += 1,
                _ => right += 1,
            }
        }
        let cherry = walk.cherry_mark || walk.cherry;
        let text = match (walk.left_right, cherry) {
            (true, true) => format!("{left}\t{right}\t{same}\n"),
            (true, false) => format!("{left}\t{right}\n"),
            (false, true) => format!("{}\t{same}\n", left + right),
            (false, false) => format!("{}\n", left + right + same),
        };
        return Ok(lines(text).with("count", left + right + same));
    }
    let abbrev = abbrev_commit || more.oneline;
    let id = |id: &str| -> anyhow::Result<String> {
        Ok(if abbrev {
            backend.abbrev_id(id, more.abbrev.unwrap_or(0))?
        } else {
            id.to_owned()
        })
    };
    let mut children: std::collections::HashMap<&str, Vec<&str>> = Default::default();
    if more.children {
        for c in &whole {
            for p in &c.parents {
                children
                    .entry(p.as_str())
                    .or_default()
                    .insert(0, c.id.as_str());
            }
        }
    }
    let spec = more
        .format
        .clone()
        .map(|f| {
            if f.is_empty() {
                "tformat:".to_owned()
            } else {
                f
            }
        })
        .or(more.pretty.clone())
        .or(more.oneline.then(|| "oneline".to_owned()))
        .or(more.header.then(|| "raw".to_owned()));
    let pretty = match &spec {
        Some(spec) => crate::pretty::Pretty::new(
            backend,
            &crate::cli::PrettyArgs {
                pretty: Some(spec.clone()),
                date: more.date.clone(),
                ..Default::default()
            },
            None,
        )?,
        None => None,
    };
    let user = pretty
        .as_ref()
        .is_some_and(|p| matches!(p.fmt, crate::pretty::Fmt::User(_)));
    let oneline = pretty
        .as_ref()
        .is_some_and(|p| matches!(p.fmt, crate::pretty::Fmt::Oneline));
    let formatted = more.format.is_some() || more.pretty.is_some() || more.oneline;
    let header_line = !(user && more.no_commit_header);
    let prefix = if formatted && !oneline && header_line {
        "commit "
    } else {
        ""
    };
    // --header alone ends each commit with a NUL.
    let term = if formatted { '\n' } else { '\0' };
    let mut out = String::new();
    let mut disk = 0u64;
    if objects_edge && more.disk_usage.is_none() {
        for e in &edges {
            out.push_str(&format!("-{e}\n"));
        }
    }
    for c in &commits {
        if !filters.iter().all(ObjectFilter::keeps_commits) && !given.contains(&c.id) {
            continue;
        }
        if more.disk_usage.is_some() {
            disk += backend.object_disk(&c.id)?.0;
            continue;
        }
        if more.timestamp {
            out.push_str(&format!("{} ", c.committer.time));
        }
        out.push_str(prefix);
        if header_line {
            if let Some(m) = walk.mark(c.mark) {
                out.push(m);
            }
            out.push_str(&id(&c.id)?);
        }
        if parents {
            for p in &c.parents {
                out.push(' ');
                out.push_str(p);
            }
        }
        for child in children.get(c.id.as_str()).into_iter().flatten() {
            out.push(' ');
            out.push_str(child);
        }
        let Some(pretty) = &pretty else {
            out.push('\n');
            continue;
        };
        if oneline {
            out.push(' ');
        } else if header_line {
            out.push('\n');
        }
        let mut commit = crate::pretty::parse(&backend.read_object(&c.id)?);
        commit.mark = c.mark;
        let (_, body) = pretty.parts(&commit);
        if !user || !body.is_empty() {
            out.push_str(&body);
            out.push(term);
        }
    }
    if objects {
        let mut listed = Vec::new();
        // Annotated tags the walk starts from come first, by their names.
        let mut tips: Vec<String> = revs
            .iter()
            .filter(|r| !r.starts_with('^') && !r.contains(".."))
            .cloned()
            .collect();
        if all {
            tips.splice(0..0, backend.ref_details()?.into_iter().map(|r| r.name));
        }
        let mut tagged = std::collections::HashSet::new();
        for tip in tips {
            let Ok(obj) = backend.read_object(&tip) else {
                continue;
            };
            if obj.kind != "tag" || !tagged.insert(obj.id.clone()) {
                continue;
            }
            let text = String::from_utf8_lossy(&obj.data);
            let name = text
                .lines()
                .find_map(|l| l.strip_prefix("tag "))
                .unwrap_or_default()
                .to_owned();
            listed.push((obj.id, name));
        }
        let ids: Vec<String> = commits
            .iter()
            .filter(|c| c.mark != Some('-'))
            .map(|c| c.id.clone())
            .collect();
        let (mut absent, mut omitted) = (Vec::new(), Vec::new());
        for (oid, path, gone) in backend.list_objects(&ids, &edges)? {
            if gone {
                match missing {
                    "error" => return Err(fatal(format!("missing object {oid}"))),
                    "print" => absent.push(oid),
                    _ => {}
                }
                continue;
            }
            if !filters.is_empty() {
                let (kind, size) = backend.object_header(&oid)?;
                let depth = if path.is_empty() {
                    0
                } else {
                    path.split('/').count()
                };
                if !filters.iter().all(|f| f.keeps(&kind, size, depth)) {
                    omitted.push(oid);
                    continue;
                }
            }
            listed.push((oid, path));
        }
        for (oid, path) in listed {
            if more.disk_usage.is_some() {
                disk += backend.object_disk(&oid)?.0;
            } else if more.no_object_names && !more.object_names {
                out.push_str(&format!("{oid}\n"));
            } else {
                out.push_str(&format!("{oid} {path}\n"));
            }
        }
        if more.filter_print_omitted && more.disk_usage.is_none() {
            for oid in oidset_order(&omitted) {
                out.push_str(&format!("~{oid}\n"));
            }
        }
        if more.disk_usage.is_none() {
            for oid in oidset_order(&absent) {
                out.push_str(&format!("?{oid}\n"));
            }
        }
    }
    if let Some(style) = &more.disk_usage {
        out = match style.as_str() {
            "" => format!("{disk}\n"),
            "human" => format!("{}\n", humanise_bytes(disk)),
            s => {
                return Err(fatal(format!(
                    "invalid value for '--disk-usage=<format>': '{s}', the only allowed format is 'human'"
                )));
            }
        };
    }
    if more.quiet {
        out.clear();
    }
    Ok(lines(out))
}

/// `rev-list --bisect`, `--bisect-vars` and `--bisect-all`.
fn rev_list_bisect(
    backend: &Arc<dyn GitBackend>,
    opts: &rgit_git::LogOptions,
    more: &crate::cli::RevListArgs,
) -> anyhow::Result<Output> {
    let (mut tips, mut hidden) = (Vec::new(), Vec::new());
    // `--bisect` also walks the bisection's own refs, as in git.
    if more.bisect && backend.rev_parse("refs/bisect/bad").is_ok() {
        tips.push("refs/bisect/bad".to_owned());
        for r in backend.ref_details()? {
            if r.name.starts_with("refs/bisect/good-") {
                hidden.push(r.name);
            }
        }
    }
    for rev in &opts.revs {
        if let Some(h) = rev.strip_prefix('^') {
            hidden.push(h.to_owned());
        } else if let Some((a, b)) = rev.split_once("..") {
            let or_head = |s: &str| if s.is_empty() { "HEAD" } else { s }.to_owned();
            hidden.push(or_head(a));
            tips.push(or_head(b.trim_start_matches('.')));
        } else {
            tips.push(rev.clone());
        }
    }
    let (list, reaches, nr) = backend.rev_list_bisect(
        &tips,
        &hidden,
        opts.first_parent,
        &opts.paths,
        more.bisect_all,
    )?;
    let mut out = String::new();
    if more.bisect_all {
        let decorations = crate::pretty::decorations(backend);
        for (c, dist) in &list {
            let mut labels: Vec<String> = decorations
                .get(c)
                .into_iter()
                .flatten()
                .map(|(_, l)| l.clone())
                .collect();
            labels.push(format!("dist={dist}"));
            out.push_str(&format!("{c} ({})\n", labels.join(", ")));
        }
        if !more.bisect_vars {
            return Ok(lines(out));
        }
        out.push_str("------\n");
    }
    if !more.bisect_vars {
        if let Some((c, _)) = list.first() {
            out.push_str(&format!("{c}\n"));
        }
        return Ok(lines(out));
    }
    if list.is_empty() {
        return Err(fail(true, true, ""));
    }
    let cnt = (nr - reaches).max(reaches);
    let rev = list.first().map(|(c, _)| c.as_str()).unwrap_or("");
    out.push_str(&format!(
        "bisect_rev='{rev}'\nbisect_nr={}\nbisect_good={}\nbisect_bad={}\nbisect_all={nr}\nbisect_steps={}\n",
        cnt - 1,
        nr - reaches - 1,
        reaches - 1,
        rgit_git::bisect_steps(nr)
    ));
    Ok(lines(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_shorten_and_match() {
        assert_eq!(shorten("refs/heads/main"), "main");
        assert_eq!(shorten("refs/remotes/origin/HEAD"), "origin");
        assert_eq!(strip("refs/heads/a/b", "lstrip=2").unwrap(), "a/b");
        assert_eq!(strip("refs/heads/a/b", "lstrip=-1").unwrap(), "b");
        assert!(ref_matches("refs/heads", "refs/heads/main"));
        assert!(!ref_matches("refs/head", "refs/heads/main"));
        assert!(ref_matches("refs/*/ma*", "refs/heads/main"));
        assert_eq!(relative("c.txt", "dir/"), "../c.txt");
        assert_eq!(relative("dir", "dir/"), "./");
        assert_eq!(relative("dir/sub/b", "dir/"), "sub/b");
        assert_eq!(relative("d2/x", "dir/sub/"), "../../d2/x");
    }
}
