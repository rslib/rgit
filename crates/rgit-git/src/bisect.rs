//! `git bisect`, run natively with git's state files (`.git/BISECT_*` and
//! `refs/bisect/*`) and git's choice of commit at each step, so git and rgit
//! can take turns on the same bisect.

use crate::rev::RevParse;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::path::PathBuf;

use git2::{Oid, Repository};

use crate::GitError;

/// How a bisect command ended, as git's `enum bisect_error` success codes.
#[derive(PartialEq)]
enum Step {
    Ok,
    FirstBad,
    MergeBase,
    OnlySkipped,
}

#[derive(Clone)]
struct Terms {
    bad: String,
    good: String,
}

impl Default for Terms {
    fn default() -> Self {
        Terms {
            bad: "bad".into(),
            good: "good".into(),
        }
    }
}

/// The bad commit, the good ones (by ref name) and the skipped ones.
type Refs = (Option<Oid>, Vec<Oid>, Vec<Oid>);

fn fail(msg: impl Into<String>) -> GitError {
    GitError::Other(msg.into())
}

/// Run a `git bisect` subcommand: `start`, `bad`/`good`/`new`/`old` or a
/// custom term, `skip`, `next`, `reset`, `log`, `replay`, `run` or `terms`.
/// Returns what git prints; a failure carries it too.
pub(crate) fn run(repo: &Repository, args: &[String]) -> Result<String, GitError> {
    let mut b = Bisect {
        repo,
        out: String::new(),
        hidden: Vec::new(),
    };
    let res = b.command(args);
    let out = b.out.trim_end().to_owned();
    match res {
        Ok(Step::OnlySkipped) => Err(GitError::Cli(out)),
        Ok(_) => Ok(out),
        Err(e) => {
            let e = e.to_string();
            Err(GitError::Cli(match (out.is_empty(), e.is_empty()) {
                (true, _) => e,
                (false, true) => out,
                (false, false) => format!("{out}\n{e}"),
            }))
        }
    }
}

struct Bisect<'r> {
    repo: &'r Repository,
    out: String,
    /// The bottoms of `skip A..B` ranges: git's range walk leaves them marked
    /// uninteresting, so the step right after hides them too.
    hidden: Vec<Oid>,
}

