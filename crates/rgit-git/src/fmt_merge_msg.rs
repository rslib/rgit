//! `git fmt-merge-msg`: a merge commit's message from FETCH_HEAD-style lines,
//! as git's fmt-merge-msg.c writes it.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use git2::{Oid, Repository, Sort};

use crate::GitError;

const DEFAULT_LOG_LEN: usize = 20;

#[derive(Default)]
struct Source {
    name: String,
    head_status: u8,
    branch: Vec<String>,
    tag: Vec<String>,
    r_branch: Vec<String>,
    generic: Vec<String>,
}

struct Origin {
    name: String,
    oid: Oid,
    local_branch: bool,
}

/// `git fmt-merge-msg`'s options: `-m`, `--log[=<n>]` (`None`: merge.log),
/// `--into-name`.
#[derive(Default)]
pub struct FmtMergeMsgOpts {
    pub message: Option<String>,
    pub log: Option<usize>,
    pub into_name: Option<String>,
}

fn joined(out: &mut String, one: &str, many: &str, list: &[String]) {
    match list {
        [] => {}
        [x] => {
            let _ = write!(out, "{one}{x}");
        }
        [rest @ .., last] => {
            out.push_str(many);
            out.push_str(&rest.join(", "));
            let _ = write!(out, " and {last}");
        }
    }
}

