//! `git am`: mailboxes are split and read as git's mailsplit and mailinfo do,
//! each patch goes through rgit's apply, and progress lives in git's
//! `.git/rebase-apply`, so git and rgit can each continue what the other began.

use crate::rev::RevParse;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use git2::{Oid, Repository};

use crate::GitError;
use crate::apply::{self, ApplyOpts};

/// A session command instead of new mail.
#[derive(Clone, PartialEq)]
enum Resume {
    Continue,
    AllowEmpty,
    Skip,
    Abort,
    Quit,
    Show(String),
}

/// `git am`'s command line.
#[derive(Default)]
struct Args {
    resume: Option<Resume>,
    mboxes: Vec<String>,
    three_way: Option<bool>,
    signoff: bool,
    keep: Option<char>,
    message_id: Option<bool>,
    scissors: Option<bool>,
    keep_cr: Option<bool>,
    utf8: Option<bool>,
    committer_date_is_author_date: bool,
    ignore_date: bool,
    interactive: bool,
    no_verify: bool,
    quiet: bool,
    empty: Option<String>,
    patch_format: Option<String>,
    apply: Vec<String>,
    gpg_sign: Option<String>,
    no_gpg_sign: bool,
    rerere: Option<bool>,
}

fn parse_args(args: &[String]) -> Result<Args, GitError> {
    let mut a = Args::default();
    let mut paths_only = false;
    for arg in args {
        if paths_only || !arg.starts_with('-') || arg == "-" {
            a.mboxes.push(arg.clone());
            continue;
        }
        let value = |flag: &str| arg.strip_prefix(flag).map(str::to_owned);
        match arg.as_str() {
            "--" => paths_only = true,
            "--continue" | "--resolved" | "-r" => a.resume = Some(Resume::Continue),
            "--allow-empty" => a.resume = Some(Resume::AllowEmpty),
            "--skip" => a.resume = Some(Resume::Skip),
            "--abort" => a.resume = Some(Resume::Abort),
            "--quit" => a.resume = Some(Resume::Quit),
            "--show-current-patch" => a.resume = Some(Resume::Show("raw".into())),
            "-3" | "--3way" => a.three_way = Some(true),
            "--no-3way" => a.three_way = Some(false),
            "-s" | "--signoff" => a.signoff = true,
            "-k" | "--keep" => a.keep = Some('t'),
            "--keep-non-patch" => a.keep = Some('b'),
            "-m" | "--message-id" => a.message_id = Some(true),
            "--no-message-id" => a.message_id = Some(false),
            "-c" | "--scissors" => a.scissors = Some(true),
            "--no-scissors" => a.scissors = Some(false),
            "--keep-cr" => a.keep_cr = Some(true),
            "--no-keep-cr" => a.keep_cr = Some(false),
            "-u" | "--utf8" => a.utf8 = Some(true),
            "--no-utf8" => a.utf8 = Some(false),
            "--committer-date-is-author-date" => a.committer_date_is_author_date = true,
            "--ignore-date" => a.ignore_date = true,
            "-i" | "--interactive" => a.interactive = true,
            "-n" | "--no-verify" => a.no_verify = true,
            "-q" | "--quiet" => a.quiet = true,
            "--no-gpg-sign" => a.no_gpg_sign = true,
            "--rerere-autoupdate" => a.rerere = Some(true),
            "--no-rerere-autoupdate" => a.rerere = Some(false),
            s if s.starts_with("--gpg-sign") || s.starts_with("-S") => {
                let key = s
                    .strip_prefix("--gpg-sign")
                    .or_else(|| s.strip_prefix("-S"))
                    .unwrap_or("");
                a.gpg_sign = Some(key.strip_prefix('=').unwrap_or(key).to_owned());
            }
            "--reject" | "--ignore-space-change" | "--ignore-whitespace" => {
                a.apply.push(arg.clone())
            }
            _ => {
                if let Some(v) = value("--show-current-patch=") {
                    a.resume = Some(Resume::Show(v));
                } else if let Some(v) = value("--empty=") {
                    a.empty = Some(v);
                } else if let Some(v) = value("--patch-format=") {
                    a.patch_format = Some(v);
                } else if [
                    "-p",
                    "-C",
                    "--directory=",
                    "--exclude=",
                    "--include=",
                    "--whitespace=",
                ]
                .iter()
                .any(|p| arg.starts_with(p))
                {
                    a.apply.push(arg.clone());
                } else {
                    return Err(GitError::Other(format!("unknown option `{arg}'")));
                }
            }
        }
    }
    Ok(a)
}

/// Run `git am` with `args`; `stdin` is the mail when no mailbox is named.
pub(crate) fn am(
    repo: &Repository,
    args: &[String],
    stdin: Option<&[u8]>,
) -> Result<String, GitError> {
    let a = parse_args(args)?;
    let dir = repo.path().join("rebase-apply");
    let mut out = String::new();
    let in_progress = dir.join("last").is_file() && dir.join("next").is_file();
    if in_progress {
        if !a.mboxes.is_empty() || stdin.is_some() {
            return Err(GitError::Other(format!(
                "previous rebase directory {} still exists but mbox given.",
                dir.display()
            )));
        }
        let mut s = Session::load(&dir, &a)?;
        let result = match a.resume.clone() {
            None => s.run(repo, true, &mut out),
            Some(Resume::Continue) => s.resolve(repo, false, &mut out),
            Some(Resume::AllowEmpty) => s.resolve(repo, true, &mut out),
            Some(Resume::Skip) => s.skip(repo, &mut out),
            Some(Resume::Abort) => s.abort(repo, &mut out),
            Some(Resume::Quit) => s.destroy(),
            Some(Resume::Show(part)) => return s.show(&part),
        };
        return finish(result, out);
    }
    if a.resume.is_some() {
        return Err(GitError::Other(
            "Resolve operation not in progress, we are not resuming.".into(),
        ));
    }
    if dir.exists() {
        return Err(GitError::Other(format!(
            "Stray {} directory found.\nUse \"git am --abort\" to remove it.",
            dir.display()
        )));
    }
    let cfg = repo.config()?;
    let flag = |key: &str| cfg.get_bool(key).unwrap_or(false);
    let keep_cr = a.keep_cr.unwrap_or_else(|| flag("am.keepcr"));
    let mails = read_mails(repo, &a, stdin, keep_cr)?;
    if mails.is_empty() {
        return Ok(String::new());
    }
    let mut s = Session {
        dir,
        cur: 1,
        last: mails.len(),
        threeway: a.three_way.unwrap_or_else(|| flag("am.threeWay")),
        quiet: a.quiet,
        sign: a.signoff,
        utf8: a.utf8.unwrap_or(true),
        keep: a.keep.unwrap_or('f'),
        message_id: a.message_id.unwrap_or_else(|| flag("am.messageid")),
        scissors: a.scissors,
        apply: a.apply.clone(),
        run: RunOpts::from(&a),
        msg: String::new(),
        rerere: a.rerere,
    };
    s.setup(repo, &mails)?;
    let result = s.run(repo, false, &mut out);
    finish(result, out)
}

/// Fold the progress text into the result: on a stop it leads the error.
fn finish(result: Result<(), GitError>, out: String) -> Result<String, GitError> {
    match result {
        Ok(()) => Ok(out.trim_end().to_owned()),
        Err(GitError::Conflict(why)) => Err(GitError::Conflict(format!("{out}{why}"))),
        Err(e) if out.is_empty() => Err(e),
        Err(e) => Err(GitError::Other(format!("{out}{e}"))),
    }
}

/// The options git does not keep in the state, taken from each invocation.
#[derive(Default)]
struct RunOpts {
    committer_date_is_author_date: bool,
    ignore_date: bool,
    no_verify: bool,
    interactive: bool,
    empty: String,
    gpg_sign: Option<String>,
    no_gpg_sign: bool,
}

impl RunOpts {
    fn from(a: &Args) -> Self {
        RunOpts {
            committer_date_is_author_date: a.committer_date_is_author_date,
            ignore_date: a.ignore_date,
            no_verify: a.no_verify,
            interactive: a.interactive,
            empty: a.empty.clone().unwrap_or_else(|| "stop".into()),
            gpg_sign: a.gpg_sign.clone(),
            no_gpg_sign: a.no_gpg_sign,
        }
    }
}