impl Bisect<'_> {
    fn path(&self, name: &str) -> PathBuf {
        self.repo.path().join(name)
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.path(name)).unwrap_or_default()
    }

    fn exists(&self, name: &str) -> bool {
        self.path(name).exists()
    }

    fn append(&self, name: &str, text: &str) -> Result<(), GitError> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path(name))?;
        f.write_all(text.as_bytes())?;
        Ok(())
    }

    fn commit(&self, rev: &str) -> Option<Oid> {
        Some(self.repo.rev_single(rev).ok()?.peel_to_commit().ok()?.id())
    }

    fn subject(&self, oid: Oid) -> String {
        self.repo
            .find_commit(oid)
            .ok()
            .and_then(|c| c.summary().ok().flatten().map(str::to_owned))
            .unwrap_or_default()
    }

    fn read_oid(&self, name: &str) -> Option<Oid> {
        Oid::from_str(self.read(name).trim()).ok()
    }

    fn get_terms(&self) -> Option<Terms> {
        let text = self.read("BISECT_TERMS");
        let mut lines = text.lines();
        Some(Terms {
            bad: lines.next()?.to_owned(),
            good: lines.next()?.to_owned(),
        })
    }

    fn command(&mut self, args: &[String]) -> Result<Step, GitError> {
        let (cmd, rest) = args.split_first().ok_or_else(|| fail("need a command"))?;
        let mut terms = self.get_terms().unwrap_or_default();
        let one = |what: &str| {
            if rest.len() > 1 {
                Err(fail(format!(
                    "'{cmd}' requires either no argument or {what}"
                )))
            } else {
                Ok(rest.first().map(String::as_str))
            }
        };
        match cmd.as_str() {
            "start" => self.start(&mut Terms::default(), rest),
            "reset" => self.reset(one("a commit")?),
            "terms" => self.terms(one("an option")?),
            "next" if rest.is_empty() => self.next(&terms),
            "next" => Err(fail("'next' requires 0 arguments")),
            "log" => {
                let log = self.read("BISECT_LOG");
                if log.is_empty() {
                    return Err(fail("We are not bisecting."));
                }
                self.out.push_str(&log);
                Ok(Step::Ok)
            }
            "replay" => match rest {
                [file] => self.replay(&mut terms, file),
                _ => Err(fail("no logfile given")),
            },
            "skip" => self.skip(&mut terms, rest),
            "run" => self.run(&terms, rest),
            _ => {
                self.check_and_set_terms(&mut terms, cmd)?;
                if *cmd != terms.good && *cmd != terms.bad {
                    return Err(fail(format!("unknown command: '{cmd}'")));
                }
                self.state(&mut terms, args)
            }
        }
    }

    fn write_terms(&self, bad: &str, good: &str) -> Result<(), GitError> {
        if bad == good {
            return Err(fail("please use two different terms"));
        }
        for (term, orig) in [(bad, "bad"), (good, "good")] {
            if !git2::Reference::is_valid_name(&format!("refs/bisect/{term}")) {
                return Err(fail(format!("'{term}' is not a valid term")));
            }
            let builtin = [
                "help",
                "start",
                "skip",
                "next",
                "reset",
                "visualize",
                "view",
                "replay",
                "log",
                "run",
                "terms",
            ];
            if builtin.contains(&term) {
                return Err(fail(format!(
                    "can't use the builtin command '{term}' as a term"
                )));
            }
            if (orig != "bad" && ["bad", "new"].contains(&term))
                || (orig != "good" && ["good", "old"].contains(&term))
            {
                return Err(fail(format!(
                    "can't change the meaning of the term '{term}'"
                )));
            }
        }
        std::fs::write(self.path("BISECT_TERMS"), format!("{bad}\n{good}\n"))?;
        Ok(())
    }

    fn check_and_set_terms(&self, terms: &mut Terms, cmd: &str) -> Result<(), GitError> {
        if ["skip", "start", "terms"].contains(&cmd) {
            return Ok(());
        }
        let has_file = !self.read("BISECT_TERMS").is_empty();
        if has_file && cmd != terms.bad && cmd != terms.good {
            return Err(fail(format!(
                "Invalid command: you're currently in a {}/{} bisect",
                terms.bad, terms.good
            )));
        }
        if !has_file {
            let pair = match cmd {
                "bad" | "good" => ("bad", "good"),
                "new" | "old" => ("new", "old"),
                _ => return Ok(()),
            };
            *terms = Terms {
                bad: pair.0.into(),
                good: pair.1.into(),
            };
            self.write_terms(pair.0, pair.1)?;
        }
        Ok(())
    }

    fn start(&mut self, terms: &mut Terms, argv: &[String]) -> Result<Step, GitError> {
        let mut no_checkout = self.repo.is_bare();
        let mut first_parent = false;
        let mut must_write_terms = false;
        let has_double_dash = argv.iter().any(|a| a == "--");
        let mut revs = Vec::new();
        let mut i = 0;
        while i < argv.len() {
            let arg = argv[i].as_str();
            let value = |names: [&str; 2]| {
                names
                    .iter()
                    .find_map(|n| arg.strip_prefix(&format!("{n}=")).map(str::to_owned))
            };
            if arg == "--" {
                break;
            } else if arg == "--no-checkout" {
                no_checkout = true;
            } else if arg == "--first-parent" {
                first_parent = true;
            } else if matches!(
                arg,
                "--term-good" | "--term-old" | "--term-bad" | "--term-new"
            ) {
                i += 1;
                let t = argv.get(i).ok_or_else(|| fail("'' is not a valid term"))?;
                must_write_terms = true;
                if matches!(arg, "--term-good" | "--term-old") {
                    terms.good = t.clone();
                } else {
                    terms.bad = t.clone();
                }
            } else if let Some(t) = value(["--term-good", "--term-old"]) {
                must_write_terms = true;
                terms.good = t;
            } else if let Some(t) = value(["--term-bad", "--term-new"]) {
                must_write_terms = true;
                terms.bad = t;
            } else if arg.starts_with("--") {
                return Err(fail(format!("unrecognized option: '{arg}'")));
            } else if let Some(oid) = self.commit(arg) {
                revs.push(oid);
            } else if has_double_dash {
                return Err(fail(format!(
                    "'{arg}' does not appear to be a valid revision"
                )));
            } else {
                break;
            }
            i += 1;
        }
        let pathspec_pos = i;
        must_write_terms |= !revs.is_empty();

        let head = self.repo.find_reference("HEAD")?;
        let head_oid = self.repo.refname_to_id("HEAD").ok();
        let mut start_head = self.read("BISECT_START").trim().to_owned();
        if !start_head.is_empty() {
            if !no_checkout && self.checkout(&start_head).is_err() {
                return Err(fail(format!(
                    "checking out '{start_head}' failed. Try 'git bisect start <valid-branch>'."
                )));
            }
        } else {
            let head_oid = head_oid.ok_or_else(|| fail("bad HEAD - I need a HEAD"))?;
            start_head = match head.symbolic_target().ok().flatten() {
                None => head_oid.to_string(),
                Some(t) => t
                    .strip_prefix("refs/heads/")
                    .ok_or_else(|| fail("bad HEAD - strange symbolic ref"))?
                    .to_owned(),
            };
        }

        self.clean_state()?;
        std::fs::write(self.path("BISECT_START"), format!("{start_head}\n"))?;
        if first_parent {
            std::fs::write(self.path("BISECT_FIRST_PARENT"), "\n")?;
        }
        if no_checkout {
            let oid = self
                .commit(&start_head)
                .ok_or_else(|| fail(format!("invalid ref: '{start_head}'")))?;
            std::fs::write(self.path("BISECT_HEAD"), format!("{oid}\n"))?;
        }
        let names = if pathspec_pos + 1 < argv.len() {
            sq_quote_argv(&argv[pathspec_pos..])
        } else {
            String::new()
        };
        std::fs::write(self.path("BISECT_NAMES"), format!("{names}\n"))?;
        for (n, oid) in revs.iter().enumerate() {
            let state = if n == 0 { &terms.bad } else { &terms.good };
            self.write(state, &oid.to_string(), terms, true)?;
        }
        if must_write_terms {
            self.write_terms(&terms.bad, &terms.good)?;
        }
        self.append(
            "BISECT_LOG",
            &format!("git bisect start{}\n", sq_quote_argv(argv)),
        )?;
        let res = self.auto_next(terms);
        if !matches!(res, Ok(Step::Ok | Step::FirstBad | Step::MergeBase)) {
            self.clean_state()?;
        }
        res
    }

    /// Record `rev` as `state` in refs/bisect and the log.
    fn write(&self, state: &str, rev: &str, terms: &Terms, nolog: bool) -> Result<(), GitError> {
        let tag = if state == terms.bad {
            format!("refs/bisect/{state}")
        } else if state == terms.good || state == "skip" {
            format!("refs/bisect/{state}-{rev}")
        } else {
            return Err(fail(format!("Bad bisect_write argument: {state}")));
        };
        let oid = self
            .repo
            .rev_single(rev)
            .map_err(|_| fail(format!("couldn't get the oid of the rev '{rev}'")))?
            .id();
        self.repo.reference(&tag, oid, true, "")?;
        let commit = self.repo.find_object(oid, None)?.peel_to_commit()?.id();
        let mut log = format!("# {state}: [{commit}] {}\n", self.subject(commit));
        if !nolog {
            let _ = writeln!(log, "git bisect {state} {rev}");
        }
        self.append("BISECT_LOG", &log)
    }

    fn autostart(&self) -> Result<(), GitError> {
        if self.read("BISECT_START").is_empty() {
            return Err(fail("You need to start by \"git bisect start\"\n"));
        }
        Ok(())
    }

    fn state(&mut self, terms: &mut Terms, argv: &[String]) -> Result<Step, GitError> {
        self.autostart()?;
        let state = argv[0].as_str();
        self.check_and_set_terms(terms, state)?;
        if state != terms.good && state != terms.bad && state != "skip" {
            return Err(fail(""));
        }
        let revs = &argv[1..];
        if revs.len() > 1 && state == terms.bad {
            return Err(fail(format!(
                "'git bisect {}' can take only one argument.",
                terms.bad
            )));
        }
        let mut oids = Vec::new();
        if revs.is_empty() {
            let head = if self.exists("BISECT_HEAD") {
                "BISECT_HEAD"
            } else {
                "HEAD"
            };
            let oid = match head {
                "HEAD" => self.repo.refname_to_id("HEAD").ok(),
                _ => self.read_oid(head),
            };
            oids.push(oid.ok_or_else(|| fail(format!("Bad rev input: {head}")))?);
        }
        for rev in revs {
            let obj = self
                .repo
                .rev_single(rev)
                .map_err(|_| fail(format!("Bad rev input: {rev}")))?;
            let commit = obj
                .peel_to_commit()
                .map_err(|_| fail(format!("Bad rev input (not a commit): {rev}")))?;
            oids.push(commit.id());
        }
        let mut expected = self.read_oid("BISECT_EXPECTED_REV");
        for oid in oids {
            self.write(state, &oid.to_string(), terms, false)?;
            if expected.is_some_and(|e| e != oid) {
                let _ = std::fs::remove_file(self.path("BISECT_ANCESTORS_OK"));
                let _ = std::fs::remove_file(self.path("BISECT_EXPECTED_REV"));
                expected = None;
            }
        }
        self.auto_next(terms)
    }

    fn skip(&mut self, terms: &mut Terms, argv: &[String]) -> Result<Step, GitError> {
        let mut words = vec!["skip".to_owned()];
        for arg in argv {
            if !arg.contains("..") {
                words.push(arg.clone());
                continue;
            }
            let (from, to) = arg.split_once("..").unwrap_or_default();
            let hide: Vec<Oid> = self
                .commit(if from.is_empty() { "HEAD" } else { from })
                .into_iter()
                .collect();
            let tip = self
                .commit(if to.is_empty() { "HEAD" } else { to })
                .ok_or_else(|| fail(format!("bad revision '{arg}'")))?;
            let walk = Graph::walk(self.repo, &[tip], &hide, false, &[])?;
            words.extend(walk.list.iter().rev().map(Oid::to_string));
            self.hidden.extend(hide);
        }
        self.state(terms, &words)
    }

    fn bisect_refs(&self, terms: &Terms) -> Result<Refs, GitError> {
        let bad = self
            .repo
            .refname_to_id(&format!("refs/bisect/{}", terms.bad))
            .ok();
        let mut refs: Vec<(String, Oid)> = Vec::new();
        for r in self.repo.references_glob("refs/bisect/*")? {
            let r = r?;
            if let (Ok(name), Some(oid)) = (r.name(), r.target()) {
                refs.push((name.to_owned(), oid));
            }
        }
        refs.sort();
        let with = |prefix: String| -> Vec<Oid> {
            refs.iter()
                .filter(|(n, _)| n.starts_with(&prefix))
                .map(|(_, o)| *o)
                .collect()
        };
        let goods = with(format!("refs/bisect/{}-", terms.good));
        let skips = with("refs/bisect/skip-".to_owned());
        Ok((bad, goods, skips))
    }

    fn next_check(&mut self, terms: &Terms, current: Option<&str>) -> Result<(), GitError> {
        let (bad, goods, _) = self.bisect_refs(terms)?;
        let (missing_good, missing_bad) = (goods.is_empty(), bad.is_none());
        if !missing_good && !missing_bad {
            return Ok(());
        }
        let Some(current) = current else {
            return Err(fail(""));
        };
        if missing_good && !missing_bad && current == terms.good {
            let _ = writeln!(
                self.out,
                "warning: bisecting only with a {} commit",
                terms.bad
            );
            return Ok(());
        }
        let need = "You need to give me at least one bad|new and good|old revision.\nYou can \
                    use \"git bisect bad|new\" and \"git bisect good|old\" for that.";
        Err(fail(if self.read("BISECT_START").is_empty() {
            format!(
                "You need to start by \"git bisect start\".\n{}",
                need.replacen("You need", "You then need", 1)
            )
        } else {
            need.to_owned()
        }))
    }

    fn print_status(&mut self, terms: &Terms) -> Result<(), GitError> {
        let (bad, goods, _) = self.bisect_refs(terms)?;
        let text = match (bad.is_some(), goods.len()) {
            (true, n) if n > 0 => return Ok(()),
            // git 2.55+ quotes the terms in every status message.
            (false, 0) => format!(
                "status: waiting for both '{}' and '{}' commits",
                terms.good, terms.bad
            ),
            (false, 1) => format!(
                "status: waiting for '{}' commit, 1 '{}' commit known",
                terms.bad, terms.good
            ),
            (false, n) => format!(
                "status: waiting for '{}' commit, {n} '{}' commits known",
                terms.bad, terms.good
            ),
            (true, _) => format!(
                "status: waiting for '{}' commit(s), '{}' commit known",
                terms.good, terms.bad
            ),
        };
        let _ = writeln!(self.out, "{text}");
        self.append("BISECT_LOG", &format!("# {text}\n"))
    }

    fn auto_next(&mut self, terms: &Terms) -> Result<Step, GitError> {
        if self.next_check(terms, None).is_err() {
            self.print_status(terms)?;
            return Ok(Step::Ok);
        }
        self.next(terms)
    }

    fn next(&mut self, terms: &Terms) -> Result<Step, GitError> {
        self.autostart()?;
        self.next_check(terms, Some(&terms.good))?;
        let res = self.next_all(terms)?;
        let bad = self.bisect_refs(terms)?.0;
        match (&res, bad) {
            (Step::FirstBad, Some(bad)) => self.append(
                "BISECT_LOG",
                &format!(
                    "# first '{}' commit: [{bad}] {}\n",
                    terms.bad,
                    self.subject(bad)
                ),
            )?,
            (Step::OnlySkipped, Some(bad)) => {
                let (_, goods, _) = self.bisect_refs(terms)?;
                let mut log = "# only skipped commits left to test\n".to_owned();
                let hide: Vec<Oid> = goods.iter().chain(&self.hidden).copied().collect();
                for oid in Graph::walk(self.repo, &[bad], &hide, false, &[])?
                    .list
                    .into_iter()
                    .rev()
                {
                    let _ = writeln!(
                        log,
                        "# possible first '{}' commit: [{oid}] {}",
                        terms.bad,
                        self.subject(oid)
                    );
                }
                self.append("BISECT_LOG", &log)?;
            }
            _ => {}
        }
        Ok(res)
    }

    /// bisect.c's `bisect_next_all`: pick the next commit and check it out,
    /// or report the first bad one.
    fn next_all(&mut self, terms: &Terms) -> Result<Step, GitError> {
        let no_checkout = self.exists("BISECT_HEAD");
        let (bad, goods, skips) = self.bisect_refs(terms)?;
        let first_parent = self.exists("BISECT_FIRST_PARENT");
        let find_all = !skips.is_empty();
        let bad = bad.ok_or_else(|| fail(format!("a '{}' revision is needed", terms.bad)))?;
        if let Some(step) = self.check_ancestors(terms, bad, &goods, &skips, no_checkout)? {
            return Ok(step);
        }
        let paths: Vec<String> = self
            .read("BISECT_NAMES")
            .lines()
            .flat_map(|l| sq_dequote(l.trim()).unwrap_or_default())
            .collect();
        let hide: Vec<Oid> = goods.iter().chain(&self.hidden).copied().collect();
        let graph = Graph::walk(self.repo, &[bad], &hide, first_parent, &paths)?;
        let (list, reaches, all, _) = graph.find_bisection(find_all, first_parent);
        let (list, tried) = managed_skipped(list, &skips, bad);
        let Some(&rev) = list.first() else {
            if !tried.is_empty() {
                self.only_skipped(terms, &tried, None);
                return Ok(Step::OnlySkipped);
            }
            let _ = writeln!(
                self.out,
                "{bad} was both '{}' and '{}'",
                terms.good, terms.bad
            );
            return Err(fail(""));
        };
        if all == 0 {
            return Err(fail(
                "No testable commit found.\nMaybe you started with bad path arguments?",
            ));
        }
        if rev == bad {
            if !tried.is_empty() {
                self.only_skipped(terms, &tried, Some(bad));
                return Ok(Step::OnlySkipped);
            }
            let _ = writeln!(self.out, "{rev} is the first '{}' commit", terms.bad);
            self.show_commit(rev)?;
            return Ok(Step::FirstBad);
        }
        let nr = all - reaches - 1;
        let steps = estimate_steps(all);
        let _ = writeln!(
            self.out,
            "Bisecting: {nr} revision{} left to test after this (roughly {steps} step{})",
            if nr == 1 { "" } else { "s" },
            if steps == 1 { "" } else { "s" }
        );
        self.checkout_step(rev, no_checkout)?;
        Ok(Step::Ok)
    }

    fn only_skipped(&mut self, terms: &Terms, tried: &[Oid], bad: Option<Oid>) {
        let _ = writeln!(
            self.out,
            "There are only 'skip'ped commits left to test.\nThe first '{}' commit could be any of:",
            terms.bad
        );
        for oid in tried.iter().chain(bad.as_ref()) {
            let _ = writeln!(self.out, "{oid}");
        }
        let _ = writeln!(self.out, "We cannot bisect more!");
    }

    /// `check_good_are_ancestors_of_bad`: a good commit off the bad one's
    /// history means a merge base must be tested first.
    fn check_ancestors(
        &mut self,
        terms: &Terms,
        bad: Oid,
        goods: &[Oid],
        skips: &[Oid],
        no_checkout: bool,
    ) -> Result<Option<Step>, GitError> {
        if self.path("BISECT_ANCESTORS_OK").is_file() || goods.is_empty() {
            return Ok(None);
        }
        let mut off = false;
        for &g in goods {
            off |= g != bad && !self.repo.graph_descendant_of(bad, g)?;
        }
        if off {
            let mut input = vec![bad];
            input.extend(goods);
            let bases = self.repo.merge_bases_many(&input)?;
            let good_hex = goods
                .iter()
                .map(Oid::to_string)
                .collect::<Vec<_>>()
                .join(" ");
            for &mb in bases.iter() {
                if mb == bad {
                    let (b, g) = (&terms.bad, &terms.good);
                    return Err(fail(if self.read_oid("BISECT_EXPECTED_REV") == Some(bad) {
                        match (b.as_str(), g.as_str()) {
                            ("bad", "good") => format!(
                                "The merge base {bad} is bad.\nThis means the bug has been fixed \
                                 between {bad} and [{good_hex}]."
                            ),
                            ("new", "old") => format!(
                                "The merge base {bad} is new.\nThe property has changed between \
                                 {bad} and [{good_hex}]."
                            ),
                            _ => format!(
                                "The merge base {bad} is '{b}'.\nThis means the first '{g}' commit \
                                 is between {bad} and [{good_hex}]."
                            ),
                        }
                    } else {
                        format!(
                            "Some '{g}' revs are not ancestors of the '{b}' rev.\ngit bisect \
                             cannot work properly in this case.\nMaybe you mistook '{g}' and \
                             '{b}' revs?"
                        )
                    }));
                } else if goods.contains(&mb) {
                    continue;
                } else if skips.contains(&mb) {
                    let _ = writeln!(
                        self.out,
                        "warning: the merge base between {bad} and [{good_hex}] must be \
                         skipped.\nSo we cannot be sure the first '{}' commit is between {mb} \
                         and {bad}.\nWe continue anyway.",
                        terms.bad
                    );
                } else {
                    let _ = writeln!(self.out, "Bisecting: a merge base must be tested");
                    self.checkout_step(mb, no_checkout)?;
                    return Ok(Some(Step::MergeBase));
                }
            }
        }
        std::fs::write(self.path("BISECT_ANCESTORS_OK"), "")?;
        Ok(None)
    }

    /// Move to the commit to test: check it out detached, or with
    /// `--no-checkout` only point BISECT_HEAD at it.
    fn checkout_step(&mut self, oid: Oid, no_checkout: bool) -> Result<(), GitError> {
        std::fs::write(self.path("BISECT_EXPECTED_REV"), format!("{oid}\n"))?;
        if no_checkout {
            std::fs::write(self.path("BISECT_HEAD"), format!("{oid}\n"))?;
        } else {
            self.checkout(&oid.to_string())?;
        }
        let _ = writeln!(self.out, "[{oid}] {}", self.subject(oid));
        Ok(())
    }

    /// `git checkout <name>`: a branch by name, anything else detached. Local
    /// changes that the switch would overwrite stop it.
    fn checkout(&self, name: &str) -> Result<(), GitError> {
        let refname = format!("refs/heads/{name}");
        let branch = self.repo.find_reference(&refname).is_ok();
        let rev = if branch { refname.as_str() } else { name };
        let commit = self.repo.rev_single(rev)?.peel_to_commit()?;
        self.repo.checkout_tree(
            commit.as_object(),
            Some(git2::build::CheckoutBuilder::new().safe()),
        )?;
        if branch {
            self.repo.set_head(&refname)?;
        } else {
            self.repo.set_head_detached(commit.id())?;
        }
        Ok(())
    }

    /// git's `show --stat --summary --no-abbrev-commit` of the first bad commit.
    fn show_commit(&mut self, oid: Oid) -> Result<(), GitError> {
        let c = self.repo.find_commit(oid)?;
        let _ = writeln!(self.out, "commit {oid}");
        if c.parent_count() > 1 {
            let ids: Vec<String> = c
                .parent_ids()
                .map(|p| p.to_string()[..7].to_owned())
                .collect();
            let _ = writeln!(self.out, "Merge: {}", ids.join(" "));
        }
        let a = c.author();
        let _ = writeln!(
            self.out,
            "Author: {} <{}>\nDate:   {}\n",
            a.name().unwrap_or(""),
            a.email().unwrap_or(""),
            crate::git_repo::format_git_date(a.when(), "default")
        );
        for line in c.message().unwrap_or("").trim_end().lines() {
            if line.is_empty() {
                self.out.push('\n');
            } else {
                let _ = writeln!(self.out, "    {line}");
            }
        }
        let old = c.parent(0).ok().map(|p| p.tree()).transpose()?;
        let mut diff = self
            .repo
            .diff_tree_to_tree(old.as_ref(), Some(&c.tree()?), None)?;
        diff.find_similar(None)?;
        if diff.deltas().len() > 0 {
            let format = git2::DiffStatsFormat::FULL | git2::DiffStatsFormat::INCLUDE_SUMMARY;
            let buf = diff.stats()?.to_buf(format, 80)?;
            self.out.push('\n');
            self.out.push_str(&String::from_utf8_lossy(&buf));
        }
        Ok(())
    }

    fn reset(&mut self, commit: Option<&str>) -> Result<Step, GitError> {
        let branch = match commit {
            None => {
                let b = self.read("BISECT_START").trim_end().to_owned();
                if b.is_empty() {
                    self.out.push_str("We are not bisecting.\n");
                    return Ok(Step::Ok);
                }
                b
            }
            Some(c) => {
                self.commit(c)
                    .ok_or_else(|| fail(format!("'{c}' is not a valid commit")))?;
                c.to_owned()
            }
        };
        if !self.exists("BISECT_HEAD") && self.checkout(&branch).is_err() {
            return Err(fail(format!(
                "could not check out original HEAD '{branch}'. Try 'git bisect reset <commit>'."
            )));
        }
        self.clean_state()?;
        Ok(Step::Ok)
    }

    fn clean_state(&self) -> Result<(), GitError> {
        let mut names = Vec::new();
        for r in self.repo.references_glob("refs/bisect/*")? {
            names.extend(r?.name().ok().map(str::to_owned));
        }
        for n in names {
            self.repo.find_reference(&n)?.delete()?;
        }
        for f in [
            "BISECT_HEAD",
            "BISECT_EXPECTED_REV",
            "BISECT_ANCESTORS_OK",
            "BISECT_LOG",
            "BISECT_NAMES",
            "BISECT_RUN",
            "BISECT_TERMS",
            "BISECT_FIRST_PARENT",
            "BISECT_START",
        ] {
            let _ = std::fs::remove_file(self.path(f));
        }
        Ok(())
    }

    fn terms(&mut self, option: Option<&str>) -> Result<Step, GitError> {
        let terms = self.get_terms().ok_or_else(|| fail("no terms defined"))?;
        let text = match option {
            None => format!(
                "Your current terms are {} for the old state\nand {} for the new state.",
                terms.good, terms.bad
            ),
            Some("--term-good" | "--term-old") => terms.good,
            Some("--term-bad" | "--term-new") => terms.bad,
            Some(o) => {
                return Err(fail(format!(
                    "invalid argument {o} for 'git bisect terms'.\nSupported options are: \
                     --term-good|--term-old and --term-bad|--term-new."
                )));
            }
        };
        let _ = writeln!(self.out, "{text}");
        Ok(Step::Ok)
    }

    fn replay(&mut self, terms: &mut Terms, file: &str) -> Result<Step, GitError> {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        if text.is_empty() {
            return Err(fail(format!("cannot read file '{file}' for replaying")));
        }
        if !self.read("BISECT_START").is_empty() {
            self.reset(None)?;
        }
        for line in text.lines() {
            let p = line.trim_start_matches([' ', '\t']);
            let Some(rest) = p
                .strip_prefix("git bisect")
                .or_else(|| p.strip_prefix("git-bisect"))
                .filter(|r| r.starts_with([' ', '\t']))
            else {
                continue;
            };
            let rest = rest.trim_start_matches([' ', '\t']);
            let (word, rev) = rest.split_once([' ', '\t']).unwrap_or((rest, ""));
            let rev = rev.trim_start_matches([' ', '\t']).trim_end();
            if let Some(t) = self.get_terms() {
                *terms = t;
            }
            self.check_and_set_terms(terms, word)?;
            if word == "start" {
                let argv = sq_dequote(rev).ok_or_else(|| fail(""))?;
                if self.start(terms, &argv)? != Step::Ok {
                    return Err(fail(""));
                }
            } else if word == terms.good || word == terms.bad || word == "skip" {
                self.write(word, rev, terms, false)?;
            } else if word == "terms" {
                let argv = sq_dequote(rev).unwrap_or_default();
                self.terms(argv.first().filter(|_| argv.len() == 1).map(String::as_str))?;
            } else {
                return Err(fail(format!("'{word}'?? what are you talking about?")));
            }
        }
        self.auto_next(terms)
    }

    fn run(&mut self, terms: &Terms, argv: &[String]) -> Result<Step, GitError> {
        if self.next_check(terms, None).is_err() {
            self.print_status(terms)?;
            return Err(fail(""));
        }
        if argv.is_empty() {
            return Err(fail("bisect run failed: no command provided."));
        }
        let command = sq_quote_argv(argv).trim_start().to_owned();
        let mut first = true;
        loop {
            let res = self.exec(&command);
            if first && (res == 126 || res == 127) {
                first = false;
                let rc = self.verify_good(terms, &command)?;
                if !(0..128).contains(&rc) {
                    return Err(fail(format!(
                        "unable to verify {command} on '{}' revision",
                        terms.good
                    )));
                }
                if rc == res {
                    return Err(fail(format!(
                        "bogus exit code {rc} for '{}' revision",
                        terms.good
                    )));
                }
            }
            if !(0..128).contains(&res) {
                return Err(fail(format!(
                    "bisect run failed: exit code {res} from {command} is < 0 or >= 128"
                )));
            }
            let state = match res {
                125 => "skip".to_owned(),
                0 => terms.good.clone(),
                _ => terms.bad.clone(),
            };
            let from = self.out.len();
            let step = self.state(&mut terms.clone(), std::slice::from_ref(&state));
            std::fs::write(self.path("BISECT_RUN"), &self.out[from..])?;
            match step {
                Ok(Step::Ok) => continue,
                Ok(Step::OnlySkipped) => {
                    return Err(fail("bisect run cannot continue any more"));
                }
                Ok(Step::MergeBase) => self.out.push_str("bisect run success\n"),
                Ok(Step::FirstBad) => {
                    let _ = writeln!(self.out, "bisect found first '{}' commit", terms.bad);
                }
                Err(e) => {
                    return Err(fail(format!(
                        "{e}\nbisect run failed: 'git bisect {state}' exited with error code 1"
                    )));
                }
            }
            return Ok(Step::Ok);
        }
    }

    /// Run `command` through the shell in the working tree, keeping its
    /// output in ours. Returns git's exit code (128+signal when killed).
    fn exec(&mut self, command: &str) -> i32 {
        let _ = writeln!(self.out, "running {command}");
        let dir = self.repo.workdir().unwrap_or(self.repo.path());
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .arg(command)
            .current_dir(dir)
            .stdin(std::process::Stdio::null())
            .output();
        let Ok(out) = out else {
            return -1;
        };
        self.out.push_str(&String::from_utf8_lossy(&out.stdout));
        self.out.push_str(&String::from_utf8_lossy(&out.stderr));
        #[cfg(unix)]
        if let Some(sig) = std::os::unix::process::ExitStatusExt::signal(&out.status) {
            return 128 + sig;
        }
        out.status.code().unwrap_or(-1)
    }

    /// Run `command` on a good commit, to tell a missing script (126/127)
    /// from a real result.
    fn verify_good(&mut self, terms: &Terms, command: &str) -> Result<i32, GitError> {
        let no_checkout = self.exists("BISECT_HEAD");
        let Some(&good) = self.bisect_refs(terms)?.1.first() else {
            return Ok(-1);
        };
        let current = if no_checkout {
            self.read_oid("BISECT_HEAD")
        } else {
            self.repo.refname_to_id("HEAD").ok()
        };
        let Some(current) = current else {
            return Ok(-1);
        };
        if self.checkout_step(good, no_checkout).is_err() {
            return Ok(-1);
        }
        let rc = self.exec(command);
        if self.checkout_step(current, no_checkout).is_err() {
            return Ok(-1);
        }
        Ok(rc)
    }
}