fn comment_lines(out: &mut String, text: &str, comment: &str) {
    for line in text.split_inclusive('\n') {
        out.push_str(comment);
        if !line.starts_with(['\n', '\t']) {
            out.push(' ');
        }
        out.push_str(line);
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
}

fn people_count(out: &mut String, people: &[(String, usize)]) {
    match people {
        [] => {}
        [(a, _)] => out.push_str(a),
        [(a, n), (b, m)] => {
            let _ = write!(out, "{a} ({n}) and {b} ({m})");
        }
        [(a, n), ..] => {
            let _ = write!(out, "{a} ({n}) and others");
        }
    }
}

/// The message git's `fmt-merge-msg` prints for `input`; `Err` carries
/// git's `error in line` message.
pub fn fmt_merge_msg(git_dir: &Path, input: &str, o: &FmtMergeMsgOpts) -> Result<String, GitError> {
    let repo = Repository::open(git_dir)?;
    let config = repo.config()?.snapshot()?;
    let get = |k: &str| config.get_string(k).ok();
    let comment = get("core.commentChar")
        .filter(|c| c != "auto" && !c.is_empty())
        .unwrap_or_else(|| "#".to_owned());
    let mut suppress: Vec<String> = Vec::new();
    let mut seen_suppress = false;
    if let Ok(mut entries) = config.multivar("merge.suppressdest", None) {
        while let Some(Ok(e)) = entries.next() {
            seen_suppress = true;
            match e.value().ok() {
                Some("") | None => suppress.clear(),
                Some(v) => suppress.push(v.to_owned()),
            }
        }
    }
    if !seen_suppress {
        suppress = vec!["main".to_owned(), "master".to_owned()];
    }
    let log_config = ["merge.log", "merge.summary"]
        .iter()
        .find_map(|k| {
            config
                .get_i64(k)
                .ok()
                .map(|n| n.max(0) as usize)
                .or_else(|| {
                    config
                        .get_bool(k)
                        .ok()
                        .map(|b| if b { DEFAULT_LOG_LEN } else { 0 })
                })
        })
        .unwrap_or(0);
    let limit = o.log.unwrap_or(log_config);
    let branch_desc = config.get_bool("merge.branchdesc").unwrap_or(false);

    let head_ref = repo.find_reference("HEAD")?;
    let head_name = head_ref.symbolic_target().ok().flatten().map(str::to_owned);
    let head = repo
        .refname_to_id("HEAD")
        .map_err(|_| GitError::Other("No current branch".to_owned()))?;
    let current = o.into_name.clone().unwrap_or_else(|| match &head_name {
        Some(n) => n.strip_prefix("refs/heads/").unwrap_or(n).to_owned(),
        None => "HEAD".to_owned(),
    });

    // Merge parents: those not already reached from HEAD or another parent.
    let mut given: Vec<(Oid, Oid)> = Vec::new();
    for line in input.split('\n') {
        let Some((hex, rest)) = line.split_at_checked(40) else {
            continue;
        };
        let Ok(oid) = Oid::from_str(hex) else {
            continue;
        };
        if !rest.starts_with("\t\t") || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        if let Ok(c) = repo.find_object(oid, None).and_then(|x| x.peel_to_commit()) {
            given.push((oid, c.id()));
        }
    }
    let mut heads: Vec<Oid> = given.iter().map(|g| g.1).collect();
    heads.push(head);
    heads.sort();
    heads.dedup();
    let reduced: Vec<Oid> = heads
        .iter()
        .copied()
        .filter(|&c| {
            !heads
                .iter()
                .any(|&d| d != c && repo.graph_descendant_of(d, c).unwrap_or(false))
        })
        .collect();
    let used: Vec<Oid> = given
        .iter()
        .filter(|g| reduced.contains(&g.1))
        .map(|g| g.0)
        .collect();

    let mut srcs: Vec<Source> = Vec::new();
    let mut origins: Vec<Origin> = Vec::new();
    let body = input.strip_suffix('\n').unwrap_or(input);
    for (n, line) in body.split('\n').enumerate() {
        let bad = || GitError::Cli(format!("fatal: error in line {}: {line}", n + 1));
        let b = line.as_bytes();
        if b.len() < 43 || b[40] != b'\t' {
            return Err(bad());
        }
        if line[41..].starts_with("not-for-merge") {
            continue;
        }
        if b[41] != b'\t' {
            return Err(bad());
        }
        let oid = Oid::from_str(&line[..40]).map_err(|_| bad())?;
        if !used.contains(&oid) {
            continue;
        }
        let desc = &line[42..];
        let (what, src, pulling_head) = match desc.find(" of ") {
            Some(i) => (&desc[..i], &desc[i + 4..], false),
            None => (desc, desc, true),
        };
        let si = match srcs.iter().position(|s| s.name == src) {
            Some(i) => i,
            None => {
                srcs.push(Source {
                    name: src.to_owned(),
                    ..Default::default()
                });
                srcs.len() - 1
            }
        };
        let s = &mut srcs[si];
        let mut local_branch = false;
        let origin = if pulling_head {
            s.head_status |= 1;
            src
        } else if let Some(b) = what.strip_prefix("branch ") {
            local_branch = true;
            s.branch.push(b.to_owned());
            s.head_status |= 2;
            b
        } else if let Some(t) = what.strip_prefix("tag ") {
            s.tag.push(t.to_owned());
            s.head_status |= 2;
            what
        } else if let Some(r) = what.strip_prefix("remote-tracking branch ") {
            s.r_branch.push(r.to_owned());
            s.head_status |= 2;
            r
        } else {
            s.generic.push(what.to_owned());
            s.head_status |= 2;
            src
        };
        let name = if src == "." || src == origin {
            match origin.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')) {
                Some(inner) => inner.to_owned(),
                None => origin.to_owned(),
            }
        } else {
            format!("{origin} of {src}")
        };
        origins.push(Origin {
            name,
            oid,
            local_branch: local_branch && src == ".",
        });
    }

    let mut out = o.message.clone().unwrap_or_default();
    if o.message.is_none() && !srcs.is_empty() {
        out.push_str("Merge ");
        for (i, s) in srcs.iter().enumerate() {
            if i > 0 {
                out.push_str("; ");
            }
            if s.head_status == 1 {
                out.push_str(&s.name);
                continue;
            }
            let mut sep = "";
            if s.head_status == 3 {
                sep = ", ";
                out.push_str("HEAD");
            }
            for (list, one, many) in [
                (&s.branch, "branch ", "branches "),
                (
                    &s.r_branch,
                    "remote-tracking branch ",
                    "remote-tracking branches ",
                ),
                (&s.tag, "tag ", "tags "),
                (&s.generic, "commit ", "commits "),
            ] {
                if !list.is_empty() {
                    out.push_str(sep);
                    sep = ", ";
                    joined(&mut out, one, many, list);
                }
            }
            if s.name != "." {
                let _ = write!(out, " of {}", s.name);
            }
        }
        let suppressed = suppress
            .iter()
            .any(|p| crate::apply::wildmatch(p, &current));
        if !suppressed {
            let _ = write!(out, " into {current}");
        }
        out.push('\n');
    }

    // Annotated tags' messages (signatures are not checked).
    let mut tags = String::new();
    let mut first: Option<usize> = None;
    let mut count = 0;
    for (i, origin) in origins.iter().enumerate() {
        if repo.find_tag(origin.oid).is_err() {
            continue;
        }
        let odb = repo.odb()?;
        let raw = odb.read(origin.oid)?;
        let text = String::from_utf8_lossy(raw.data()).into_owned();
        // ponytail: a signed tag's signature is dropped, not verified with gpg.
        let payload = match text.find("-----BEGIN ") {
            Some(i) => text[..i].to_owned(),
            None => text,
        };
        count += 1;
        if count == 2
            && let Some(f) = first
        {
            let mut head = String::from("\n");
            comment_lines(&mut head, &origins[f].name, &comment);
            tags.insert_str(0, &head);
        }
        if count == 1 {
            first = Some(i);
        } else {
            tags.push('\n');
            comment_lines(&mut tags, &origin.name, &comment);
        }
        if let Some(i) = payload.find("\n\n") {
            tags.push_str(&payload[i + 2..]);
        }
        if !tags.is_empty() && !tags.ends_with('\n') {
            tags.push('\n');
        }
    }
    if !tags.is_empty() {
        out.push('\n');
        out.push_str(&tags);
    }

    if limit > 0 {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        let me = |committer: bool| {
            crate::plumbing::ident(&repo, committer)
                .ok()
                .and_then(|i| i.rsplitn(3, ' ').nth(2).map(str::to_owned))
        };
        for origin in &origins {
            let Ok(tip) = repo
                .find_object(origin.oid, None)
                .and_then(|x| x.peel_to_commit())
            else {
                continue;
            };
            let mut walk = repo.revwalk()?;
            walk.set_sorting(Sort::TIME | Sort::TOPOLOGICAL)?;
            walk.push(tip.id())?;
            walk.hide(head)?;
            let mut subjects = Vec::new();
            let mut authors: BTreeMap<String, usize> = BTreeMap::new();
            let mut committers: BTreeMap<String, usize> = BTreeMap::new();
            let mut n = 0;
            let person = |sig: git2::Signature| sig.name().unwrap_or("").trim().to_owned();
            for id in walk {
                let c = repo.find_commit(id?)?;
                if c.parent_count() > 1 {
                    *committers.entry(person(c.committer())).or_default() += 1;
                    continue;
                }
                if n == 0 {
                    *committers.entry(person(c.committer())).or_default() += 1;
                }
                *authors.entry(person(c.author())).or_default() += 1;
                n += 1;
                if subjects.len() > limit {
                    continue;
                }
                let msg = String::from_utf8_lossy(c.message_bytes()).into_owned();
                let subject = msg
                    .trim_start()
                    .split("\n\n")
                    .next()
                    .unwrap_or("")
                    .lines()
                    .map(str::trim)
                    .collect::<Vec<_>>()
                    .join(" ");
                subjects.push(if subject.is_empty() {
                    c.id().to_string()
                } else {
                    subject
                });
            }
            for (people, label, committer) in [(authors, "By", false), (committers, "Via", true)] {
                let mut list: Vec<(String, usize)> = people.into_iter().collect();
                list.sort_by_key(|p| std::cmp::Reverse(p.1));
                let mine = list.len() == 1
                    && me(committer).is_some_and(|m| {
                        m.strip_prefix(&list[0].0)
                            .is_some_and(|r| r.starts_with(" <"))
                    });
                if list.is_empty() || mine {
                    continue;
                }
                let _ = write!(out, "\n{comment} {label} ");
                people_count(&mut out, &list);
            }
            if n > limit {
                let _ = write!(out, "\n* {}: ({n} commits)\n", origin.name);
            } else {
                let _ = write!(out, "\n* {}:\n", origin.name);
            }
            if origin.local_branch && branch_desc {
                let branch = origin.name.clone();
                if let Some(desc) = get(&format!("branch.{branch}.description")) {
                    for l in desc.split_inclusive('\n') {
                        let _ = write!(out, "  : {l}");
                    }
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                }
            }
            for (i, s) in subjects.iter().enumerate() {
                if i >= limit {
                    out.push_str("  ...\n");
                } else {
                    let _ = writeln!(out, "  {s}");
                }
            }
        }
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}