/// An am session, as `.git/rebase-apply` records it.
struct Session {
    dir: PathBuf,
    cur: usize,
    last: usize,
    threeway: bool,
    quiet: bool,
    sign: bool,
    utf8: bool,
    /// `t` for -k, `b` for --keep-non-patch, else `f`.
    keep: char,
    message_id: bool,
    scissors: Option<bool>,
    apply: Vec<String>,
    run: RunOpts,
    /// The current patch's commit message.
    msg: String,
    /// `--[no-]rerere-autoupdate`.
    rerere: Option<bool>,
}

/// Why a patch stopped the run: git's hints follow.
fn stop(why: String) -> GitError {
    GitError::Conflict(why)
}

impl Session {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// Write a state file ending in a newline unless empty, as git's write_file.
    fn write(&self, name: &str, text: &str) -> Result<(), GitError> {
        let nl = if text.is_empty() || text.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        Ok(std::fs::write(self.path(name), format!("{text}{nl}"))?)
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.path(name)).unwrap_or_default()
    }

    fn msgnum(&self) -> String {
        format!("{:04}", self.cur)
    }

    fn say(&self, out: &mut String, line: &str) {
        if !self.quiet {
            out.push_str(line);
            out.push('\n');
        }
    }

    fn setup(&mut self, repo: &Repository, mails: &[Vec<u8>]) -> Result<(), GitError> {
        std::fs::create_dir_all(&self.dir)?;
        for (i, mail) in mails.iter().enumerate() {
            std::fs::write(self.path(&format!("{:04}", i + 1)), mail)?;
        }
        let tf = |b: bool| if b { "t" } else { "f" };
        self.write("threeway", tf(self.threeway))?;
        self.write("quiet", tf(self.quiet))?;
        self.write("sign", tf(self.sign))?;
        self.write("utf8", tf(self.utf8))?;
        self.write("keep", &self.keep.to_string())?;
        self.write("messageid", tf(self.message_id))?;
        self.write("scissors", self.scissors.map_or("", tf))?;
        self.write("quoted-cr", "")?;
        if let Some(auto) = self.rerere {
            self.write("rerere-autoupdate", tf(auto))?;
        }
        let opts: String = self.apply.iter().map(|o| format!(" {}", sq(o))).collect();
        self.write("apply-opt", &opts)?;
        self.write("applying", "")?;
        match repo.head().ok().and_then(|h| h.target()) {
            Some(head) => {
                self.write("abort-safety", &head.to_string())?;
                repo.reference("ORIG_HEAD", head, true, "am")?;
            }
            None => {
                self.write("abort-safety", "")?;
                if let Ok(mut r) = repo.find_reference("ORIG_HEAD") {
                    r.delete()?;
                }
            }
        }
        // git reads a session as started once next and last exist.
        self.write("next", &format!("{}\n", self.cur))?;
        self.write("last", &format!("{}\n", self.last))?;
        Ok(())
    }

    fn load(dir: &Path, a: &Args) -> Result<Self, GitError> {
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
        let num = |name: &str| {
            read(name)
                .trim()
                .parse::<usize>()
                .map_err(|_| GitError::Other(format!("could not read '{name}'")))
        };
        let t = |name: &str| read(name).trim() == "t";
        let mut s = Session {
            dir: dir.to_path_buf(),
            cur: num("next")?,
            last: num("last")?,
            threeway: t("threeway"),
            quiet: t("quiet") || a.quiet,
            sign: t("sign"),
            utf8: t("utf8"),
            keep: read("keep").trim().chars().next().unwrap_or('f'),
            message_id: t("messageid"),
            scissors: match read("scissors").trim() {
                "t" => Some(true),
                "f" => Some(false),
                _ => None,
            },
            apply: sq_dequote_all(read("apply-opt").trim()),
            run: RunOpts::from(a),
            msg: read("final-commit"),
            rerere: dir
                .join("rerere-autoupdate")
                .exists()
                .then(|| t("rerere-autoupdate")),
        };
        if a.signoff && !s.sign {
            s.sign = true;
        }
        Ok(s)
    }

    /// Apply the patches from the current one on (git's `am_run`); `resume`
    /// reuses the current patch's parsed message.
    fn run(
        &mut self,
        repo: &Repository,
        mut resume: bool,
        out: &mut String,
    ) -> Result<(), GitError> {
        let _ = std::fs::remove_file(self.path("dirtyindex"));
        repo.index()?.read(true)?;
        if let Some(dirty) = dirty_index(repo)? {
            self.write("dirtyindex", "t")?;
            return Err(GitError::Other(format!(
                "Dirty index: cannot apply patches (dirty: {dirty})"
            )));
        }
        while self.cur <= self.last {
            let mail = self.path(&self.msgnum());
            if !mail.exists() {
                self.next(repo)?;
                continue;
            }
            if resume {
                self.msg = self.read("final-commit");
            } else if !self.parse_mail(repo, &std::fs::read(&mail)?)? {
                self.next(repo)?;
                continue;
            }
            resume = false;
            if self.run.interactive && !self.interactive(repo)? {
                self.next(repo)?;
                continue;
            }
            let empty_patch = self.read("patch").is_empty();
            let mut keep = false;
            if empty_patch {
                match self.run.empty.as_str() {
                    "drop" => {
                        self.say(out, &format!("Skipping: {}", first_line(&self.msg)));
                        self.next(repo)?;
                        continue;
                    }
                    "keep" => {
                        keep = true;
                        self.say(
                            out,
                            &format!("Creating an empty commit: {}", first_line(&self.msg)),
                        );
                    }
                    _ => {
                        out.push_str("Patch is empty.\n");
                        return Err(stop(self.resolve_hint(repo)));
                    }
                }
            }
            if !self.run.no_verify {
                let file = self.path("final-commit");
                crate::git_repo::run_hook_file(repo, "applypatch-msg", &[&file])?;
                self.msg = std::fs::read_to_string(&file).map_err(|_| {
                    GitError::Other(format!(
                        "'{}' was deleted by the applypatch-msg hook",
                        file.display()
                    ))
                })?;
            }
            if !keep {
                self.say(out, &format!("Applying: {}", first_line(&self.msg)));
                let merged = match self.apply(repo, out) {
                    Ok(merged) => merged,
                    Err(e) => {
                        let _ = writeln!(out, "{e}");
                        let _ = writeln!(
                            out,
                            "Patch failed at {} {}",
                            self.msgnum(),
                            first_line(&self.msg)
                        );
                        out.push_str(
                            "hint: Use 'git am --show-current-patch=diff' to see the failed patch\n",
                        );
                        return Err(stop(self.resolve_hint(repo)));
                    }
                };
                if merged && dirty_index(repo)?.is_none() {
                    self.say(out, "No changes -- Patch already applied.");
                    self.next(repo)?;
                    continue;
                }
            }
            self.commit(repo, out)?;
            self.next(repo)?;
        }
        self.destroy()
    }

    /// git's hints for a stopped patch.
    fn resolve_hint(&self, repo: &Repository) -> String {
        let cmd = if self.run.interactive {
            "git am -i"
        } else {
            "git am"
        };
        let mut s = format!(
            "hint: When you have resolved this problem, run \"{cmd} --continue\".\n\
             hint: If you prefer to skip this patch, run \"{cmd} --skip\" instead.\n"
        );
        if self.read("patch").is_empty() && dirty_index(repo).ok().flatten().is_none() {
            let _ = writeln!(
                s,
                "hint: To record the empty patch as an empty commit, run \"{cmd} --allow-empty\"."
            );
        }
        let _ = write!(
            s,
            "hint: To restore the original branch and stop patching, run \"{cmd} --abort\".\n\
             hint: Disable this message with \"git config set advice.mergeConflict false\""
        );
        let off = repo
            .config()
            .and_then(|c| c.get_bool("advice.mergeConflict"))
            .is_ok_and(|on| !on);
        if off { String::new() } else { s }
    }

    /// Split the current mail into info, msg and patch and write the author
    /// and message files. `false` skips the mail (pine's folder data).
    fn parse_mail(&mut self, repo: &Repository, mail: &[u8]) -> Result<bool, GitError> {
        let cfg = repo.config()?;
        let scissors = self
            .scissors
            .unwrap_or_else(|| cfg.get_bool("mailinfo.scissors").unwrap_or(false));
        let m = mailinfo(
            mail,
            &MailOpts {
                keep_subject: self.keep == 't',
                keep_non_patch: self.keep == 'b',
                message_id: self.message_id,
                scissors,
            },
        );
        let mut info = String::new();
        if !m.email.is_empty() || !m.author.is_empty() {
            let _ = writeln!(info, "Author: {}\nEmail: {}", m.author, m.email);
        }
        for line in m.subject.split('\n').filter(|_| !m.subject.is_empty()) {
            let _ = writeln!(info, "Subject: {line}");
        }
        if !m.date.is_empty() {
            let _ = writeln!(info, "Date: {}", m.date);
        }
        info.push('\n');
        self.write("info", &info)?;
        self.write("msg", &m.msg)?;
        std::fs::write(self.path("patch"), &m.patch)?;
        if m.author == "Mail System Internal Data" {
            return Ok(false);
        }
        let mut msg = git2::message_prettify(format!("{}\n\n{}", m.subject, m.msg), None)?;
        if self.sign {
            msg = crate::git_repo::signoff(&msg, &repo.committer_from_env()?);
        }
        self.write(
            "author-script",
            &format!(
                "GIT_AUTHOR_NAME={}\nGIT_AUTHOR_EMAIL={}\nGIT_AUTHOR_DATE={}\n",
                sq(&m.author),
                sq(&m.email),
                sq(&m.date)
            ),
        )?;
        self.write("final-commit", &msg)?;
        self.msg = msg;
        Ok(true)
    }

    /// Apply `patch` to the index and working tree, falling back to a
    /// three-way merge with `--3way`; returns whether it fell back.
    fn apply(&self, repo: &Repository, out: &mut String) -> Result<bool, GitError> {
        let mut opts = ApplyOpts {
            index: true,
            quiet: true,
            ..Default::default()
        };
        for o in &self.apply {
            if let Some(n) = o.strip_prefix("-p") {
                opts.strip = n.parse().ok();
            } else if let Some(d) = o.strip_prefix("--directory=") {
                opts.directory = Some(d.to_owned());
            } else if let Some(g) = o.strip_prefix("--exclude=") {
                opts.exclude.push(g.to_owned());
            } else if let Some(g) = o.strip_prefix("--include=") {
                opts.include.push(g.to_owned());
            } else if let Some(w) = o.strip_prefix("--whitespace=") {
                opts.whitespace = Some(w.to_owned());
            } else if o == "--reject" {
                opts.reject = true;
            }
        }
        let action = opts
            .whitespace
            .clone()
            .or_else(|| repo.config().ok()?.get_string("apply.whitespace").ok())
            .unwrap_or_else(|| "warn".into());
        let mut files = apply::parse_patch(&std::fs::read(self.path("patch"))?, &opts)
            .map_err(|e| GitError::Other(format!("error: {e}")))?;
        let ws = apply::check_whitespace(&mut files, &action, Some(repo.path()), false, true);
        if ws.fatal {
            return Err(GitError::Other(
                ws.summary
                    .trim_end()
                    .trim_start_matches("error: ")
                    .to_owned(),
            ));
        }
        out.push_str(&ws.summary);
        let mut merged = false;
        let result = match apply::apply(repo, &files, &opts) {
            Err(_) if self.threeway => {
                merged = true;
                self.fall_back(repo, &files, &opts, out)
                    .map(|()| String::new())
            }
            r => r,
        };
        repo.index()?.read(true)?;
        match result {
            Ok(_) => Ok(merged),
            Err(e) if merged => Err(e),
            Err(e) => {
                // git names the file and the hunk's line before the failure.
                let e = e.to_string();
                let text = match (
                    e.split_once(": patch does not apply"),
                    e.find("hunk at line "),
                ) {
                    (Some((name, _)), Some(at)) => {
                        let line: String = e[at + 13..]
                            .chars()
                            .take_while(char::is_ascii_digit)
                            .collect();
                        format!(
                            "error: patch failed: {name}:{line}\nerror: {name}: patch does not apply"
                        )
                    }
                    _ => format!("error: {e}"),
                };
                Err(GitError::Other(text))
            }
        }
    }

    /// git's fall_back_threeway: rebuild the patch's base from the blobs its
    /// `index` lines name, apply it there, and merge that into HEAD as ort
    /// would, with its report.
    fn fall_back(
        &self,
        repo: &Repository,
        files: &[apply::FilePatch],
        opts: &ApplyOpts,
        out: &mut String,
    ) -> Result<(), GitError> {
        let idx = self.path("patch-merge-index");
        let fake = ApplyOpts {
            fake_ancestor: Some(idx.clone()),
            ..opts.clone()
        };
        apply::apply(repo, files, &fake)
            .map_err(|_| GitError::Other("error: could not build fake ancestor".into()))?;
        let base = git2::Index::open(&idx)?.write_tree_to(repo)?;
        let _ = std::fs::remove_file(&idx);
        let base = repo.find_tree(base)?;
        self.say(out, "Using index info to reconstruct a base tree...");
        let ours = match repo.head().ok().and_then(|h| h.peel_to_tree().ok()) {
            Some(t) => t,
            None => repo.find_tree(repo.treebuilder(None)?.write()?)?,
        };
        if !self.quiet {
            for d in repo
                .diff_tree_to_tree(Some(&ours), Some(&base), None)?
                .deltas()
            {
                let kind = match d.status() {
                    git2::Delta::Added => 'A',
                    git2::Delta::Modified => 'M',
                    _ => continue,
                };
                let path = d.new_file().path().unwrap_or(Path::new("")).display();
                let _ = writeln!(out, "{kind}\t{path}");
            }
        }
        let text: String = files.iter().map(apply::FilePatch::render).collect();
        let theirs = git2::Diff::from_buffer(text.as_bytes())
            .and_then(|d| repo.apply_to_tree(&base, &d, None))
            .and_then(|mut i| i.write_tree_to(repo))
            .map_err(|_| {
                GitError::Other(
                    "error: Did you hand edit your patch?\nIt does not apply to blobs recorded \
                     in its index."
                        .into(),
                )
            })?;
        let theirs = repo.find_tree(theirs)?;
        self.say(out, "Falling back to patching base and 3-way merge...");
        let mut merged = repo.merge_trees(&base, &ours, &theirs, None)?;
        let label = first_line(&self.msg).to_owned();
        for line in crate::git_repo::merge_report(repo, &merged, [&base, &ours, &theirs], &label)? {
            self.say(out, &line);
        }
        let labels = [
            "constructed merge base".to_owned(),
            "HEAD".to_owned(),
            label,
        ];
        crate::git_repo::checkout_merged(repo, &mut merged, &ours, "am", Some(&labels))?;
        if merged.has_conflicts() {
            out.push_str(&crate::rerere::report(repo, self.rerere));
            return Err(GitError::Other(
                "error: Failed to merge in the changes.".into(),
            ));
        }
        Ok(())
    }

    /// Commit the index with the patch's author and message.
    fn commit(&self, repo: &Repository, out: &mut String) -> Result<(), GitError> {
        if !self.run.no_verify {
            crate::git_repo::run_hook_file(repo, "pre-applypatch", &[])?;
        }
        let script = self.read("author-script");
        let field = |key: &str| {
            script
                .lines()
                .find_map(|l| l.strip_prefix(&format!("{key}=")))
                .and_then(|v| sq_dequote(v).map(|(s, _)| s))
                .unwrap_or_default()
        };
        let (name, email, date) = (
            field("GIT_AUTHOR_NAME"),
            field("GIT_AUTHOR_EMAIL"),
            field("GIT_AUTHOR_DATE"),
        );
        let now = git2::Signature::now("x", "x")?.when();
        let when = if self.run.ignore_date || date.is_empty() {
            now
        } else {
            let (secs, zone) = crate::plumbing::parse_git_date(&date, now.offset_minutes())
                .ok_or_else(|| GitError::Other(format!("invalid date format: {date}")))?;
            git2::Time::new(secs, zone)
        };
        let author = git2::Signature::new(&name, &email, &when)?;
        let mut committer = repo.committer_from_env()?;
        if self.run.committer_date_is_author_date {
            committer = git2::Signature::new(
                committer.name().unwrap_or(""),
                committer.email().unwrap_or(""),
                &when,
            )?;
        }
        let tree = repo.find_tree(repo.index()?.write_tree()?)?;
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        if parent.is_none() {
            out.push_str("applying to an empty history\n");
        }
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        let key = crate::sign::commit_key(repo, self.run.gpg_sign.as_deref(), self.run.no_gpg_sign);
        let id = crate::sign::commit(
            repo,
            None,
            &author,
            &committer,
            &self.msg,
            &tree,
            &parents,
            key.as_deref(),
        )?;
        let log = format!("am: {}", first_line(&self.msg));
        match repo.head() {
            Ok(mut head) => {
                head.set_target(id, &log)?;
            }
            Err(_) => {
                let head = repo.find_reference("HEAD")?;
                let target = head
                    .symbolic_target()
                    .into_iter()
                    .flatten()
                    .next()
                    .unwrap_or("HEAD")
                    .to_owned();
                repo.reference(&target, id, true, &log)?;
            }
        }
        if !self.run.no_verify {
            let _ = crate::git_repo::run_hook_file(repo, "post-applypatch", &[]);
        }
        Ok(())
    }

    /// Move on to the next patch (git's `am_next`).
    fn next(&mut self, repo: &Repository) -> Result<(), GitError> {
        for f in ["author-script", "final-commit", "original-commit"] {
            let _ = std::fs::remove_file(self.path(f));
        }
        let head = repo.head().ok().and_then(|h| h.target());
        self.write(
            "abort-safety",
            &head.map(|h| h.to_string()).unwrap_or_default(),
        )?;
        self.cur += 1;
        self.write("next", &format!("{}\n", self.cur))
    }

    fn destroy(&self) -> Result<(), GitError> {
        Ok(std::fs::remove_dir_all(&self.dir)?)
    }

    /// `--continue`: commit what the user resolved, then go on.
    fn resolve(
        &mut self,
        repo: &Repository,
        allow_empty: bool,
        out: &mut String,
    ) -> Result<(), GitError> {
        self.msg = self.read("final-commit");
        if self.msg.is_empty() || !self.path("author-script").exists() {
            return Err(GitError::Other(
                "cannot resume: .git/rebase-apply/final-commit does not exist.".into(),
            ));
        }
        self.say(out, &format!("Applying: {}", first_line(&self.msg)));
        repo.index()?.read(true)?;
        if dirty_index(repo)?.is_none() {
            if allow_empty && self.read("patch").is_empty() {
                out.push_str("No changes - recorded it as an empty commit.\n");
            } else {
                out.push_str(
                    "No changes - did you forget to use 'git add'?\n\
                     If there is nothing left to stage, chances are that something else\n\
                     already introduced the same changes; you might want to skip this patch.\n",
                );
                return Err(stop(self.resolve_hint(repo)));
            }
        }
        if repo.index()?.has_conflicts() {
            out.push_str(
                "You still have unmerged paths in your index.\n\
                 You should 'git add' each file with resolved conflicts to mark them as such.\n\
                 You might run `git rm` on a file to accept \"deleted by them\" for it.\n",
            );
            return Err(stop(self.resolve_hint(repo)));
        }
        crate::rerere::say(repo, None);
        if !self.run.interactive || self.interactive(repo)? {
            self.commit(repo, out)?;
        }
        self.next(repo)?;
        self.run(repo, false, out)
    }

    /// `--skip`: drop the current patch's changes and go on.
    fn skip(&mut self, repo: &Repository, out: &mut String) -> Result<(), GitError> {
        crate::rerere::clear(repo)?;
        if let Some(head) = repo.head().ok().and_then(|h| h.peel_to_commit().ok()) {
            reset_touched(repo, &head)?;
        }
        self.next(repo)?;
        self.run(repo, false, out)
    }

    /// `--abort`: back to ORIG_HEAD, unless HEAD moved since the stop.
    fn abort(&self, repo: &Repository, out: &mut String) -> Result<(), GitError> {
        let head = repo.head().ok().and_then(|h| h.target());
        let safety = Oid::from_str(self.read("abort-safety").trim()).ok();
        crate::rerere::clear(repo)?;
        if self.path("dirtyindex").exists() {
            return self.destroy();
        }
        if head != safety {
            out.push_str(
                "warning: You seem to have moved HEAD since the last 'am' failure.\n\
                 Not rewinding to ORIG_HEAD\n",
            );
            return self.destroy();
        }
        if let Some(h) = head {
            reset_touched(repo, &repo.find_commit(h)?)?;
        }
        match repo
            .rev_single("ORIG_HEAD")
            .and_then(|o| o.peel_to_commit())
        {
            Ok(orig) => {
                let mut co = git2::build::CheckoutBuilder::new();
                co.safe();
                repo.checkout_tree(orig.as_object(), Some(&mut co))?;
                match repo.head() {
                    Ok(mut r) => {
                        r.set_target(orig.id(), "am --abort")?;
                    }
                    Err(_) => {
                        let sym = repo.find_reference("HEAD")?;
                        let target = sym
                            .symbolic_target()
                            .into_iter()
                            .flatten()
                            .next()
                            .unwrap_or("HEAD")
                            .to_owned();
                        repo.reference(&target, orig.id(), true, "am --abort")?;
                    }
                }
            }
            Err(_) => {
                if let Ok(mut r) = repo.head() {
                    r.delete()?;
                }
            }
        }
        self.destroy()
    }

    /// `--show-current-patch`: the mail (`raw`) or its diff (`diff`).
    fn show(&self, part: &str) -> Result<String, GitError> {
        let file = match part {
            "raw" => self.msgnum(),
            "diff" => "patch".to_owned(),
            other => {
                return Err(GitError::Other(format!(
                    "invalid value for '--show-current-patch': '{other}'"
                )));
            }
        };
        Ok(String::from_utf8_lossy(&std::fs::read(self.path(&file))?).into_owned())
    }

    /// Ask on the terminal whether to apply the current patch (git's `-i`).
    fn interactive(&mut self, repo: &Repository) -> Result<bool, GitError> {
        use std::io::BufRead;
        loop {
            eprint!(
                "Commit Body is:\n--------------------------\n{}--------------------------\n\
                 Apply? [y]es/[n]o/[e]dit/[v]iew patch/[a]ccept all: ",
                self.msg
            );
            let mut reply = String::new();
            if std::io::stdin().lock().read_line(&mut reply)? == 0 {
                return Err(GitError::Other(
                    "unable to read from stdin; aborting".into(),
                ));
            }
            match reply.chars().next().map(|c| c.to_ascii_lowercase()) {
                Some('y') => return Ok(true),
                Some('a') => {
                    self.run.interactive = false;
                    return Ok(true);
                }
                Some('n') => return Ok(false),
                Some('e') => {
                    let edited = crate::git_repo::edit_message(
                        repo,
                        "rebase-apply/final-commit",
                        &self.msg,
                    )?;
                    self.write("final-commit", &edited)?;
                    self.msg = edited;
                }
                Some('v') => eprint!("{}", self.read("patch")),
                _ => {}
            }
        }
    }
}