/// The commits a bisect step chooses from, as git's revision walk leaves them:
/// in walk order, with parents simplified by the pathspec.
struct Graph {
    list: Vec<Oid>,
    parents: HashMap<Oid, Vec<Oid>>,
    treesame: HashSet<Oid>,
    interesting: HashSet<Oid>,
}

impl Graph {
    /// git's `limit_list` over `tips ^goods -- paths`: commits in date order
    /// (ties first come, first served), each with its parents simplified.
    fn walk(
        repo: &Repository,
        tips: &[Oid],
        goods: &[Oid],
        first_parent: bool,
        paths: &[String],
    ) -> Result<Self, GitError> {
        let mut walk = repo.revwalk()?;
        for t in tips {
            walk.push(*t)?;
        }
        for g in goods {
            walk.hide(*g)?;
        }
        let interesting: HashSet<Oid> = walk.collect::<Result<_, _>>()?;
        let mut g = Graph {
            list: Vec::new(),
            parents: HashMap::new(),
            treesame: HashSet::new(),
            interesting,
        };
        if !tips.iter().any(|t| g.interesting.contains(t)) {
            return Ok(g);
        }
        let mut dates = HashMap::new();
        let mut date = |oid: Oid| -> Result<i64, GitError> {
            if let Some(d) = dates.get(&oid) {
                return Ok(*d);
            }
            let d = repo.find_commit(oid)?.time().seconds();
            dates.insert(oid, d);
            Ok(d)
        };
        let mut spec_opts = git2::DiffOptions::new();
        crate::pathspec::limit_diff(&mut spec_opts, paths)?;
        let mut same = |a: Option<&git2::Tree>, b: &git2::Tree| -> Result<bool, GitError> {
            let diff = repo.diff_tree_to_tree(a, Some(b), Some(&mut spec_opts))?;
            Ok(diff.deltas().len() == 0)
        };
        let mut seen: HashSet<Oid> = goods.iter().copied().collect();
        let mut queue = VecDeque::new();
        for t in tips {
            if seen.insert(*t) && g.interesting.contains(t) {
                let d = date(*t)?;
                let mut at = queue.len();
                for (i, q) in queue.iter().enumerate() {
                    if date(*q)? < d {
                        at = i;
                        break;
                    }
                }
                queue.insert(at, *t);
            }
        }
        while let Some(oid) = queue.pop_front() {
            let commit = repo.find_commit(oid)?;
            let mut parents: Vec<Oid> = commit.parent_ids().collect();
            if !paths.is_empty() {
                let tree = commit.tree()?;
                let relevant = |p: &Oid| g.interesting.contains(p) || goods.contains(p);
                if parents.is_empty() {
                    if same(None, &tree)? {
                        g.treesame.insert(oid);
                    }
                } else {
                    let (mut relevant_parents, mut relevant_change, mut irrelevant_change) =
                        (0, false, false);
                    let mut simplified = None;
                    for (n, p) in parents.iter().enumerate() {
                        if relevant(p) {
                            relevant_parents += 1;
                        }
                        if n == 1 && first_parent {
                            break;
                        }
                        if same(Some(&repo.find_commit(*p)?.tree()?), &tree)? {
                            if relevant(p) {
                                simplified = Some(*p);
                                break;
                            }
                        } else if relevant(p) {
                            relevant_change = true;
                        } else {
                            irrelevant_change = true;
                        }
                    }
                    if let Some(p) = simplified {
                        parents = vec![p];
                        g.treesame.insert(oid);
                    } else if if relevant_parents > 0 {
                        !relevant_change
                    } else {
                        !irrelevant_change
                    } {
                        g.treesame.insert(oid);
                    }
                }
            }
            for p in &parents {
                if seen.insert(*p) && g.interesting.contains(p) {
                    let d = date(*p)?;
                    let mut at = queue.len();
                    for (i, q) in queue.iter().enumerate() {
                        if date(*q)? < d {
                            at = i;
                            break;
                        }
                    }
                    queue.insert(at, *p);
                }
                if first_parent {
                    break;
                }
            }
            g.parents.insert(oid, parents);
            g.list.push(oid);
        }
        g.list.reverse();
        Ok(g)
    }

