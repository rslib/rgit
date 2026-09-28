//! `git format-patch`: commits as mbox emails, written as git writes them.

use crate::rev::RevParse;
use std::fmt::Write as _;

use git2::{Commit, Diff, DiffFindOptions, DiffOptions, Oid, Repository};

use crate::GitError;
use crate::git_repo::rfc2822_date;

/// How `--thread` links the mails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thread {
    /// Every mail replies to the first (or the cover letter).
    Shallow,
    /// Every mail replies to the one before.
    Deep,
}

/// `git format-patch` options.
#[derive(Debug, Clone, Default)]
pub struct FormatPatchOpts {
    /// `<a>..<b>`, or a base revision for the commits after it up to HEAD.
    pub range: Option<String>,
    /// The newest `count` commits (ending at `range` when given).
    pub count: Option<usize>,
    /// `-n` (true) or `-N` (false); unset numbers more than one patch.
    pub numbered: Option<bool>,
    pub subject_prefix: Option<String>,
    /// `-v <n>`: `[PATCH v<n>]` and a `v<n>-` file prefix.
    pub reroll: Option<String>,
    /// `--rfc[=<text>]`: the text before the prefix (`-text` goes after it).
    pub rfc: Option<String>,
    /// `-k`: no `[PATCH]` prefix.
    pub keep_subject: bool,
    pub cover_letter: bool,
    pub thread: Option<Thread>,
    pub in_reply_to: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    /// Extra header lines (`--add-header`).
    pub headers: Vec<String>,
    /// `--base`: a revision, or `auto` for the upstream's merge base.
    pub base: Option<String>,
    /// `From 0000...` instead of the commit id.
    pub zero_commit: bool,
    pub start_number: Option<usize>,
    /// The signature; `None` is git's version, an empty one none at all.
    pub signature: Option<String>,
    /// `-p`: no diffstat.
    pub no_stat: bool,
    /// Name files `1`, `2`, ... without a suffix.
    pub numbered_files: bool,
    pub suffix: Option<String>,
    /// `--root`: a single revision means every commit up to it.
    pub root: bool,
    /// Add a Signed-off-by trailer for the committer.
    pub signoff: bool,
    /// `--from[=<ident>]`: send as this ident (the committer's when empty),
    /// keeping the author in the body.
    pub from: Option<String>,
    /// Longest file name, suffix included (default 64).
    pub filename_max_length: Option<usize>,
    /// Show binary files as `Binary files ... differ`.
    pub no_binary: bool,
    /// Leave out commits whose change upstream already has.
    pub ignore_if_in_upstream: bool,
    /// How the cover letter uses the branch description: message (default),
    /// subject, auto or none.
    pub cover_from_description: Option<String>,
    /// A cover letter description instead of the branch's.
    pub description: Option<String>,
    /// The previous version for an interdiff in the cover letter.
    pub interdiff: Option<String>,
    /// The previous version for a range-diff in the cover letter.
    pub range_diff: Option<String>,
    /// Percent for pairing in `range_diff` (default 60).
    pub creation_factor: Option<usize>,
    /// `--attach`/`--inline`: the MIME boundary after git's dashes; the
    /// patch goes in its own part.
    pub attach: Option<String>,
    /// With `attach`, an inline part instead of an attachment.
    pub inline: bool,
    /// The notes refs whose notes go after the `---` line.
    pub notes: Vec<String>,
    /// `--no-encode-email-headers`: raw UTF-8 in From: and Subject:.
    pub no_encode_headers: bool,
    /// With `from`, keep the author's From: in the body even when it is the
    /// sender.
    pub force_in_body_from: bool,
    /// format.coverLetter=auto: a cover letter for more than one patch.
    pub cover_letter_auto: bool,
    /// `--no-to`/`--no-cc`: drop format.to/format.cc.
    pub no_to: bool,
    pub no_cc: bool,
}

/// One email: its file name and text.
#[derive(Debug, Clone)]
pub struct PatchMail {
    pub name: String,
    pub text: String,
    pub cover: bool,
}

/// The mails as `--stdout` prints them: git puts a blank line between
/// consecutive patches.
pub fn mbox(mails: &[PatchMail]) -> String {
    let mut out = String::new();
    for (i, m) in mails.iter().enumerate() {
        if i > 0 && !mails[i - 1].cover {
            out.push('\n');
        }
        out.push_str(&m.text);
    }
    out
}

/// The commits `opts` select, oldest first. Merges are skipped, as in git.
fn select<'r>(repo: &'r Repository, o: &FormatPatchOpts) -> Result<Vec<Commit<'r>>, GitError> {
    let mut walk = repo.revwalk()?;
    walk.set_sorting(git2::Sort::TOPOLOGICAL)?;
    match o.range.as_deref() {
        Some(r) if r.contains("..") => walk.push_range(r)?,
        Some(r) if o.count.is_some() || o.root => {
            walk.push(repo.rev_single(r)?.peel_to_commit()?.id())?
        }
        Some(r) => {
            walk.push_head()?;
            walk.hide(repo.rev_single(r)?.peel_to_commit()?.id())?;
        }
        None => walk.push_head()?,
    }
    let mut commits = Vec::new();
    for oid in walk {
        if o.count.is_some_and(|n| commits.len() >= n) {
            break;
        }
        let commit = repo.find_commit(oid?)?;
        if commit.parent_count() <= 1 {
            commits.push(commit);
        }
    }
    commits.reverse();
    Ok(commits)
}