fn first_line(msg: &str) -> &str {
    msg.lines().next().unwrap_or("")
}

/// The paths where the index differs from HEAD, or `None` when it matches.
fn dirty_index(repo: &Repository) -> Result<Option<String>, GitError> {
    let tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    let index = repo.index()?;
    let diff = repo.diff_tree_to_index(tree.as_ref(), Some(&index), None)?;
    let mut paths: Vec<String> = diff
        .deltas()
        .filter_map(|d| {
            d.new_file()
                .path()
                .or(d.old_file().path())
                .map(|p| p.display().to_string())
        })
        .collect();
    if index.has_conflicts() && paths.is_empty() {
        paths.push("unmerged paths".into());
    }
    Ok((!paths.is_empty()).then(|| paths.join(" ")))
}

/// Reset the index entries and files that differ from `head` back to it,
/// leaving other local changes alone (git's `clean_index` for --skip/--abort).
fn reset_touched(repo: &Repository, head: &git2::Commit<'_>) -> Result<(), GitError> {
    let tree = head.tree()?;
    let mut index = repo.index()?;
    index.read(true)?;
    let mut paths: Vec<String> = repo
        .diff_tree_to_index(Some(&tree), Some(&index), None)?
        .deltas()
        .flat_map(|d| [d.old_file().path(), d.new_file().path()])
        .flatten()
        .map(|p| p.display().to_string())
        .collect();
    for c in index.conflicts()?.flatten() {
        for e in [c.ancestor, c.our, c.their].into_iter().flatten() {
            paths.push(String::from_utf8_lossy(&e.path).into_owned());
        }
    }
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        return Ok(());
    }
    for p in &paths {
        let path = Path::new(p);
        index.remove_path(path)?;
        match tree.get_path(path) {
            Ok(entry) => {
                let blob = repo.find_blob(entry.id())?;
                index.add_frombuffer(
                    &git2::IndexEntry {
                        ctime: git2::IndexTime::new(0, 0),
                        mtime: git2::IndexTime::new(0, 0),
                        dev: 0,
                        ino: 0,
                        mode: entry.filemode() as u32,
                        uid: 0,
                        gid: 0,
                        file_size: 0,
                        id: entry.id(),
                        flags: 0,
                        flags_extended: 0,
                        path: p.as_bytes().to_vec(),
                    },
                    blob.content(),
                )?;
            }
            Err(_) => {
                if let Some(w) = repo.workdir() {
                    let _ = std::fs::remove_file(w.join(path));
                }
            }
        }
    }
    index.write()?;
    let mut co = git2::build::CheckoutBuilder::new();
    co.force().disable_pathspec_match(true);
    for p in &paths {
        co.path(p);
    }
    repo.checkout_index(Some(&mut index), Some(&mut co))?;
    Ok(())
}