    fn interesting_parents(&self, oid: Oid, first_parent: bool) -> impl Iterator<Item = &Oid> {
        self.parents[&oid]
            .iter()
            .take(if first_parent { 1 } else { usize::MAX })
            .filter(|p| self.interesting.contains(p))
    }

    /// The commits `oid` reaches that are not yet `counted`, TREESAME ones aside.
    fn count_distance(&self, oid: Oid, counted: &mut HashSet<Oid>) -> i64 {
        let mut nr = 0;
        let mut cur = Some(oid);
        while let Some(c) = cur {
            if !self.interesting.contains(&c) || !counted.insert(c) {
                break;
            }
            if !self.treesame.contains(&c) {
                nr += 1;
            }
            let ps = self.parents.get(&c).map(Vec::as_slice).unwrap_or_default();
            cur = ps.first().copied();
            for p in ps.iter().skip(1) {
                nr += self.count_distance(*p, counted);
            }
        }
        nr
    }

    /// bisect.c's `find_bisection`: the best commit to test (or, to step
    /// around skipped ones, every commit best first), how many commits it
    /// reaches and how many are left.
    fn find_bisection(
        &self,
        find_all: bool,
        first_parent: bool,
    ) -> (Vec<Oid>, i64, i64, HashMap<Oid, i64>) {
        let list = &self.list;
        let nr = list.iter().filter(|c| !self.treesame.contains(c)).count() as i64;
        let mut weight: HashMap<Oid, i64> = HashMap::new();
        let halfway = |c: &Oid, w: i64| !self.treesame.contains(c) && (2 * w - nr).abs() <= 1;
        let mut counted = 0;
        for c in list {
            let w = match self.interesting_parents(*c, first_parent).count() {
                0 if !self.treesame.contains(c) => {
                    counted += 1;
                    1
                }
                0 => 0,
                1 => -1,
                _ => -2,
            };
            weight.insert(*c, w);
        }
        let done =
            |best: Oid, weight: &HashMap<Oid, i64>| (vec![best], weight[&best], nr, weight.clone());
        for c in list {
            if weight[c] != -2 {
                continue;
            }
            let w = self.count_distance(*c, &mut HashSet::new());
            weight.insert(*c, w);
            if !find_all && halfway(c, w) {
                return done(*c, &weight);
            }
            counted += 1;
        }
        while counted < nr {
            let mut progress = false;
            for c in list {
                if weight[c] >= 0 {
                    continue;
                }
                let Some(q) = self
                    .interesting_parents(*c, first_parent)
                    .find(|q| weight.get(q).is_some_and(|w| *w >= 0))
                else {
                    continue;
                };
                let mut w = weight[q];
                if !self.treesame.contains(c) {
                    w += 1;
                    counted += 1;
                }
                weight.insert(*c, w);
                progress = true;
                if !find_all && halfway(c, w) {
                    return done(*c, &weight);
                }
            }
            if !progress {
                break;
            }
        }
        let Some(&head) = list.first() else {
            return (Vec::new(), 0, nr, weight);
        };
        let mut dist: Vec<(i64, Oid)> = list
            .iter()
            .filter(|c| !self.treesame.contains(c))
            .map(|c| (weight[c].min(nr - weight[c]), *c))
            .collect();
        if !find_all {
            let mut best = (-1, head);
            for d in dist {
                if d.0 > best.0 {
                    best = d;
                }
            }
            return done(best.1, &weight);
        }
        dist.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let sorted: Vec<Oid> = match dist.is_empty() {
            true => vec![head],
            false => dist.into_iter().map(|d| d.1).collect(),
        };
        let reaches = weight[&sorted[0]];
        (sorted, reaches, nr, weight)
    }
}