/// Format the selected commits as emails, oldest first (the cover letter,
/// if asked for, comes first).
pub fn format_patch(repo: &Repository, o: &FormatPatchOpts) -> Result<Vec<PatchMail>, GitError> {
    let mut commits = select(repo, o)?;
    if o.ignore_if_in_upstream
        && let Some(upstream) = upstream_side(repo, o)?
    {
        let ids = upstream
            .iter()
            .map(|c| patch_id(repo, c))
            .collect::<Result<std::collections::HashSet<_>, _>>()?;
        let mut kept = Vec::new();
        for c in commits {
            if !ids.contains(&patch_id(repo, &c)?) {
                kept.push(c);
            }
        }
        commits = kept;
    }
    if commits.is_empty() {
        return Ok(Vec::new());
    }
    let config = repo.config()?.snapshot()?;
    let cfg = |k: &str| config.get_string(k).ok();
    let cfg_all = |k: &str| {
        let mut v = Vec::new();
        if let Ok(entries) = config.multivar(k, None) {
            let _ =
                entries.for_each(|e| v.push(String::from_utf8_lossy(e.value_bytes()).into_owned()));
        }
        v
    };
    let mut prefix = o
        .subject_prefix
        .clone()
        .or_else(|| cfg("format.subjectPrefix"))
        .unwrap_or_else(|| "PATCH".to_owned());
    if let Some(rfc) = o.rfc.as_deref().filter(|r| !r.is_empty()) {
        prefix = match rfc.strip_prefix('-') {
            Some(after) => format!("{prefix} {after}"),
            None => format!("{rfc} {prefix}"),
        };
    }
    if let Some(v) = &o.reroll {
        prefix = format!("{prefix} v{v}");
    }
    let cover_letter = o.cover_letter || (o.cover_letter_auto && commits.len() > 1);
    let encode = !o.no_encode_headers;
    let attach = (o.attach.as_deref()).map(|b| {
        if b.is_empty() {
            git_version()
        } else {
            b.to_owned()
        }
    });
    let start = o.start_number.unwrap_or(1);
    let last = start + commits.len() - 1;
    let numbered = o.numbered.unwrap_or(commits.len() > 1 || cover_letter);
    let tag = |n: usize| {
        if o.keep_subject {
            String::new()
        } else if numbered {
            format!("[{prefix} {n}/{last}] ")
        } else {
            format!("[{prefix}] ")
        }
    };
    let signature = o
        .signature
        .clone()
        .or_else(|| cfg("format.signature"))
        .unwrap_or_else(git_version);
    let suffix = o
        .suffix
        .clone()
        .or_else(|| cfg("format.suffix"))
        .unwrap_or_else(|| ".patch".to_owned());
    let name = |n: usize, subject: &str| {
        if o.numbered_files {
            return n.to_string();
        }
        let mut name = o.reroll.as_ref().map_or(String::new(), |v| {
            format!("{}-", sanitize(&format!("v{v}")))
        });
        let _ = write!(name, "{n:04}-{}", sanitize(subject));
        let max = o.filename_max_length.unwrap_or(64);
        name.truncate(max.saturating_sub(suffix.len() + 1));
        name + &suffix
    };
    let mut extra = String::new();
    // Like git's add_header, To: and Cc: headers join those lists.
    let (mut to, mut cc) = (Vec::new(), Vec::new());
    for h in cfg_all("format.headers").iter().chain(&o.headers) {
        let h = h.trim_end_matches('\n');
        match h.get(..4).map(str::to_ascii_lowercase).as_deref() {
            Some("to: ") => to.push(h[4..].to_owned()),
            Some("cc: ") => cc.push(h[4..].to_owned()),
            _ => {
                let _ = writeln!(extra, "{h}");
            }
        }
    }
    let config_or_none = |no: bool, key: &str| if no { Vec::new() } else { cfg_all(key) };
    for (field, list) in [
        (
            "To",
            [config_or_none(o.no_to, "format.to"), o.to.clone(), to].concat(),
        ),
        (
            "Cc",
            [config_or_none(o.no_cc, "format.cc"), o.cc.clone(), cc].concat(),
        ),
    ] {
        for (i, addr) in list.iter().enumerate() {
            let sep = if i + 1 < list.len() { "," } else { "" };
            if i == 0 {
                let _ = writeln!(extra, "{field}: {addr}{sep}");
            } else {
                let _ = writeln!(extra, "    {addr}{sep}");
            }
        }
    }
    let me = crate::git_repo::ident_signature(repo, true)?;
    let me_ident = format!(
        "{} <{}>",
        String::from_utf8_lossy(me.name_bytes()),
        String::from_utf8_lossy(me.email_bytes())
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let email = String::from_utf8_lossy(me.email_bytes()).into_owned();
    let msg_id = |base: &str| format!("{base}.{now}.git.{email}");
    let bases = match &o.base {
        Some(b) => Some(base_info(repo, b, &commits[0])?),
        None => None,
    };
    let sig = if signature.is_empty() {
        String::new()
    } else {
        let end = if signature.ends_with('\n') { "" } else { "\n" };
        format!("-- \n{signature}{end}\n")
    };
    let from_line = |id: Oid| {
        let id = if o.zero_commit { Oid::ZERO_SHA1 } else { id };
        format!("From {id} Mon Sep 17 00:00:00 2001\n")
    };
    let threading = |message_id: Option<&str>, refs: &[String]| {
        let mut s = String::new();
        if let Some(id) = message_id {
            let _ = writeln!(s, "Message-ID: <{id}>");
        }
        if let Some(last) = refs.last() {
            let _ = writeln!(s, "In-Reply-To: <{last}>");
            for (i, r) in refs.iter().enumerate() {
                let _ = writeln!(s, "{}<{r}>", if i == 0 { "References: " } else { "\t" });
            }
        }
        s
    };

    let mut mails = Vec::new();
    let mut refs: Vec<String> = o
        .in_reply_to
        .iter()
        .map(|r| {
            r.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_owned()
        })
        .collect();
    let mut message_id = None;
    if cover_letter {
        if o.thread.is_some() {
            message_id = Some(msg_id("cover"));
        }
        let tip = commits.last().expect("commits");
        let mut text = from_line(tip.id());
        text.push_str(&threading(message_id.as_deref(), &refs));
        text.push_str(&from_header(&me, encode));
        let _ = writeln!(text, "Date: {}", rfc2822_date(me.when()));
        let subject_tag = if o.keep_subject {
            String::new()
        } else {
            format!("[{prefix} 0/{last}] ")
        };
        let (cover_subject, blurb) = cover_text(repo, o, &commits);
        let head = format!("Subject: {subject_tag}");
        text.push_str(&head);
        text.push_str(&encode_subject(
            &cover_subject,
            head.chars().count(),
            encode,
        ));
        text.push('\n');
        if commits
            .iter()
            .any(|c| !c.raw_header_bytes().is_ascii() || !c.message_bytes().is_ascii())
        {
            text.push_str(MIME);
        }
        text.push_str(&extra);
        let _ = write!(text, "\n{blurb}\n\n");
        text.push_str(&shortlog(&commits));
        let first = &commits[0];
        let from = match first.parent_count() {
            0 => None,
            _ => Some(first.parent(0)?.tree()?),
        };
        let diff = tree_diff(repo, from.as_ref(), &tip.tree()?)?;
        text.push_str(&diffstat(&diff, 72)?);
        text.push_str(&summary(&diff)?);
        text.push('\n');
        text.push_str(&versions(repo, o, &commits)?);
        if let Some(b) = &bases {
            text.push_str(b);
        }
        text.push_str(&sig);
        mails.push(PatchMail {
            name: name(0, "cover-letter"),
            text,
            cover: true,
        });
    }
    for (i, c) in commits.iter().enumerate() {
        let n = start + i;
        if let Some(thread) = o.thread {
            if let Some(prev) = message_id.take() {
                let shallow = thread == Thread::Shallow;
                // Shallow threads keep replying to the root once there is one.
                let to_root = shallow && !refs.is_empty() && (!cover_letter || i > 0);
                if !to_root {
                    refs.push(prev);
                }
            }
            message_id = Some(msg_id(&c.id().to_string()));
        }
        let subject = c.summary()?.unwrap_or("").to_owned();
        let mut text = from_line(c.id());
        text.push_str(&threading(message_id.as_deref(), &refs));
        let author = c.author();
        let author_ident = format!(
            "{} <{}>",
            String::from_utf8_lossy(author.name_bytes()),
            String::from_utf8_lossy(author.email_bytes())
        );
        let sender = o.from.as_ref().map(|f| {
            if f.is_empty() {
                me_ident.clone()
            } else {
                f.clone()
            }
        });
        match &sender {
            Some(ident) => {
                let (name, email) = ident
                    .rsplit_once(" <")
                    .map_or((ident.as_str(), ""), |(n, e)| (n, e.trim_end_matches('>')));
                text.push_str(&from_header(&git2::Signature::now(name, email)?, encode));
            }
            None => text.push_str(&from_header(&author, encode)),
        }
        let _ = writeln!(text, "Date: {}", rfc2822_date(author.when()));
        let head = format!("Subject: {}", tag(n));
        text.push_str(&head);
        text.push_str(&encode_subject(&subject, head.chars().count(), encode));
        text.push('\n');
        let in_body_from = sender
            .as_ref()
            .is_some_and(|s| o.force_in_body_from || *s != author_ident);
        let file_name = name(n, &subject);
        match &attach {
            Some(b) => {
                text.push_str(&extra);
                let _ = write!(
                    text,
                    "MIME-Version: 1.0\nContent-Type: multipart/mixed; boundary=\"{BOUNDARY}{b}\"\n\nThis is a multi-part message in MIME format.\n--{BOUNDARY}{b}\nContent-Type: text/plain; charset=UTF-8; format=fixed\nContent-Transfer-Encoding: 8bit\n\n"
                );
                // git trims the blank lines ending an empty message.
                if body_of(c).is_empty() && !in_body_from {
                    text.pop();
                }
            }
            None => {
                if !c.message_bytes().is_ascii() {
                    text.push_str(MIME);
                }
                text.push_str(&extra);
            }
        }
        text.push('\n');
        if in_body_from {
            let _ = write!(text, "From: {author_ident}\n\n");
        }
        let mut body = body_of(c);
        if o.signoff {
            body = add_signoff(&body, &me_ident);
        }
        text.push_str(&body);
        let parent = match c.parent_count() {
            0 => None,
            _ => Some(c.parent(0)?.tree()?),
        };
        let diff = tree_diff_opts(repo, parent.as_ref(), &c.tree()?, !o.no_binary)?;
        let notes = notes_block(repo, &o.notes, c.id());
        if !notes.is_empty() {
            let _ = write!(text, "---\n{notes}");
        }
        if diff.deltas().len() == 0 {
            // git shows an empty commit with no separator or stat.
        } else if o.no_stat {
            text.push('\n');
        } else {
            if notes.is_empty() {
                text.push_str("---\n");
            } else {
                text.push('\n');
            }
            text.push_str(&diffstat(&diff, 72)?);
            text.push_str(&summary(&diff)?);
            text.push('\n');
            if let Some(b) = &attach {
                let disposition = if o.inline { "inline" } else { "attachment" };
                let _ = write!(
                    text,
                    "\n--{BOUNDARY}{b}\nContent-Type: text/x-patch; name=\"{file_name}\"\nContent-Transfer-Encoding: 8bit\nContent-Disposition: {disposition}; filename=\"{file_name}\"\n\n"
                );
            }
        }
        text.push_str(&patch_text(&diff)?);
        if !cover_letter && commits.len() == 1 {
            let v = versions(repo, o, &commits)?;
            if !v.is_empty() {
                text.push('\n');
                text.push_str(&v);
            }
        }
        if i == 0
            && !cover_letter
            && let Some(b) = &bases
        {
            text.push_str(b);
        }
        match &attach {
            // git leaves the signature out of attached patches.
            Some(b) => {
                let _ = write!(text, "\n--{BOUNDARY}{b}--\n\n\n");
            }
            None => text.push_str(&sig),
        }
        mails.push(PatchMail {
            name: file_name,
            text,
            cover: false,
        });
    }
    Ok(mails)
}

/// The other side of the range for `--ignore-if-in-upstream`: the commits
/// in `b..a` for `a..b`, or `HEAD..<since>`.
fn upstream_side<'r>(
    repo: &'r Repository,
    o: &FormatPatchOpts,
) -> Result<Option<Vec<Commit<'r>>>, GitError> {
    let (ours, theirs) = match o.range.as_deref() {
        Some(r) if r.contains("..") => {
            let (a, b) = r.split_once("..").unwrap_or_default();
            (if b.is_empty() { "HEAD" } else { b }, a)
        }
        Some(r) if o.count.is_none() && !o.root => ("HEAD", r),
        _ => return Ok(None),
    };
    let mut walk = repo.revwalk()?;
    walk.push(repo.rev_single(theirs)?.peel_to_commit()?.id())?;
    walk.hide(repo.rev_single(ours)?.peel_to_commit()?.id())?;
    let mut out = Vec::new();
    for id in walk {
        let c = repo.find_commit(id?)?;
        if c.parent_count() <= 1 {
            out.push(c);
        }
    }
    Ok(Some(out))
}