/// Read every mailbox (or `stdin`) into single mails, as git's mailsplit does.
fn read_mails(
    repo: &Repository,
    a: &Args,
    stdin: Option<&[u8]>,
    keep_cr: bool,
) -> Result<Vec<Vec<u8>>, GitError> {
    let workdir = repo.workdir().unwrap_or(repo.path());
    let sources: Vec<Option<PathBuf>> = if a.mboxes.is_empty() {
        vec![None]
    } else {
        a.mboxes
            .iter()
            .map(|m| (m != "-").then(|| workdir.join(m)))
            .collect()
    };
    let format = match &a.patch_format {
        Some(f) => f.clone(),
        None => detect_format(sources[0].as_deref())?
            .ok_or_else(|| GitError::Other("Patch format detection failed.".into()))?
            .to_owned(),
    };
    let read = |src: &Option<PathBuf>| -> Result<Vec<u8>, GitError> {
        match src {
            Some(p) => std::fs::read(p).map_err(|e| {
                GitError::Other(format!("could not open '{}' for reading: {e}", p.display()))
            }),
            None => Ok(stdin.unwrap_or_default().to_vec()),
        }
    };
    let mut mails = Vec::new();
    match format.as_str() {
        "mbox" | "mboxrd" => {
            let mboxrd = format == "mboxrd";
            for src in &sources {
                match src {
                    Some(p) if p.is_dir() => {
                        for f in maildir(p)? {
                            mails.extend(split_mbox(&std::fs::read(f)?, keep_cr, mboxrd, true));
                        }
                    }
                    src => mails.extend(split_mbox(&read(src)?, keep_cr, mboxrd, false)),
                }
            }
        }
        "stgit" => {
            for src in &sources {
                mails.push(stgit_to_mail(&read(src)?));
            }
        }
        "stgit-series" => {
            for src in &sources {
                let series = String::from_utf8_lossy(&read(src)?).into_owned();
                let dir = src.as_deref().and_then(Path::parent).unwrap_or(workdir);
                for name in series.lines().map(str::trim) {
                    if name.is_empty() || name.starts_with('#') {
                        continue;
                    }
                    mails.push(stgit_to_mail(&read(&Some(dir.join(name)))?));
                }
            }
        }
        "hg" => {
            for src in &sources {
                mails.push(hg_to_mail(&read(src)?)?);
            }
        }
        f => return Err(GitError::Other(format!("Invalid patch format: {f}"))),
    }
    Ok(mails)
}