/// Commits best first with their distances, how many commits the best
/// reaches and how many there are.
pub type BisectPick<T> = (Vec<(T, i64)>, i64, i64);

/// rev-list's `--bisect` over `tips ^hidden -- paths`: the best commit first
/// (every commit, best first, with `all`), each with its distance from the
/// ends, how many commits the best reaches and how many there are.
pub(crate) fn rev_list_bisect(
    repo: &Repository,
    tips: &[Oid],
    hidden: &[Oid],
    first_parent: bool,
    paths: &[String],
    all: bool,
) -> Result<BisectPick<Oid>, GitError> {
    let graph = Graph::walk(repo, tips, hidden, first_parent, paths)?;
    let (list, reaches, nr, weight) = graph.find_bisection(all, first_parent);
    let list = list
        .into_iter()
        .map(|c| {
            let w = weight.get(&c).copied().unwrap_or(0);
            (c, w.min(nr - w))
        })
        .collect();
    Ok((list, reaches, nr))
}

/// bisect.c's `estimate_bisect_steps`.
pub fn bisect_steps(all: i64) -> i64 {
    estimate_steps(all)
}

/// bisect.c's `managed_skipped`: the commits to choose from with skipped ones
/// stepped around, and the skipped ones tried on the way.
fn managed_skipped(list: Vec<Oid>, skips: &[Oid], bad: Oid) -> (Vec<Oid>, Vec<Oid>) {
    if skips.is_empty() {
        return (list, Vec::new());
    }
    let (mut tried, mut filtered) = (Vec::new(), Vec::new());
    let mut skipped_first = false;
    for c in list {
        if skips.contains(&c) {
            skipped_first = true;
            tried.push(c);
        } else if !skipped_first {
            return (vec![c], tried);
        } else {
            filtered.push(c);
        }
    }
    if !skipped_first {
        return (filtered, tried);
    }
    let count = filtered.len() as i64;
    let prn = get_prn(count as u32) as i64;
    let index = (count * prn / PRN_MODULO) * sqrti(prn) / sqrti(PRN_MODULO);
    for (i, c) in filtered.iter().enumerate() {
        if i as i64 == index {
            let from = if *c != bad { i } else { i.saturating_sub(1) };
            return (filtered[from..].to_vec(), tried);
        }
    }
    (filtered, tried)
}