/// `body` with a `Signed-off-by:` for `ident`, as git adds one: on the
/// trailer block if the message ends in one, else after a blank line.
fn add_signoff(body: &str, ident: &str) -> String {
    let sob = format!("Signed-off-by: {ident}");
    let trimmed = body.trim_end();
    let last = trimmed.rsplit("\n\n").next().unwrap_or("");
    let is_trailer = |l: &str| {
        l.split_once(": ").is_some_and(|(k, _)| {
            !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
    };
    if trimmed.lines().last() == Some(sob.as_str()) {
        return body.to_owned();
    }
    if trimmed.is_empty() {
        format!("{sob}\n")
    } else if last
        .lines()
        .all(|l| is_trailer(l) || l.starts_with([' ', '\t']))
    {
        format!("{trimmed}\n{sob}\n")
    } else {
        format!("{trimmed}\n\n{sob}\n")
    }
}

/// The cover letter's subject and blurb: git's placeholders, or taken from
/// the branch description as `--cover-from-description` says.
fn cover_text(repo: &Repository, o: &FormatPatchOpts, commits: &[Commit]) -> (String, String) {
    let placeholder = (
        "*** SUBJECT HERE ***".to_owned(),
        "*** BLURB HERE ***".to_owned(),
    );
    let mode = o
        .cover_from_description
        .clone()
        .or_else(|| {
            repo.config()
                .ok()?
                .get_string("format.coverFromDescription")
                .ok()
        })
        .unwrap_or_else(|| "message".to_owned());
    let description = o.description.clone().or_else(|| {
        // The branch whose tip is the series' last commit.
        let tip = commits.last()?.id();
        let head = repo.head().ok()?;
        let branch = match o.range.as_deref().and_then(|r| r.split_once("..")) {
            Some((_, b)) if !b.is_empty() => format!("refs/heads/{b}"),
            _ if head.is_branch() => head.name().ok()?.to_owned(),
            _ => return None,
        };
        let r = repo.find_reference(&branch).ok()?;
        if r.peel_to_commit().ok()?.id() != tip {
            return None;
        }
        let short = branch.strip_prefix("refs/heads/")?;
        repo.config()
            .ok()?
            .get_string(&format!("branch.{short}.description"))
            .ok()
    });
    let Some(desc) = description.filter(|d| !d.trim().is_empty() && mode != "none") else {
        return placeholder;
    };
    let desc = desc.trim().to_owned();
    let (first, rest) = desc.split_once("\n\n").unwrap_or((&desc, ""));
    let as_subject = match mode.as_str() {
        "subject" => true,
        "auto" => first.len() < 100,
        _ => false,
    };
    if as_subject {
        let rest = rest.trim();
        (
            first.replace('\n', " "),
            if rest.is_empty() {
                placeholder.1
            } else {
                rest.to_owned()
            },
        )
    } else {
        (placeholder.0, desc)
    }
}

/// The `--interdiff`/`--range-diff` sections against the previous version.
fn versions(
    repo: &Repository,
    o: &FormatPatchOpts,
    commits: &[Commit],
) -> Result<String, GitError> {
    let mut out = String::new();
    let against = match o.reroll.as_deref().and_then(|v| v.parse::<u32>().ok()) {
        Some(n) if n > 1 => format!(" against v{}", n - 1),
        _ => String::new(),
    };
    let (Some(first), Some(tip)) = (commits.first(), commits.last()) else {
        return Ok(out);
    };
    let base = if first.parent_count() > 0 {
        first.parent_id(0)?.to_string()
    } else {
        String::new()
    };
    if let Some(prev) = &o.interdiff {
        let old = repo.rev_single(prev)?.peel_to_tree()?;
        let diff = tree_diff_opts(repo, Some(&old), &tip.tree()?, false)?;
        let _ = write!(out, "Interdiff{against}:\n{}", patch_text(&diff)?);
    }
    if let Some(prev) = &o.range_diff {
        let old = if prev.contains("..") {
            prev.clone()
        } else {
            let p = repo.rev_single(prev)?.peel_to_commit()?.id();
            let b = repo.merge_base(p, Oid::from_str(&base).unwrap_or(p))?;
            format!("{b}..{p}")
        };
        let text = crate::range_diff::range_diff(
            repo,
            &crate::RangeDiffOpts {
                range1: old,
                range2: format!("{base}..{}", tip.id()),
                // Like git, pair a new version of the same series readily.
                creation_factor: o.creation_factor.unwrap_or(999),
                patches: true,
                left_only: false,
                right_only: false,
                ..Default::default()
            },
        )?;
        let _ = write!(out, "Range-diff{against}:\n{text}");
    }
    Ok(out)
}

/// The dashes git puts before a MIME boundary.
const BOUNDARY: &str = "------------";

/// The notes of `id` in `refs` as git shows them after `---`: a
/// `Notes (<ref>):` header each and the lines indented.
fn notes_block(repo: &Repository, refs: &[String], id: Oid) -> String {
    let mut out = String::new();
    for r in refs {
        let Ok(note) = repo.find_note(Some(r), id) else {
            continue;
        };
        if r == "refs/notes/commits" {
            out.push_str("\nNotes:\n");
        } else {
            let short = r.strip_prefix("refs/").unwrap_or(r);
            let short = short.strip_prefix("notes/").unwrap_or(short);
            let _ = write!(out, "\nNotes ({short}):\n");
        }
        let text = String::from_utf8_lossy(note.message_bytes()).into_owned();
        for line in text.strip_suffix('\n').unwrap_or(&text).split('\n') {
            let _ = writeln!(out, "    {line}");
        }
    }
    out
}

const MIME: &str =
    "MIME-Version: 1.0\nContent-Type: text/plain; charset=UTF-8\nContent-Transfer-Encoding: 8bit\n";

/// The installed git's version for the signature, as git signs, else rgit's.
fn git_version() -> String {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .strip_prefix("git version ")
                .map(str::to_owned)
        })
        .unwrap_or_else(|| format!("rgit {}", env!("CARGO_PKG_VERSION")))
}