/// A maildir's mails, `cur` and `new` together, in git's order (digit runs
/// compared as numbers), skipping dot files.
fn maildir(dir: &Path) -> Result<Vec<PathBuf>, GitError> {
    let mut names = Vec::new();
    for sub in ["cur", "new"] {
        let Ok(entries) = std::fs::read_dir(dir.join(sub)) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with('.') {
                names.push(format!("{sub}/{name}"));
            }
        }
    }
    names.sort_by(|a, b| maildir_cmp(a.as_bytes(), b.as_bytes()));
    Ok(names.into_iter().map(|n| dir.join(n)).collect())
}

/// git's maildir_filename_cmp.
fn maildir_cmp(mut a: &[u8], mut b: &[u8]) -> std::cmp::Ordering {
    let digits = |s: &[u8]| s.iter().take_while(|c| c.is_ascii_digit()).count();
    while let (Some(&x), Some(&y)) = (a.first(), b.first()) {
        if x.is_ascii_digit() && y.is_ascii_digit() {
            let (i, j) = (digits(a), digits(b));
            let num = |s: &[u8]| String::from_utf8_lossy(s).parse::<u128>().unwrap_or(0);
            let ord = num(&a[..i]).cmp(&num(&b[..j]));
            if ord.is_ne() {
                return ord;
            }
            (a, b) = (&a[i..], &b[j..]);
        } else {
            if x != y {
                return x.cmp(&y);
            }
            (a, b) = (&a[1..], &b[1..]);
        }
    }
    a.len().cmp(&b.len())
}

/// git's detect_patch_format for the first input (stdin and maildirs are
/// mbox); `None` when it cannot tell.
fn detect_format(first: Option<&Path>) -> Result<Option<&'static str>, GitError> {
    let Some(path) = first.filter(|p| !p.is_dir()) else {
        return Ok(Some("mbox"));
    };
    let data = std::fs::read(path).map_err(|e| {
        GitError::Other(format!(
            "could not open '{}' for reading: {e}",
            path.display()
        ))
    })?;
    let text = String::from_utf8_lossy(&data);
    let lines: Vec<&str> = text.lines().collect();
    let Some(at) = lines.iter().position(|l| !l.is_empty()) else {
        return Ok(None);
    };
    let (l1, l2, l3) = (
        lines[at],
        lines.get(at + 1).copied().unwrap_or(""),
        lines.get(at + 2).copied().unwrap_or(""),
    );
    if l1.starts_with("From ") || l1.starts_with("From: ") {
        return Ok(Some("mbox"));
    }
    if l1.starts_with("# This series applies on GIT commit") {
        return Ok(Some("stgit-series"));
    }
    if l1 == "# HG changeset patch" {
        return Ok(Some("hg"));
    }
    if l2.is_empty()
        && ["From:", "Author:", "Date:"]
            .iter()
            .any(|p| l3.starts_with(p))
    {
        return Ok(Some("stgit"));
    }
    // git's is_mail: every line up to the first blank one a header, from the top.
    let header = |l: &str| {
        l.find(':').is_some_and(|i| {
            i > 0
                && l[..i]
                    .bytes()
                    .all(|b| (33..=57).contains(&b) || (59..=126).contains(&b))
        })
    };
    let mail = lines
        .iter()
        .take_while(|l| !l.is_empty())
        .all(|l| l.starts_with([' ', '\t']) || header(l));
    Ok(mail.then_some("mbox"))
}

fn split_line(data: &[u8]) -> (&[u8], &[u8]) {
    match data.iter().position(|&b| b == b'\n') {
        Some(i) => (&data[..i], &data[i + 1..]),
        None => (data, &[]),
    }
}

/// git's stgit_patch_to_mail: the header lines become mail headers, the
/// first other line the subject.
fn stgit_to_mail(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut subject = false;
    let mut rest = data;
    while !rest.is_empty() {
        let (line, next) = split_line(rest);
        rest = next;
        let s = String::from_utf8_lossy(line);
        if s.trim().is_empty() {
            continue;
        } else if let Some(x) = s.strip_prefix("Author:") {
            out.extend(format!("From:{x}\n").bytes());
        } else if s.starts_with("From") || s.starts_with("Date") {
            out.extend(format!("{s}\n").bytes());
        } else if !subject {
            out.extend(format!("Subject: {s}\n").bytes());
            subject = true;
        } else {
            out.extend(format!("\n{s}\n").bytes());
            break;
        }
    }
    out.extend(rest);
    out
}