const PRN_MODULO: i64 = 32768;

/// git's pseudo random number from "man 3 rand", seeded by `count`.
fn get_prn(count: u32) -> u32 {
    let count = count.wrapping_mul(1103515245).wrapping_add(12345);
    (count / 65536) % PRN_MODULO as u32
}

/// git's float square root, truncated.
fn sqrti(val: i64) -> i64 {
    if val == 0 {
        return 0;
    }
    let v = val as f32;
    let mut x = v;
    loop {
        let y = (x + v / x) / 2.0;
        let d = (y - x).abs();
        x = y;
        if d < 0.5 {
            break;
        }
    }
    x as i64
}

fn estimate_steps(all: i64) -> i64 {
    if all < 3 {
        return 0;
    }
    let n = 63 - i64::from(all.leading_zeros());
    let e = 1 << n;
    if e < 3 * (all - e) { n } else { n - 1 }
}

/// git's `sq_quote_argv`: each word as ` 'word'`.
fn sq_quote_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            let mut q = String::from(" '");
            for c in a.chars() {
                match c {
                    '\'' | '!' => {
                        q.push_str("'\\");
                        q.push(c);
                        q.push('\'');
                    }
                    c => q.push(c),
                }
            }
            q.push('\'');
            q
        })
        .collect()
}

/// The words of a `sq_quote_argv` string, or `None` if it is not one.
fn sq_dequote(s: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_ascii_whitespace()).is_some() {}
        if chars.peek().is_none() {
            return Some(out);
        }
        if chars.next() != Some('\'') {
            return None;
        }
        let mut word = String::new();
        loop {
            match chars.next()? {
                '\'' if chars.next_if_eq(&'\\').is_some() => {
                    let c = chars.next().filter(|c| matches!(c, '\'' | '!'))?;
                    word.push(c);
                    chars.next_if_eq(&'\'')?;
                }
                '\'' => break,
                c => word.push(c),
            }
        }
        out.push(word);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_and_steps_match_git() {
        let words = vec!["a b".to_owned(), "it's!".to_owned()];
        let q = sq_quote_argv(&words);
        assert_eq!(q, " 'a b' 'it'\\''s'\\!''");
        assert_eq!(sq_dequote(&q), Some(words));
        assert_eq!(
            [1, 2, 3, 4, 5, 9, 10, 1000].map(estimate_steps),
            [0, 0, 1, 1, 1, 2, 2, 9]
        );
        assert_eq!(sqrti(32768), 181);
        assert_eq!(get_prn(1), 16838);
    }
}