/// `--base`'s trailer: the base commit and the patch ids of the commits
/// between it and the first patch.
fn base_info(repo: &Repository, base: &str, first: &Commit) -> Result<String, GitError> {
    let base = if base == "auto" {
        let head = repo.head()?;
        let branch = git2::Branch::wrap(head);
        let upstream = branch.upstream().map_err(|_| {
            GitError::Other(
                "--base=auto needs an upstream; set one or give --base=<commit>".to_owned(),
            )
        })?;
        let target = upstream.get().peel_to_commit()?.id();
        repo.merge_base(target, first.id())?
    } else {
        repo.rev_single(base)?.peel_to_commit()?.id()
    };
    let mut out = format!("\nbase-commit: {base}\n");
    if first.parent_count() > 0 {
        let mut walk = repo.revwalk()?;
        walk.push(first.parent_id(0)?)?;
        walk.hide(base)?;
        for oid in walk {
            let c = repo.find_commit(oid?)?;
            if c.parent_count() != 1 {
                continue;
            }
            let _ = writeln!(out, "prerequisite-patch-id: {}", patch_id(repo, &c)?);
        }
    }
    Ok(out)
}

/// git's patch id of a commit's change (diff.c's `diff_get_patch_id`): each
/// file's header and lines, whitespace removed, hashed on its own and summed,
/// so file order does not matter.
pub(crate) fn patch_id(repo: &Repository, c: &Commit) -> Result<Oid, GitError> {
    patch_id_in(repo, c, &[])
}

/// [`patch_id`] of the commit's diff limited to `paths`.
pub(crate) fn patch_id_in(
    repo: &Repository,
    c: &Commit,
    paths: &[String],
) -> Result<Oid, GitError> {
    use sha1::{Digest, Sha1};
    let parent = match c.parent_count() {
        0 => None,
        _ => Some(c.parent(0)?.tree()?),
    };
    let diff = tree_diff(repo, parent.as_ref(), &c.tree()?)?;
    let mut sum = [0u8; 20];
    let squeeze = |b: &[u8]| -> Vec<u8> {
        b.iter()
            .copied()
            .filter(|c| !c.is_ascii_whitespace())
            .collect()
    };
    for i in 0..diff.deltas().len() {
        let Some(patch) = git2::Patch::from_diff(&diff, i)? else {
            continue;
        };
        let delta = patch.delta();
        let name = delta.new_file().path().or(delta.old_file().path());
        if !paths.is_empty()
            && !name.is_some_and(|p| crate::pathspec_matches(paths, &p.to_string_lossy()))
        {
            continue;
        }
        let path = |f: git2::DiffFile| {
            f.path()
                .map_or(Vec::new(), |p| squeeze(p.to_string_lossy().as_bytes()))
        };
        let (a, b) = (path(delta.old_file()), path(delta.new_file()));
        let (ma, mb) = (
            u32::from(delta.old_file().mode()),
            u32::from(delta.new_file().mode()),
        );
        let mut h = Sha1::new();
        h.update(b"diff--gita/");
        h.update(&a);
        h.update(b"b/");
        h.update(&b);
        if delta.status() == git2::Delta::Added {
            h.update(format!("newfilemode{mb:06o}"));
        } else if delta.status() == git2::Delta::Deleted {
            h.update(format!("deletedfilemode{ma:06o}"));
        } else if ma != mb {
            h.update(format!("oldmode{ma:06o}newmode{mb:06o}"));
        }
        if delta.flags().is_binary() {
            h.update(delta.old_file().id().to_string());
            h.update(delta.new_file().id().to_string());
        } else {
            match delta.status() {
                git2::Delta::Added => {
                    h.update(b"---/dev/null+++b/");
                    h.update(&b);
                }
                git2::Delta::Deleted => {
                    h.update(b"---a/");
                    h.update(&a);
                    h.update(b"+++/dev/null");
                }
                _ => {
                    h.update(b"---a/");
                    h.update(&a);
                    h.update(b"+++b/");
                    h.update(&b);
                }
            }
            for hunk in 0..patch.num_hunks() {
                for l in 0..patch.num_lines_in_hunk(hunk)? {
                    let line = patch.line_in_hunk(hunk, l)?;
                    // A context line's leading space is whitespace too, and
                    // git skips the `\ No newline at end of file` notes.
                    if matches!(line.origin(), '+' | '-') {
                        h.update([line.origin() as u8]);
                    }
                    if matches!(line.origin(), '+' | '-' | ' ') {
                        h.update(squeeze(line.content()));
                    }
                }
            }
        }
        let mut carry = 0u16;
        for (s, d) in sum.iter_mut().zip(h.finalize()) {
            carry += u16::from(*s) + u16::from(d);
            *s = carry as u8;
            carry >>= 8;
        }
    }
    Ok(Oid::from_bytes(&sum)?)
}