/// git's hg_patch_to_mail: `# User` and `# Date` become From and Date.
fn hg_to_mail(data: &[u8]) -> Result<Vec<u8>, GitError> {
    let bad = |what: &str| GitError::Other(format!("error: {what}\nFailed to split patches."));
    let mut out = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let (line, next) = split_line(rest);
        rest = next;
        let s = String::from_utf8_lossy(line);
        if let Some(user) = s.strip_prefix("# User ") {
            out.extend(format!("From: {user}\n").bytes());
        } else if let Some(date) = s.strip_prefix("# Date ") {
            let (secs, tz) = date
                .split_once(' ')
                .ok_or_else(|| bad("invalid Date line"))?;
            let secs: i64 = secs.parse().map_err(|_| bad("invalid timestamp"))?;
            let tz: i64 = tz.parse().map_err(|_| bad("invalid Date line"))?;
            // hg's zone is seconds west of UTC; git's east, in whole minutes.
            let minutes = (tz.abs() / 3600 * 60 + tz.abs() % 3600 / 60) as i32;
            let offset = if tz > 0 { -minutes } else { minutes };
            let when = crate::git_repo::rfc2822_date(git2::Time::new(secs, offset));
            out.extend(format!("Date: {when}\n").bytes());
        } else if s.starts_with("# ") {
            continue;
        } else {
            out.extend(format!("\n{s}\n").bytes());
            break;
        }
    }
    out.extend(rest);
    Ok(out)
}

/// git's `is_from_line`: `From ` and something that ends like a date.
pub(crate) fn is_from_line(line: &[u8]) -> bool {
    let len = line.len();
    if len < 20 || !line.starts_with(b"From ") {
        return false;
    }
    let mut colon = len - 2;
    loop {
        if colon < 5 {
            return false;
        }
        colon -= 1;
        if line[colon] == b':' {
            break;
        }
    }
    let digit = |i: usize| line.get(i).is_some_and(u8::is_ascii_digit);
    if colon < 4
        || ![colon - 4, colon - 2, colon - 1, colon + 1, colon + 2]
            .into_iter()
            .all(digit)
    {
        return false;
    }
    let year: String = String::from_utf8_lossy(line.get(colon + 3..).unwrap_or_default())
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    year.parse::<i64>().unwrap_or(0) > 90
}

/// Split an mbox into mails at `From ` lines (not in a `bare` mail), fixing
/// CRLF endings unless `keep_cr` and unescaping `>From ` for mboxrd.
pub(crate) fn split_mbox(data: &[u8], keep_cr: bool, mboxrd: bool, bare: bool) -> Vec<Vec<u8>> {
    let start = data
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(data.len());
    let data = &data[start..];
    if data.is_empty() {
        return Vec::new();
    }
    let mut lines = data.split_inclusive(|&b| b == b'\n').peekable();
    let bare = bare || !lines.peek().is_some_and(|l| is_from_line(l));
    let mut mails = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    for line in lines {
        if !cur.is_empty() && !bare && is_from_line(line) {
            mails.push(std::mem::take(&mut cur));
        }
        let mut line = line;
        let crlf = !keep_cr && line.ends_with(b"\r\n");
        if crlf {
            line = &line[..line.len() - 2];
        }
        let gt = line.iter().take_while(|&&b| b == b'>').count();
        if mboxrd && gt > 0 && line[gt..].starts_with(b"From ") {
            line = &line[1..];
        }
        cur.extend_from_slice(line);
        if crlf {
            cur.push(b'\n');
        }
    }
    mails.push(cur);
    mails
}

/// What mailinfo reads from a mail.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Mail {
    pub(crate) author: String,
    pub(crate) email: String,
    pub(crate) subject: String,
    pub(crate) date: String,
    /// The message body, before the patch.
    pub(crate) msg: String,
    pub(crate) patch: Vec<u8>,
}

pub(crate) struct MailOpts {
    pub(crate) keep_subject: bool,
    pub(crate) keep_non_patch: bool,
    pub(crate) message_id: bool,
    pub(crate) scissors: bool,
}

/// The headers mailinfo keeps, primary (mail) or secondary (in-body).
#[derive(Default, Clone)]
struct Headers {
    from: Option<String>,
    subject: Option<String>,
    date: Option<String>,
}

impl Headers {
    fn slot(&mut self, i: usize) -> &mut Option<String> {
        match i {
            0 => &mut self.from,
            1 => &mut self.subject,
            _ => &mut self.date,
        }
    }
}

const HEADERS: [&str; 3] = ["From", "Subject", "Date"];

/// The value of `line` if it is header `name` (case-insensitive), as git's
/// parse_header takes it: after the colon and one more character.
fn header_value<'a>(line: &'a [u8], name: &str) -> Option<&'a [u8]> {
    let n = name.len();
    (line.len() > n && line[..n].eq_ignore_ascii_case(name.as_bytes()) && line[n] == b':')
        .then(|| line.get(n + 2..).unwrap_or_default())
}

/// A part's content type and transfer encoding.
#[derive(Default, Clone)]
struct PartInfo {
    boundary: Option<String>,
    charset: String,
    encoding: String,
}

/// A `key=value` parameter of a Content-Type header.
fn content_param(value: &str, key: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    let at = lower.find(&format!("{key}="))? + key.len() + 1;
    let rest = &value[at..];
    Some(match rest.strip_prefix('"') {
        Some(q) => q.split('"').next().unwrap_or("").to_owned(),
        None => rest
            .split(|c: char| c == ';' || c.is_whitespace())
            .next()
            .unwrap_or("")
            .to_owned(),
    })
}

/// Read the header block at the start of `lines`: unfolded header lines, then
/// the index of the first body line.
fn read_headers(lines: &[&[u8]]) -> (Vec<Vec<u8>>, usize) {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = trim_end(lines[i]);
        if line.is_empty() || !is_rfc2822_header(line) {
            break;
        }
        let mut h = line.to_vec();
        i += 1;
        while let Some(next) = lines
            .get(i)
            .filter(|l| l.starts_with(b" ") || l.starts_with(b"\t"))
        {
            h.push(b'\n');
            h.extend_from_slice(trim_end(&next[1..]));
            i += 1;
        }
        out.push(h);
    }
    (out, i)
}

fn trim_end(s: &[u8]) -> &[u8] {
    let end = s
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(0, |i| i + 1);
    &s[..end]
}

fn is_rfc2822_header(line: &[u8]) -> bool {
    if line.starts_with(b"From ") || line.starts_with(b">From ") {
        return true;
    }
    for &c in line {
        if c == b':' {
            return true;
        }
        if !((33..=57).contains(&c) || (59..=126).contains(&c)) {
            break;
        }
    }
    false
}