/// `git request-pull`: the summary of what `url` (as `end`, default HEAD)
/// holds beyond `start`, and git's warnings when `url` does not have it.
pub(crate) fn request_pull(
    repo: &Repository,
    start: &str,
    url: &str,
    end: Option<&str>,
    patch: bool,
) -> Result<(String, Vec<String>), GitError> {
    let spec = end.unwrap_or("HEAD");
    let (local, remote) = match spec.split_once(':') {
        Some((l, r)) => (if l.is_empty() { "HEAD" } else { l }, r),
        None => (spec, if end.is_some() { spec } else { "" }),
    };
    let mut pretty = remote.strip_prefix("refs/").unwrap_or(remote);
    pretty = pretty.strip_prefix("heads/").unwrap_or(pretty);
    let mut pretty = pretty.to_owned();
    // The local ref: HEAD's branch, a branch or tag of that name, else a revision.
    let head_ref = if local == "HEAD" {
        repo.find_reference("HEAD")
            .ok()
            .and_then(|r| r.symbolic_target().ok().flatten().map(str::to_owned))
    } else {
        [
            local.to_owned(),
            format!("refs/heads/{local}"),
            format!("refs/tags/{local}"),
        ]
        .into_iter()
        .find(|n| n.starts_with("refs/") && repo.find_reference(n).is_ok())
    };
    let head = head_ref.clone().unwrap_or_else(|| local.to_owned());
    let local_obj = repo
        .rev_single(&head)
        .map_err(|_| GitError::Other(format!("Not a valid revision: {local}")))?;
    let headrev = local_obj.peel_to_commit()?.id();
    let baserev = repo.rev_single(start)?.peel_to_commit()?.id();
    let merge_base = repo
        .merge_base(baserev, headrev)
        .map_err(|_| GitError::Other(format!("No commits in common between {start} and {head}")))?;
    let config = repo.config()?;
    let branch = head_ref
        .as_deref()
        .and_then(|r| r.strip_prefix("refs/heads/"))
        .and_then(|b| {
            config
                .get_string(&format!("branch.{b}.description"))
                .ok()
                .map(|d| (b.to_owned(), d))
        });

    // A ref of that name on the remote that points at the same commit.
    let dir = repo.workdir().unwrap_or(repo.path());
    let (resolved_url, refs) = crate::git_repo::ls_remote(Some(dir), Some(url), true)?;
    let target = if remote.is_empty() { "HEAD" } else { remote };
    let mut remote_sha = None;
    let mut found = None;
    for (name, sha, _) in &refs {
        let (name, deref) = match name.strip_suffix("^{}") {
            Some(n) => (n, true),
            None => (name.as_str(), false),
        };
        if sha == target {
            found = Some((sha.clone(), sha.clone()));
            break;
        }
        if name == target || name.ends_with(&format!("/{target}")) {
            if !deref {
                remote_sha = Some(sha.clone());
            }
            if *sha == headrev.to_string() {
                let sha = remote_sha.clone().unwrap_or_else(|| headrev.to_string());
                found = Some((sha, name.to_owned()));
                break;
            }
        }
    }
    let mut warnings = Vec::new();
    match &found {
        None => {
            warnings.push(format!(
                "warn: No match for commit {headrev} found at {url}"
            ));
            warnings.push(format!("warn: Are you sure you pushed '{target}' there?"));
        }
        Some((sha, _)) if *sha != local_obj.id().to_string() => {
            warnings.push(format!(
                "warn: {head} found at {url} but points to a different object"
            ));
            warnings.push(format!("warn: Are you sure you pushed '{target}' there?"));
        }
        Some(_) => {}
    }
    if found
        .as_ref()
        .is_some_and(|(_, r)| *r == format!("refs/tags/{pretty}"))
    {
        pretty = format!("tags/{pretty}");
    }

    let line = |id: Oid| -> Result<String, GitError> {
        let c = repo.find_commit(id)?;
        Ok(format!(
            "{} ({})",
            c.summary().ok().flatten().unwrap_or(""),
            crate::git_repo::format_git_date(c.committer().when(), "iso")
        ))
    };
    const RULE: &str = "----------------------------------------------------------------\n";
    let mut out = format!(
        "The following changes since commit {merge_base}:\n\n  {}\n\nare available in the Git repository at:\n\n  {resolved_url} {pretty}\n\nfor you to fetch changes up to {headrev}:\n\n  {}\n\n{RULE}",
        line(merge_base)?,
        line(headrev)?
    );
    if let Some(tag) = local_obj.as_tag() {
        let msg = String::from_utf8_lossy(tag.message_bytes().unwrap_or_default()).into_owned();
        for l in msg.lines() {
            if l.starts_with("-----BEGIN PGP ")
                || l.starts_with("-----BEGIN SSH ")
                || l.starts_with("-----BEGIN SIGNED ")
            {
                break;
            }
            out.push_str(l);
            out.push('\n');
        }
        out.push('\n');
        out.push_str(RULE);
    }
    if let Some((b, desc)) = branch {
        out.push_str(&format!(
            "(from the branch description for {b} local branch)\n\n{desc}\n{RULE}"
        ));
    }
    // git shortlog: authors by name, each one's commits oldest first.
    let mut walk = repo.revwalk()?;
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::REVERSE)?;
    walk.push(headrev)?;
    walk.hide(baserev)?;
    let mut groups: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for id in walk {
        let c = repo.find_commit(id?)?;
        let name = String::from_utf8_lossy(c.author().name_bytes()).into_owned();
        groups
            .entry(name)
            .or_default()
            .push(c.summary().ok().flatten().unwrap_or("").to_owned());
    }
    for (name, subjects) in groups {
        out.push_str(&format!("{name} ({}):\n", subjects.len()));
        for s in subjects {
            out.push_str(&format!("      {s}\n"));
        }
        out.push('\n');
    }
    let from = repo.find_commit(merge_base)?.tree()?;
    let to = repo.find_commit(headrev)?.tree()?;
    let mut diff = repo.diff_tree_to_tree(Some(&from), Some(&to), None)?;
    diff.find_similar(Some(DiffFindOptions::new().renames(true)))?;
    let width = std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(80);
    out.push_str(&diffstat(&diff, width)?);
    out.push_str(&summary(&diff)?);
    if patch {
        out.push('\n');
        out.push_str(&patch_text(&diff)?);
    }
    Ok((out, warnings))
}

/// One commit of `git cherry`: whether an equivalent change (same patch id)
/// is already upstream, its id and subject.
#[derive(Debug, Clone)]
pub struct CherryCommit {
    pub upstream_has_it: bool,
    pub id: String,
    pub subject: String,
}

/// `git cherry`: the commits of `head` (after `limit`) that are not in
/// `upstream`, oldest first, marked by whether upstream has an equivalent.
pub(crate) fn cherry(
    repo: &Repository,
    upstream: &str,
    head: &str,
    limit: Option<&str>,
) -> Result<Vec<CherryCommit>, GitError> {
    let resolve =
        |r: &str| -> Result<Oid, GitError> { Ok(repo.rev_single(r)?.peel_to_commit()?.id()) };
    let up = resolve(upstream).map_err(|_| {
        GitError::Other(format!(
            "could not find {upstream}; name the upstream branch (rgit cherry <upstream>)"
        ))
    })?;
    let head = resolve(head)?;
    let side = |from: Oid, hide: &[Oid]| -> Result<Vec<Commit<'_>>, GitError> {
        let mut walk = repo.revwalk()?;
        walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::REVERSE)?;
        walk.push(from)?;
        for h in hide {
            walk.hide(*h)?;
        }
        let mut out = Vec::new();
        for id in walk {
            let c = repo.find_commit(id?)?;
            if c.parent_count() <= 1 {
                out.push(c);
            }
        }
        Ok(out)
    };
    let theirs = side(up, &[head])?
        .iter()
        .map(|c| patch_id(repo, c))
        .collect::<Result<std::collections::HashSet<_>, _>>()?;
    let mut hide = vec![up];
    if let Some(l) = limit {
        hide.push(resolve(l)?);
    }
    side(head, &hide)?
        .iter()
        .map(|c| {
            Ok(CherryCommit {
                upstream_has_it: theirs.contains(&patch_id(repo, c)?),
                id: c.id().to_string(),
                subject: c.summary().ok().flatten().unwrap_or("").to_owned(),
            })
        })
        .collect()
}

fn tree_diff<'r>(
    repo: &'r Repository,
    from: Option<&git2::Tree>,
    to: &git2::Tree,
) -> Result<Diff<'r>, GitError> {
    tree_diff_opts(repo, from, to, true)
}

fn tree_diff_opts<'r>(
    repo: &'r Repository,
    from: Option<&git2::Tree>,
    to: &git2::Tree,
    binary: bool,
) -> Result<Diff<'r>, GitError> {
    let mut opts = DiffOptions::new();
    opts.show_binary(binary);
    let mut diff = repo.diff_tree_to_tree(from, Some(to), Some(&mut opts))?;
    diff.find_similar(Some(DiffFindOptions::new().renames(true)))?;
    Ok(diff)
}

/// The commit message after the subject paragraph, as the email body.
fn body_of(c: &Commit) -> String {
    let msg = String::from_utf8_lossy(c.message_bytes()).into_owned();
    let body = msg
        .split_once("\n\n")
        .map_or("", |(_, rest)| rest)
        .trim_start_matches('\n')
        .trim_end();
    if body.is_empty() {
        String::new()
    } else {
        format!("{body}\n")
    }
}

/// `From: name <email>`, quoted or RFC 2047-encoded as git does.
fn from_header(sig: &git2::Signature, encode: bool) -> String {
    let name = String::from_utf8_lossy(sig.name_bytes()).into_owned();
    let email = String::from_utf8_lossy(sig.email_bytes()).into_owned();
    let mut out = String::from("From: ");
    let mut max = 78;
    if encode && needs_rfc2047(&name) {
        out.push_str(&rfc2047(&name, 6, true));
        max = 76;
    } else if name.contains([
        '(', ')', '<', '>', '[', ']', ':', ';', '@', ',', '.', '"', '\\',
    ]) {
        let quoted = format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""));
        out.push_str(&wrap(&quoted, 6, 1, 78));
    } else {
        out.push_str(&wrap(&name, 6, 1, 78));
    }
    let col = out.rsplit('\n').next().unwrap_or("").chars().count();
    if max < col + 2 + email.len() + 1 {
        out.push('\n');
    }
    let _ = writeln!(out, " <{email}>");
    out
}

/// The subject after `Subject: [PATCH] ` (`used` columns), wrapped at 78 or
/// RFC 2047-encoded.
fn encode_subject(subject: &str, used: usize, encode: bool) -> String {
    if encode && needs_rfc2047(subject) {
        rfc2047(subject, used, false)
    } else {
        wrap(subject, used, 1, 78)
    }
}

fn needs_rfc2047(s: &str) -> bool {
    !s.is_ascii() || s.contains('\n') || s.contains("=?")
}

/// git's RFC 2047 Q-encoding, folded to 76 columns.
fn rfc2047(s: &str, used: usize, address: bool) -> String {
    let special = |b: u8| {
        if !b.is_ascii_graphic() || b == b'=' || b == b'?' || b == b'_' {
            return true;
        }
        address && !(b.is_ascii_alphanumeric() || b"!*+-/".contains(&b))
    };
    let mut out = String::from("=?UTF-8?q?");
    let mut len = used + "UTF-8".len() + 5;
    for ch in s.chars() {
        let mut buf = [0; 4];
        let bytes = ch.encode_utf8(&mut buf).as_bytes();
        let encoded: String = if bytes.len() > 1 || special(bytes[0]) {
            bytes.iter().map(|b| format!("={b:02X}")).collect()
        } else {
            ch.to_string()
        };
        if len + encoded.len() + 2 > 76 {
            out.push_str("?=\n =?UTF-8?q?");
            len = "UTF-8".len() + 5 + 1;
        }
        len += encoded.len();
        out.push_str(&encoded);
    }
    out.push_str("?=");
    out
}

/// Words of `text` wrapped at `width`; the first line already has `used`
/// columns, later lines start with `indent` spaces.
fn wrap(text: &str, used: usize, indent: usize, width: usize) -> String {
    let mut out = String::new();
    let mut col = used;
    for (i, word) in text.split(' ').filter(|w| !w.is_empty()).enumerate() {
        let w = word.chars().count();
        if i > 0 && col + 1 + w > width {
            out.push('\n');
            out.push_str(&" ".repeat(indent));
            col = indent;
        } else if i > 0 {
            out.push(' ');
            col += 1;
        }
        out.push_str(word);
        col += w;
    }
    out
}