/// Parse a mail as git's mailinfo does.
pub(crate) fn mailinfo(mail: &[u8], o: &MailOpts) -> Mail {
    let start = mail
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(mail.len());
    let lines: Vec<&[u8]> = mail[start..].split_inclusive(|&b| b == b'\n').collect();
    let (headers, body_at) = read_headers(&lines);
    let mut primary = Headers::default();
    let mut top = PartInfo::default();
    let mut message_id = None;
    for h in &headers {
        if let Some((i, v)) = HEADERS
            .iter()
            .enumerate()
            .find_map(|(i, n)| header_value(h, n).map(|v| (i, v)))
        {
            *primary.slot(i) = Some(decode_header(v));
        } else if let Some(v) = header_value(h, "Content-Type") {
            let v = String::from_utf8_lossy(v);
            top.boundary = content_param(&v, "boundary");
            top.charset = content_param(&v, "charset").unwrap_or_default();
        } else if let Some(v) = header_value(h, "Content-Transfer-Encoding") {
            top.encoding = String::from_utf8_lossy(v).trim().to_ascii_lowercase();
        } else if let Some(v) = header_value(h, "Message-ID").filter(|_| o.message_id) {
            message_id = Some(String::from_utf8_lossy(v).trim().to_owned());
        }
    }
    let body: Vec<u8> = lines[body_at..].concat();
    // Each text part's charset and decoded content.
    let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
    match &top.boundary {
        Some(b) => {
            for raw in multipart(&body, b) {
                let lines: Vec<&[u8]> = raw.split_inclusive(|&c| c == b'\n').collect();
                let (hs, at) = read_headers(&lines);
                let (mut charset, mut encoding) = (top.charset.clone(), String::new());
                for h in hs {
                    if let Some(v) = header_value(&h, "Content-Type") {
                        charset = content_param(&String::from_utf8_lossy(v), "charset")
                            .unwrap_or_default();
                    } else if let Some(v) = header_value(&h, "Content-Transfer-Encoding") {
                        encoding = String::from_utf8_lossy(v).trim().to_ascii_lowercase();
                    }
                }
                let rest = lines[at..].concat();
                // The blank line after a part's headers is not its content.
                let rest = rest
                    .strip_prefix(b"\n")
                    .map_or(rest.clone(), <[u8]>::to_vec);
                parts.push((charset, decode_body(&rest, &encoding)));
            }
        }
        None => parts.push((top.charset.clone(), decode_body(&body, &top.encoding))),
    }

    let mut secondary = Headers::default();
    let mut log = String::new();
    let mut patch: Vec<u8> = Vec::new();
    let mut in_patch = false;
    let mut header_stage = true;
    let mut accumulated: Vec<u8> = Vec::new();
    for (charset, data) in &parts {
        for line in data.split_inclusive(|&b| b == b'\n') {
            if in_patch {
                patch.extend_from_slice(line);
                continue;
            }
            if header_stage && line.iter().all(|b| *b == b'\n') {
                if !accumulated.is_empty() {
                    flush_inbody(&mut accumulated, &mut secondary);
                    header_stage = false;
                }
                continue;
            }
            if header_stage {
                header_stage = inbody_header(line, &mut accumulated, &mut secondary, o.scissors);
                if header_stage {
                    continue;
                }
            }
            let text = to_utf8(line, charset);
            if o.scissors && is_scissors_line(&text) {
                log.clear();
                header_stage = true;
                secondary = Headers::default();
                continue;
            }
            if patchbreak(&text) {
                if let Some(id) = &message_id {
                    let _ = writeln!(log, "Message-ID: {id}");
                }
                in_patch = true;
                patch.extend_from_slice(line);
                continue;
            }
            log.push_str(&text);
        }
    }
    flush_inbody(&mut accumulated, &mut secondary);

    // In-body headers count only when there is a patch.
    let has_patch = !patch.is_empty();
    let pick = |s: Option<String>, p: Option<String>| if has_patch && s.is_some() { s } else { p };
    let mut m = Mail {
        msg: log,
        patch,
        ..Default::default()
    };
    if let Some(from) = pick(secondary.from, primary.from) {
        let (name, email) = handle_from(&cleanup_space(&from));
        m.author = name;
        m.email = email;
    }
    if let Some(subject) = pick(secondary.subject, primary.subject) {
        m.subject = if o.keep_subject {
            subject
        } else {
            cleanup_space(&cleanup_subject(&subject, o.keep_non_patch))
        };
    }
    if let Some(date) = pick(secondary.date, primary.date) {
        m.date = cleanup_space(&date);
    }
    m
}

/// The parts of a multipart body, between its `--boundary` lines.
fn multipart(body: &[u8], boundary: &str) -> Vec<Vec<u8>> {
    let open = format!("--{boundary}");
    let close = format!("{open}--");
    let mut parts = Vec::new();
    let mut cur: Option<Vec<u8>> = None;
    for line in body.split_inclusive(|&c| c == b'\n') {
        let t = trim_end(line);
        if t == open.as_bytes() || t == close.as_bytes() {
            parts.extend(cur.take());
            if t == open.as_bytes() {
                cur = Some(Vec::new());
            }
        } else if let Some(p) = cur.as_mut() {
            p.extend_from_slice(line);
        }
    }
    parts.extend(cur);
    parts
}

/// Store the in-body header gathered so far, unless one came before.
fn flush_inbody(acc: &mut Vec<u8>, sec: &mut Headers) {
    if acc.is_empty() {
        return;
    }
    let h = trim_end(acc).to_vec();
    for (i, n) in HEADERS.iter().enumerate() {
        // git keeps the subject's line end, which `-k` then prints as an
        // extra empty `Subject: ` line.
        let from: &[u8] = if *n == "Subject" { acc } else { &h };
        if let Some(v) = header_value(from, n)
            && sec.slot(i).is_none()
        {
            *sec.slot(i) = Some(decode_header(v));
        }
    }
    acc.clear();
}

/// One line at the top of the body while in-body headers may still come;
/// returns whether it was taken as a header.
fn inbody_header(line: &[u8], acc: &mut Vec<u8>, sec: &mut Headers, scissors: bool) -> bool {
    if !acc.is_empty() && (line.starts_with(b" ") || line.starts_with(b"\t")) {
        if scissors && is_scissors_line(&String::from_utf8_lossy(line)) {
            flush_inbody(acc, sec);
            return false;
        }
        if acc.ends_with(b"\n") {
            acc.pop();
        }
        acc.extend_from_slice(line);
        return true;
    }
    flush_inbody(acc, sec);
    if line.starts_with(b">From") && line.get(5).is_some_and(u8::is_ascii_whitespace) {
        return is_format_patch_separator(&line[1..]);
    }
    if line.starts_with(b"[PATCH]") && line.get(7).is_some_and(u8::is_ascii_whitespace) {
        sec.subject = Some(String::from_utf8_lossy(line).into_owned());
        return true;
    }
    if HEADERS.iter().any(|n| header_value(line, n).is_some()) {
        acc.extend_from_slice(line);
        return true;
    }
    false
}

/// A `From <sha> Mon Sep 17 00:00:00 2001` line as format-patch writes it.
fn is_format_patch_separator(line: &[u8]) -> bool {
    let text = String::from_utf8_lossy(line);
    let Some(rest) = text.strip_prefix("From ") else {
        return false;
    };
    let (sha, date) = rest.split_at(rest.find(' ').unwrap_or(rest.len()));
    sha.len() == 40
        && sha.bytes().all(|b| b.is_ascii_hexdigit())
        && date.trim_end() == " Mon Sep 17 00:00:00 2001"
}

/// git's `patchbreak`: where the message ends and the patch begins.
fn patchbreak(line: &str) -> bool {
    if line.starts_with("diff -") || line.starts_with("Index: ") {
        return true;
    }
    if line.len() < 4 || !line.starts_with("---") {
        return false;
    }
    let b = line.as_bytes();
    if b[3] == b' ' && !b[4].is_ascii_whitespace() {
        return true;
    }
    for &c in &b[3..] {
        if c == b'\n' {
            return true;
        }
        if !c.is_ascii_whitespace() {
            break;
        }
    }
    false
}

/// git's `is_scissors_line`: a `-- >8 --` perforation.
fn is_scissors_line(line: &str) -> bool {
    let b = line.as_bytes();
    let (mut scissors, mut gap, mut perforation) = (0, 0, 0);
    let (mut first, mut last) = (None, None);
    let mut in_perforation = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            if in_perforation {
                perforation += 1;
                gap += 1;
            }
            i += 1;
            continue;
        }
        last = Some(i);
        first.get_or_insert(i);
        if c == b'-' {
            in_perforation = true;
            perforation += 1;
            i += 1;
            continue;
        }
        let rest = &b[i..];
        if rest.starts_with(b">8")
            || rest.starts_with(b"8<")
            || rest.starts_with(b">%")
            || rest.starts_with(b"%<")
        {
            in_perforation = true;
            perforation += 2;
            scissors += 2;
            i += 2;
            continue;
        }
        in_perforation = false;
        i += 1;
    }
    let visible = match (first, last) {
        (Some(f), Some(l)) => l - f + 1,
        _ => 0,
    };
    scissors > 0 && 8 <= visible && visible < perforation * 3 && gap * 2 < perforation
}

/// git's `cleanup_subject`: drop leading `Re:`, `[PATCH ...]`, spaces and
/// colons (with `keep_non_patch`, other `[...]` stay).
fn cleanup_subject(subject: &str, keep_non_patch: bool) -> String {
    let mut s = subject.as_bytes().to_vec();
    let mut at = 0;
    while at < s.len() {
        match s[at] {
            b'r' | b'R' => {
                if s.len() <= at + 3 {
                    break;
                }
                if matches!(s[at + 1], b'e' | b'E') && s[at + 2] == b':' {
                    s.drain(at..at + 3);
                    continue;
                }
                break;
            }
            b' ' | b'\t' | b':' => {
                s.remove(at);
                continue;
            }
            b'[' => {
                let Some(close) = s[at..].iter().position(|&c| c == b']') else {
                    break;
                };
                let remove = close + 1;
                let is_patch = remove >= 7 && s[at..at + remove].windows(5).any(|w| w == b"PATCH");
                if !keep_non_patch || is_patch {
                    s.drain(at..at + remove);
                } else {
                    at += remove;
                    if s.get(at).is_some_and(u8::is_ascii_whitespace) {
                        at += 1;
                    }
                }
                continue;
            }
            _ => break,
        }
    }
    String::from_utf8_lossy(&s).trim().to_owned()
}