/// A cover letter's `git shortlog`: commits grouped by author.
fn shortlog(commits: &[Commit]) -> String {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for c in commits {
        let name = String::from_utf8_lossy(c.author().name_bytes()).into_owned();
        let subject = c.summary().ok().flatten().unwrap_or("").to_owned();
        match groups.iter_mut().find(|(n, _)| *n == name) {
            Some((_, subjects)) => subjects.push(subject),
            None => groups.push((name, vec![subject])),
        }
    }
    groups.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = String::new();
    for (name, subjects) in groups {
        let _ = writeln!(out, "{name} ({}):", subjects.len());
        for s in subjects {
            let _ = writeln!(out, "  {}", wrap(&s, 2, 4, 72));
        }
        out.push('\n');
    }
    out
}

/// A file's name in a diffstat: `dir/{old => new}` for a rename.
fn stat_name(delta: &git2::DiffDelta) -> String {
    let path = |f: git2::DiffFile| f.path().map_or(String::new(), |p| p.display().to_string());
    let (old, new) = (path(delta.old_file()), path(delta.new_file()));
    if matches!(delta.status(), git2::Delta::Renamed | git2::Delta::Copied) && old != new {
        pprint_rename(&old, &new)
    } else if new.is_empty() {
        old
    } else {
        new
    }
}

/// git's `pprint_rename`: the common leading folders and trailing part
/// outside braces.
pub fn pprint_rename(a: &str, b: &str) -> String {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let mut pfx = 0;
    let mut i = 0;
    while i < ab.len() && i < bb.len() && ab[i] == bb[i] {
        if ab[i] == b'/' {
            pfx = i + 1;
        }
        i += 1;
    }
    let mut sfx = 0;
    let adjust = usize::from(pfx > 0);
    let (mut oa, mut ob) = (ab.len() as isize, bb.len() as isize);
    // Walk back while the tails agree, staying out of the prefix but for its
    // closing slash.
    while oa >= (pfx - adjust) as isize && ob >= (pfx - adjust) as isize {
        let ca = if (oa as usize) < ab.len() {
            ab[oa as usize]
        } else {
            0
        };
        let cb = if (ob as usize) < bb.len() {
            bb[ob as usize]
        } else {
            0
        };
        if ca != cb {
            break;
        }
        if ca == b'/' {
            sfx = ab.len() - oa as usize;
        }
        oa -= 1;
        ob -= 1;
    }
    let amid = ab.len().saturating_sub(pfx + sfx);
    let bmid = bb.len().saturating_sub(pfx + sfx);
    let mut out = String::new();
    if pfx + sfx > 0 {
        out.push_str(&a[..pfx]);
        out.push('{');
    }
    out.push_str(&a[pfx..pfx + amid]);
    out.push_str(" => ");
    out.push_str(&b[pfx..pfx + bmid]);
    if pfx + sfx > 0 {
        out.push('}');
        out.push_str(&a[a.len() - sfx..]);
    }
    out
}

/// git's `--stat` for `diff` at `width` columns (diff.c's `show_stats`).
pub fn diffstat(diff: &Diff, width: usize) -> Result<String, GitError> {
    struct Row {
        name: String,
        added: usize,
        deleted: usize,
        binary: bool,
    }
    let mut rows = Vec::new();
    for (i, delta) in diff.deltas().enumerate() {
        let patch = git2::Patch::from_diff(diff, i)?;
        let delta = patch.as_ref().map_or(delta, |p| p.delta());
        let binary = patch.is_none() || delta.flags().is_binary();
        let (added, deleted) = if binary {
            (
                delta.new_file().size() as usize,
                delta.old_file().size() as usize,
            )
        } else {
            let (_, a, d) = patch.as_ref().map_or(Ok((0, 0, 0)), |p| p.line_stats())?;
            (a, d)
        };
        rows.push(Row {
            name: stat_name(&delta),
            added,
            deleted,
            binary,
        });
    }
    let dw = |n: usize| n.to_string().len();
    let (mut max_len, mut max_change, mut bin_width, mut number_width) = (0, 0, 0, 0);
    for r in &rows {
        max_len = max_len.max(r.name.chars().count());
        if r.binary {
            bin_width = bin_width.max(14 + dw(r.added) + dw(r.deleted));
            number_width = 3;
        } else {
            max_change = max_change.max(r.added + r.deleted);
        }
    }
    number_width = number_width.max(dw(max_change));
    let width = width.max(16 + 6 + number_width);
    let mut graph_width = if max_change + 4 > bin_width {
        max_change
    } else {
        bin_width - 4
    };
    let mut name_width = max_len;
    if name_width + number_width + 6 + graph_width > width {
        let cap = (width * 3 / 8).saturating_sub(number_width + 6);
        if graph_width > cap {
            graph_width = cap.max(6);
        }
        if name_width > width.saturating_sub(number_width + 6 + graph_width) {
            name_width = width.saturating_sub(number_width + 6 + graph_width);
        } else {
            graph_width = width.saturating_sub(number_width + 6 + name_width);
        }
    }
    let scale = |it: usize| {
        if it == 0 {
            0
        } else {
            1 + it * (graph_width - 1) / max_change
        }
    };
    let mut out = String::new();
    let (mut adds, mut dels) = (0, 0);
    for r in &rows {
        let mut name = r.name.clone();
        let mut prefix = "";
        let mut len = name_width;
        if name_width < name.chars().count() {
            prefix = "...";
            len = len.saturating_sub(3);
            let chars: Vec<char> = name.chars().collect();
            let mut tail: String = chars[chars.len() - len.min(chars.len())..].iter().collect();
            if let Some(i) = tail.find('/') {
                tail = tail[i..].to_owned();
            }
            name = tail;
        }
        let padding = len.saturating_sub(name.chars().count());
        if r.binary {
            let _ = write!(
                out,
                " {prefix}{name}{} | {:>number_width$}",
                " ".repeat(padding),
                "Bin"
            );
            if r.added == 0 && r.deleted == 0 {
                out.push('\n');
            } else {
                let _ = writeln!(out, " {} -> {} bytes", r.deleted, r.added);
            }
            continue;
        }
        adds += r.added;
        dels += r.deleted;
        let (mut add, mut del) = (r.added, r.deleted);
        if graph_width <= max_change {
            let mut total = scale(add + del);
            if total < 2 && add > 0 && del > 0 {
                total = 2;
            }
            if add < del {
                add = scale(add);
                del = total - add;
            } else {
                del = scale(del);
                add = total - del;
            }
        }
        let changed = r.added + r.deleted;
        let _ = writeln!(
            out,
            " {prefix}{name}{} | {changed:>number_width$}{}{}{}",
            " ".repeat(padding),
            if changed > 0 { " " } else { "" },
            "+".repeat(add),
            "-".repeat(del)
        );
    }
    out.push_str(&crate::apply::stat_summary(rows.len(), adds, dels));
    out.push('\n');
    Ok(out)
}

/// git's `--summary` for `diff`: created, deleted, renamed and mode-changed files.
pub fn summary(diff: &Diff) -> Result<String, GitError> {
    let mut out = String::new();
    for delta in diff.deltas() {
        let (old, new) = (delta.old_file(), delta.new_file());
        let path = |f: &git2::DiffFile| f.path().map_or(String::new(), |p| p.display().to_string());
        let mode = |f: &git2::DiffFile| u32::from(f.mode());
        let mode_change = |show: Option<String>| {
            if mode(&old) != 0 && mode(&new) != 0 && mode(&old) != mode(&new) {
                let name = show.map_or(String::new(), |n| format!(" {n}"));
                format!(
                    " mode change {:06o} => {:06o}{name}\n",
                    mode(&old),
                    mode(&new)
                )
            } else {
                String::new()
            }
        };
        match delta.status() {
            git2::Delta::Added => {
                let _ = writeln!(out, " create mode {:06o} {}", mode(&new), path(&new));
            }
            git2::Delta::Deleted => {
                let _ = writeln!(out, " delete mode {:06o} {}", mode(&old), path(&old));
            }
            s @ (git2::Delta::Renamed | git2::Delta::Copied) => {
                // SAFETY: the delta is live; git2 does not expose the score.
                let score = unsafe { (*git2::Binding::raw(&delta)).similarity };
                let kind = if s == git2::Delta::Renamed {
                    "rename"
                } else {
                    "copy"
                };
                let _ = writeln!(
                    out,
                    " {kind} {} ({score}%)",
                    pprint_rename(&path(&old), &path(&new))
                );
                out.push_str(&mode_change(None));
            }
            _ => out.push_str(&mode_change(Some(path(&new)))),
        }
    }
    Ok(out)
}

/// The diff as `git diff` prints a patch.
pub(crate) fn patch_text(diff: &Diff) -> Result<String, GitError> {
    let mut out = Vec::new();
    diff.print(git2::DiffFormat::Patch, |_, _, line| {
        if matches!(line.origin(), '+' | '-' | ' ') {
            out.push(line.origin() as u8);
        }
        out.extend_from_slice(line.content());
        true
    })?;
    Ok(rezip_binary(&String::from_utf8_lossy(&out)))
}

const BASE85: &[u8; 85] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz!#$%&()*+-;<=>?@^_`{|}~";

/// Binary hunks deflated at git's level (Z_BEST_SPEED) rather than libgit2's,
/// so the base85 text matches git's.
pub(crate) fn rezip_binary(patch: &str) -> String {
    let mut out = String::with_capacity(patch.len());
    let mut lines = patch.split_inclusive('\n').peekable();
    let mut in_binary = false;
    while let Some(line) = lines.next() {
        out.push_str(line);
        if line == "GIT binary patch\n" {
            in_binary = true;
            continue;
        }
        if !(in_binary && (line.starts_with("literal ") || line.starts_with("delta "))) {
            in_binary &= line.trim_end().is_empty()
                || line.starts_with("literal ")
                || line.starts_with("delta ");
            continue;
        }
        let mut data = Vec::new();
        let mut block = Vec::new();
        while let Some(l) = lines.next_if(|l| !l.trim_end().is_empty()) {
            block.push(l);
            data.extend(decode85_line(l.trim_end()).unwrap_or_default());
        }
        match reflate(&data) {
            Some(z) => {
                for chunk in z.chunks(52) {
                    out.push_str(&encode85_line(chunk));
                }
            }
            None => block.iter().for_each(|l| out.push_str(l)),
        }
    }
    out
}

fn decode85_line(line: &str) -> Option<Vec<u8>> {
    let b = line.as_bytes();
    let len = match *b.first()? {
        c @ b'A'..=b'Z' => c - b'A' + 1,
        c @ b'a'..=b'z' => c - b'a' + 27,
        _ => return None,
    } as usize;
    let mut out = Vec::new();
    for group in b[1..].chunks(5) {
        let mut acc: u32 = 0;
        for &c in group {
            let v = BASE85.iter().position(|&x| x == c)? as u32;
            acc = acc.checked_mul(85)?.checked_add(v)?;
        }
        out.extend_from_slice(&acc.to_be_bytes());
    }
    out.truncate(len);
    Some(out)
}

fn encode85_line(data: &[u8]) -> String {
    let len = data.len() as u8;
    let mut out = String::new();
    out.push(if len <= 26 {
        (b'A' + len - 1) as char
    } else {
        (b'a' + len - 27) as char
    });
    for group in data.chunks(4) {
        let mut word = [0u8; 4];
        word[..group.len()].copy_from_slice(group);
        let mut acc = u32::from_be_bytes(word);
        let mut enc = [0u8; 5];
        for slot in enc.iter_mut().rev() {
            *slot = BASE85[(acc % 85) as usize];
            acc /= 85;
        }
        out.push_str(std::str::from_utf8(&enc).unwrap_or_default());
    }
    out.push('\n');
    out
}

/// Inflate `z` and deflate it again at zlib level 1.
fn reflate(z: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(z)
        .read_to_end(&mut raw)
        .ok()?;
    // SAFETY: `dest` is sized by compressBound and truncated to what zlib wrote.
    unsafe {
        let mut len = libz_sys::compressBound(raw.len() as _);
        let mut dest = vec![0u8; len as usize];
        let rc = libz_sys::compress2(dest.as_mut_ptr(), &mut len, raw.as_ptr(), raw.len() as _, 1);
        if rc != 0 {
            return None;
        }
        dest.truncate(len as usize);
        Some(dest)
    }
}

/// git's file-name form of a subject: runs of other characters become `-`.
fn sanitize(subject: &str) -> String {
    let mut name = String::new();
    let mut gap = false;
    let mut chars = subject.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
            if gap && !name.is_empty() {
                name.push('-');
            }
            gap = false;
            name.push(c);
            while c == '.' && chars.peek() == Some(&'.') {
                chars.next();
            }
        } else {
            gap = true;
        }
    }
    while name.ends_with(['.', '-']) {
        name.pop();
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renames_and_wrapping_match_git() {
        assert_eq!(
            pprint_rename("src/deep/old.txt", "src/deep/new.txt"),
            "src/deep/{old.txt => new.txt}"
        );
        assert_eq!(pprint_rename("a/x/f.rs", "b/x/f.rs"), "{a => b}/x/f.rs");
        assert_eq!(pprint_rename("a.txt", "b.txt"), "a.txt => b.txt");
        assert_eq!(wrap("one two three", 8, 1, 14), "one\n two three");
        assert_eq!(rfc2047("é x", 0, false), "=?UTF-8?q?=C3=A9=20x?=");
        assert_eq!(sanitize("Fix: the [bug]..."), "Fix-the-bug");
        let line = encode85_line(b"hello, world");
        assert_eq!(decode85_line(line.trim_end()).unwrap(), b"hello, world");
    }
}