/// Each whitespace run as one space.
fn cleanup_space(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_ascii_whitespace() {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    out
}

/// The name and email of a From header, as git's handle_from reads them.
fn handle_from(from: &str) -> (String, String) {
    let f = unquote_quoted_pair(from);
    let Some(at) = f.find('@') else {
        // `John Doe <johndoe>` without an @.
        let Some(bra) = f.find('<') else {
            return (String::new(), String::new());
        };
        let Some(ket) = f[bra..].find('>') else {
            return (String::new(), String::new());
        };
        let email = f[bra + 1..bra + ket].to_owned();
        let name = f[..bra].trim().to_owned();
        return (sane_name(name, &email), email);
    };
    let mut f = f.into_bytes();
    let mut start = at;
    while start > 0 {
        let c = f[start - 1];
        if c.is_ascii_whitespace() {
            break;
        }
        if c == b'<' {
            f[start - 1] = b' ';
            break;
        }
        start -= 1;
    }
    let len = f[start..]
        .iter()
        .position(|c| b" \n\t\r\x0b\x0c>".contains(c))
        .unwrap_or(f.len() - start);
    let email = String::from_utf8_lossy(&f[start..start + len]).into_owned();
    let end = (start + len + usize::from(start + len < f.len())).min(f.len());
    f.drain(start..end);
    let mut name = cleanup_space(&String::from_utf8_lossy(&f))
        .trim()
        .to_owned();
    if name.starts_with('(') && name.ends_with(')') && name.len() >= 2 {
        name = name[1..name.len() - 1].to_owned();
    }
    (sane_name(name, &email), email)
}

fn sane_name(name: String, email: &str) -> String {
    if name.is_empty() || name.len() > 60 || name.contains(['@', '<', '>']) {
        email.to_owned()
    } else {
        name
    }
}

/// Unquote `"..."` strings and backslash pairs in comments, as git does.
fn unquote_quoted_pair(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => out.extend(chars.next()),
                        '"' => break,
                        c => out.push(c),
                    }
                }
            }
            '(' => {
                out.push('(');
                let mut depth = 1;
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            out.extend(chars.next());
                            continue;
                        }
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                out.push(')');
                                break;
                            }
                        }
                        _ => {}
                    }
                    out.push(c);
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// A header value with its RFC 2047 encoded words decoded to UTF-8; on a
/// malformed word the value stays as it was, as in git.
fn decode_header(value: &[u8]) -> String {
    let text = String::from_utf8_lossy(value).into_owned();
    let mut out: Vec<u8> = Vec::new();
    let mut rest = text.as_str();
    let mut first = true;
    while let Some(start) = rest.find("=?") {
        let before = &rest[..start];
        if first || !before.trim().is_empty() {
            out.extend_from_slice(before.as_bytes());
        }
        let word = &rest[start + 2..];
        let Some(q1) = word.find('?') else {
            return text;
        };
        let charset = &word[..q1];
        let enc = word[q1 + 1..]
            .chars()
            .next()
            .map(|c| c.to_ascii_lowercase());
        if word.as_bytes().get(q1 + 2) != Some(&b'?') {
            return text;
        }
        let payload_start = q1 + 3;
        let Some(end) = word[payload_start..].find("?=") else {
            return text;
        };
        let payload = &word[payload_start..payload_start + end];
        let bytes = match enc {
            Some('b') => base64(payload.as_bytes()),
            Some('q') => decode_q(payload.as_bytes(), true),
            _ => return text,
        };
        out.extend_from_slice(to_utf8(&bytes, charset).as_bytes());
        rest = &word[payload_start + end + 2..];
        first = false;
    }
    out.extend_from_slice(rest.as_bytes());
    String::from_utf8_lossy(&out).into_owned()
}

fn hexval(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

/// Quoted-printable (with `rfc2047`, `_` is a space); `=` at a line end joins.
fn decode_q(data: &[u8], rfc2047: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        let c = data[i];
        if c == b'=' {
            let rest = &data[i + 1..];
            if rest.starts_with(b"\r\n") {
                i += 3;
                continue;
            }
            if rest.starts_with(b"\n") {
                i += 2;
                continue;
            }
            if let (Some(h), Some(l)) = (
                rest.first().copied().and_then(hexval),
                rest.get(1).copied().and_then(hexval),
            ) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(if rfc2047 && c == b'_' { b' ' } else { c });
        i += 1;
    }
    out
}

fn base64(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for &c in data {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => continue,
        };
        acc = acc << 6 | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

fn decode_body(data: &[u8], encoding: &str) -> Vec<u8> {
    match encoding {
        "base64" => base64(data),
        "quoted-printable" => decode_q(data, false),
        _ => data.to_vec(),
    }
}

/// `bytes` in `charset` as UTF-8 (Latin-1 family byte for byte, else as UTF-8).
fn to_utf8(bytes: &[u8], charset: &str) -> String {
    match charset.to_ascii_lowercase().as_str() {
        "iso-8859-1" | "latin1" | "latin-1" | "iso-8859-15" | "windows-1252" | "cp1252" => {
            bytes.iter().map(|&b| b as char).collect()
        }
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// git's `sq_quote_buf`: single quotes, with `'` and `!` escaped outside them.
fn sq(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        if c == '\'' || c == '!' {
            out.push_str("'\\");
            out.push(c);
            out.push('\'');
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// One single-quoted word from the start of `s`, and what follows it.
fn sq_dequote(s: &str) -> Option<(String, &str)> {
    let mut rest = s.strip_prefix('\'')?;
    let mut out = String::new();
    loop {
        let end = rest.find('\'')?;
        out.push_str(&rest[..end]);
        rest = &rest[end + 1..];
        let b = rest.as_bytes();
        if b.len() >= 3 && b[0] == b'\\' && matches!(b[1], b'\'' | b'!') && b[2] == b'\'' {
            out.push(b[1] as char);
            rest = &rest[3..];
            continue;
        }
        return Some((out, rest));
    }
}

/// Every single-quoted word of `s` (git's `sq_dequote_to_strvec`).
fn sq_dequote_all(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s.trim_start();
    while let Some((word, next)) = sq_dequote(rest) {
        out.push(word);
        rest = next.trim_start();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailinfo_reads_headers_body_and_patch_like_git() {
        let mail = b"From 1234567890123456789012345678901234567890 Mon Sep 17 00:00:00 2001\n\
From: =?UTF-8?q?J=C3=BCrgen=20D?= <j@x.org>\n\
Date: Wed, 1 Jan 2020 00:00:00 +0000\n\
Subject: [PATCH 2/3] Re: fix\n the thing\n\
Message-ID: <m@x>\n\
\n\
From: Other One <o@x.org>\n\
Subject: real subject\n\
\n\
Body.\n\
---\n diffstat\n\ndiff --git a/f b/f\n";
        let m = mailinfo(
            mail,
            &MailOpts {
                keep_subject: false,
                keep_non_patch: false,
                message_id: true,
                scissors: false,
            },
        );
        assert_eq!(m.author, "Other One");
        assert_eq!(m.email, "o@x.org");
        assert_eq!(m.subject, "real subject");
        assert_eq!(m.msg, "Body.\nMessage-ID: <m@x>\n");
        assert!(m.patch.starts_with(b"---\n"));
        assert_eq!(
            cleanup_subject("Re: [PATCH v2 1/2] [tag] x", true),
            "[tag] x"
        );
        assert_eq!(
            decode_header(b"=?UTF-8?q?J=C3=BCrgen=20D?= <j@x>"),
            "J\u{fc}rgen D <j@x>"
        );
        assert!(is_scissors_line("-- >8 --\n"));
        assert!(!is_scissors_line("-- 8 --\n"));
        assert_eq!(sq("it's!"), "'it'\\''s'\\!''");
        assert_eq!(sq_dequote_all(" 'it'\\''s' '-p2'"), ["it's", "-p2"]);
        assert_eq!(
            split_mbox(
                b"From x Mon Sep 17 00:00:00 2001\na\nFrom y Mon Sep 17 00:00:00 2001\nb\n",
                false,
                false,
                false
            )
            .len(),
            2
        );
    }
}
