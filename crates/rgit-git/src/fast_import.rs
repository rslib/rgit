//! `git fast-import`: read a fast-import stream, write its objects and
//! update the refs it names.

use crate::rev::RevParse;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, Read, Write};
use std::path::Path;

use git2::{ObjectType, Oid, Repository};

use crate::GitError;

fn die(msg: impl Into<String>) -> GitError {
    GitError::Other(msg.into())
}

type Files = BTreeMap<String, (u32, Oid)>;

#[derive(Default, Clone)]
struct Branch {
    tip: Option<Oid>,
    files: Files,
    delete: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum When {
    Raw,
    Permissive,
    Rfc2822,
    Now,
}

#[derive(Default)]
struct Counts {
    blobs: usize,
    trees: usize,
    commits: usize,
    tags: usize,
    duplicates: usize,
}

struct Importer<'a> {
    repo: &'a Repository,
    input: &'a mut dyn BufRead,
    out: &'a mut dyn Write,
    pushed: Option<String>,
    marks: HashMap<u32, Oid>,
    branches: HashMap<String, Branch>,
    order: Vec<String>,
    tags: Vec<(String, Oid)>,
    when: When,
    force: bool,
    export_marks: Option<String>,
    import_marks: Option<(String, bool)>,
    cli_marks: bool,
    need_done: bool,
    seen_data: bool,
    trees: HashSet<Oid>,
    seen: HashSet<Oid>,
    counts: Counts,
    failed: bool,
}

/// What `rgit fast-import` did: whether every ref updated, and the
/// statistics git prints.
pub struct FastImportReport {
    pub ok: bool,
    pub stats: String,
}

/// `git fast-import <args>`: read the stream from `input`; `out` gets the
/// replies to `cat-blob`, `ls`, `get-mark` and `progress`.
pub fn fast_import(
    git_dir: &Path,
    args: &[String],
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<FastImportReport, GitError> {
    let repo = Repository::open(git_dir)?;
    let mut im = Importer {
        repo: &repo,
        input,
        out,
        pushed: None,
        marks: HashMap::new(),
        branches: HashMap::new(),
        order: Vec::new(),
        tags: Vec::new(),
        when: When::Raw,
        force: false,
        export_marks: None,
        import_marks: None,
        cli_marks: false,
        need_done: false,
        seen_data: false,
        trees: HashSet::new(),
        seen: HashSet::new(),
        counts: Counts::default(),
        failed: false,
    };
    for a in args {
        im.option(a, true)?;
    }
    im.cli_marks = im.import_marks.is_some();
    im.load_marks()?;
    im.run()?;
    im.checkpoint()?;
    let c = &im.counts;
    let total = c.blobs + c.trees + c.commits + c.tags;
    let bar = "-".repeat(69);
    let kind = |n: usize| {
        format!(
            "{n:>10} ({:>10} duplicates {:>10} deltas of {:>10} attempts)",
            0, 0, 0
        )
    };
    // ponytail: git's counters without the pack and memory ones; rgit
    // writes loose objects.
    let stats = format!(
        "fast-import statistics:\n{bar}\n\
         Total objects:  {total:>10} ({:>10} duplicates                  )\n\
         \x20     blobs  :   {}\n\
         \x20     trees  :   {}\n\
         \x20     commits:   {}\n\
         \x20     tags   :   {}\n\
         Total branches:  {:>10} ({:>10} loads     )\n\
         \x20     marks:     {:>10} ({:>10} unique    )\n{bar}\n",
        c.duplicates,
        kind(c.blobs),
        kind(c.trees),
        kind(c.commits),
        kind(c.tags),
        im.branches.len(),
        im.branches.len(),
        im.marks.len(),
        im.marks.len(),
    );
    Ok(FastImportReport {
        ok: !im.failed,
        stats,
    })
}

impl Importer<'_> {
    fn option(&mut self, arg: &str, cli: bool) -> Result<(), GitError> {
        let (name, value) = match arg.split_once('=') {
            Some((n, v)) => (n, Some(v.to_owned())),
            None => (arg, None),
        };
        let need = || {
            value
                .clone()
                .ok_or_else(|| die(format!("option '{name}' requires a value")))
        };
        match name {
            "--date-format" => {
                self.when = match need()?.as_str() {
                    "raw" => When::Raw,
                    "raw-permissive" => When::Permissive,
                    "rfc2822" => When::Rfc2822,
                    "now" => When::Now,
                    f => return Err(die(format!("unknown --date-format argument {f}"))),
                }
            }
            "--force" => self.force = true,
            "--done" => self.need_done = true,
            "--export-marks" => self.export_marks = Some(need()?),
            "--import-marks" => self.import_marks = Some((need()?, false)),
            "--import-marks-if-exists" => self.import_marks = Some((need()?, true)),
            "--quiet"
            | "--stats"
            | "--relative-marks"
            | "--no-relative-marks"
            | "--allow-unsafe-features"
            | "--export-pack-edges"
            | "--max-pack-size"
            | "--big-file-threshold"
            | "--depth"
            | "--active-branches"
            | "--cat-blob-fd"
            | "--rewrite-submodules-from"
            | "--rewrite-submodules-to"
            | "--signed-commits"
            | "--signed-tags" => {}
            _ if !cli => {}
            _ => return Err(die(format!("unknown option {arg}"))),
        }
        Ok(())
    }

    fn load_marks(&mut self) -> Result<(), GitError> {
        let Some((file, if_exists)) = self.import_marks.clone() else {
            return Ok(());
        };
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(_) if if_exists => return Ok(()),
            Err(e) => return Err(die(format!("cannot read '{file}': {e}"))),
        };
        for line in text.lines() {
            let parsed = line
                .strip_prefix(':')
                .and_then(|l| l.split_once(' '))
                .and_then(|(m, id)| Some((m.parse::<u32>().ok()?, Oid::from_str(id).ok()?)));
            let Some((m, id)) = parsed else {
                return Err(die(format!("corrupt mark line: {line}")));
            };
            self.marks.insert(m, id);
        }
        Ok(())
    }

    /// The next command line without its newline, skipping comments; `None`
    /// at the end of the stream.
    fn line(&mut self) -> Result<Option<String>, GitError> {
        if let Some(l) = self.pushed.take() {
            return Ok(Some(l));
        }
        loop {
            let mut buf = Vec::new();
            if self.input.read_until(b'\n', &mut buf)? == 0 {
                return Ok(None);
            }
            if buf.last() == Some(&b'\n') {
                buf.pop();
            }
            if !buf.starts_with(b"feature ") && !buf.starts_with(b"option ") {
                self.seen_data = true;
            }
            if buf.first() == Some(&b'#') {
                continue;
            }
            return Ok(Some(String::from_utf8_lossy(&buf).into_owned()));
        }
    }

    fn unread(&mut self, l: String) {
        self.pushed = Some(l);
    }

    /// Consume one blank line if the stream has one next.
    fn optional_lf(&mut self) -> Result<(), GitError> {
        if self.pushed.is_some() {
            if self.pushed.as_deref() == Some("") {
                self.pushed = None;
            }
            return Ok(());
        }
        if self.input.fill_buf()?.first() == Some(&b'\n') {
            self.input.consume(1);
        }
        Ok(())
    }

    /// A `data` command's bytes.
    fn data(&mut self, line: Option<String>) -> Result<Vec<u8>, GitError> {
        let line = match line {
            Some(l) => l,
            None => self
                .line()?
                .ok_or_else(|| die("Expected 'data n' command, found EOF"))?,
        };
        let Some(arg) = line.strip_prefix("data ") else {
            return Err(die(format!("Expected 'data n' command, found: {line}")));
        };
        let mut body = Vec::new();
        if let Some(delim) = arg.strip_prefix("<<") {
            loop {
                let mut l = Vec::new();
                if self.input.read_until(b'\n', &mut l)? == 0 {
                    return Err(die(format!("EOF in data (terminator '{delim}' not found)")));
                }
                if l.strip_suffix(b"\n").unwrap_or(&l) == delim.as_bytes() {
                    break;
                }
                body.extend_from_slice(&l);
            }
        } else {
            let n: usize = arg
                .parse()
                .map_err(|_| die(format!("invalid count in data: {arg}")))?;
            let got = Read::take(&mut *self.input, n as u64).read_to_end(&mut body)?;
            if got < n {
                return Err(die(format!("EOF in data ({} bytes remaining)", n - got)));
            }
        }
        self.optional_lf()?;
        Ok(body)
    }

    fn write(&mut self, kind: ObjectType, data: &[u8]) -> Result<Oid, GitError> {
        let id = self.repo.odb()?.write(kind, data)?;
        if !self.seen.insert(id) {
            self.counts.duplicates += 1;
        } else {
            match kind {
                ObjectType::Blob => self.counts.blobs += 1,
                ObjectType::Commit => self.counts.commits += 1,
                ObjectType::Tag => self.counts.tags += 1,
                _ => {}
            }
        }
        Ok(id)
    }

    fn mark_line(&mut self) -> Result<Option<u32>, GitError> {
        let Some(l) = self.line()? else {
            return Ok(None);
        };
        if let Some(m) = l.strip_prefix("mark :") {
            return Ok(Some(
                m.parse().map_err(|_| die(format!("Invalid mark: {l}")))?,
            ));
        }
        self.unread(l);
        Ok(None)
    }

    fn skip_original_oid(&mut self) -> Result<(), GitError> {
        if let Some(l) = self.line()?
            && !l.starts_with("original-oid ")
        {
            self.unread(l);
        }
        Ok(())
    }

    fn run(&mut self) -> Result<(), GitError> {
        let mut done = false;
        while let Some(l) = self.line()? {
            let (cmd, arg) = l.split_once(' ').unwrap_or((l.as_str(), ""));
            match cmd {
                "blob" => {
                    let mark = self.mark_line()?;
                    self.skip_original_oid()?;
                    let data = self.data(None)?;
                    let id = self.write(ObjectType::Blob, &data)?;
                    if let Some(m) = mark {
                        self.marks.insert(m, id);
                    }
                }
                "commit" => self.commit(arg.to_owned())?,
                "tag" => self.tag(arg.to_owned())?,
                "reset" => self.reset(arg.to_owned())?,
                "checkpoint" => {
                    self.checkpoint()?;
                    self.optional_lf()?;
                }
                "progress" => {
                    writeln!(self.out, "{l}")?;
                    self.out.flush()?;
                    self.optional_lf()?;
                }
                "done" => {
                    done = true;
                    break;
                }
                "feature" | "option" if self.seen_data => {
                    return Err(die(format!("Got {cmd} command '{arg}' after data command")));
                }
                "feature" => self.feature(arg)?,
                "option" => {
                    if let Some(opt) = arg.strip_prefix("git ") {
                        self.option(opt, false)?;
                    }
                }
                "alias" => {
                    let mark = self
                        .mark_line()?
                        .ok_or_else(|| die("Expected 'mark' command"))?;
                    let to = self.line()?.unwrap_or_default();
                    let Some(to) = to.strip_prefix("to ") else {
                        return Err(die(format!("Expected 'to' command, got {to}")));
                    };
                    let id = self.commitish(to)?.ok_or_else(|| die("Invalid ref name"))?;
                    self.marks.insert(mark, id);
                    self.optional_lf()?;
                }
                "cat-blob" | "ls" | "get-mark" => self.query(&l, None)?,
                _ => return Err(die(format!("Unsupported command: {l}"))),
            }
        }
        if self.need_done && !done {
            return Err(die("stream ends early"));
        }
        Ok(())
    }

    fn feature(&mut self, f: &str) -> Result<(), GitError> {
        let (name, value) = f.split_once('=').unwrap_or((f, ""));
        match name {
            "date-format" | "export-marks" | "force" => {
                self.option(&format!("--{f}"), true)?;
            }
            "import-marks" | "import-marks-if-exists" => {
                if !self.cli_marks {
                    self.import_marks = Some((value.to_owned(), name != "import-marks"));
                    self.load_marks()?;
                }
            }
            "done" => self.need_done = true,
            "relative-marks" | "no-relative-marks" | "notes" | "ls" | "cat-blob" | "get-mark"
            | "alias" => {}
            _ => {
                return Err(die(format!(
                    "This version of fast-import does not support feature {f}."
                )));
            }
        }
        Ok(())
    }

    /// A `<dataref>`: `:<mark>` or an object id.
    fn dataref(&self, s: &str) -> Result<Oid, GitError> {
        if let Some(m) = s.strip_prefix(':') {
            let n: u32 = m.parse().map_err(|_| die(format!("Invalid mark: {s}")))?;
            return self
                .marks
                .get(&n)
                .copied()
                .ok_or_else(|| die(format!("mark :{n} not declared")));
        }
        Oid::from_str(s)
            .ok()
            .filter(|_| s.len() == 40)
            .ok_or_else(|| die(format!("Invalid dataref: {s}")))
    }

    /// A `from`/`merge` commit-ish: a branch of this import, a mark, an id
    /// or a revision; `None` for the all-zero id.
    fn commitish(&self, s: &str) -> Result<Option<Oid>, GitError> {
        if let Some(b) = self.branches.get(s) {
            return Ok(b.tip);
        }
        if s.starts_with(':') {
            return self.dataref(s).map(Some);
        }
        if let Ok(id) = Oid::from_str(s)
            && s.len() == 40
        {
            return Ok((!id.is_zero()).then_some(id));
        }
        self.repo
            .rev_single(s)
            .map(|o| Some(o.id()))
            .map_err(|_| die(format!("Invalid ref name or SHA1 expression: {s}")))
    }

    fn tree_files(&self, tree: Oid, prefix: &str, files: &mut Files) -> Result<(), GitError> {
        let tree = self.repo.find_tree(tree)?;
        for e in tree.iter() {
            let path = format!("{prefix}{}", String::from_utf8_lossy(e.name_bytes()));
            let mode = e.filemode() as u32;
            if mode == 0o040000 {
                self.tree_files(e.id(), &format!("{path}/"), files)?;
            } else {
                files.insert(path, (mode, e.id()));
            }
        }
        Ok(())
    }

    fn commit_files(&self, id: Oid) -> Result<Files, GitError> {
        let mut files = Files::new();
        let tree = self
            .repo
            .find_commit(id)
            .map_err(|_| die(format!("Not a commit: {id}")))?
            .tree_id();
        self.tree_files(tree, "", &mut files)?;
        Ok(files)
    }

    fn branch(&mut self, name: &str) -> Result<&mut Branch, GitError> {
        if !self.branches.contains_key(name) {
            if !git2::Reference::is_valid_name(name) && name != "HEAD" {
                return Err(die(format!(
                    "Branch name doesn't conform to GIT style: {name}"
                )));
            }
            self.order.push(name.to_owned());
            self.branches.insert(name.to_owned(), Branch::default());
        }
        Ok(self.branches.get_mut(name).expect("inserted"))
    }

    /// Point `name` at `from`, with its files.
    fn set_from(&mut self, name: &str, from: &str) -> Result<(), GitError> {
        if from == name && self.branches.get(name).is_some_and(|b| b.tip.is_none()) {
            return Err(die(format!("Can't create a branch from itself: {name}")));
        }
        let state = match self.branches.get(from) {
            Some(b) if from != name => Some(b.clone()),
            _ => None,
        };
        let b = match state {
            Some(b) => Branch { delete: false, ..b },
            None => match self.commitish(from)? {
                Some(id) => Branch {
                    tip: Some(id),
                    files: self.commit_files(id)?,
                    delete: false,
                },
                None => Branch {
                    delete: true,
                    ..Branch::default()
                },
            },
        };
        *self.branch(name)? = b;
        Ok(())
    }

    fn ident(&self, line: &str) -> Result<String, GitError> {
        let bad = |what: &str| die(format!("{what} in ident string: {line}"));
        let lt = line.find(['<', '>']).ok_or_else(|| bad("Missing <"))?;
        if line.as_bytes()[lt] != b'<' {
            return Err(bad("Missing <"));
        }
        if lt > 0 && line.as_bytes()[lt - 1] != b' ' {
            return Err(bad("Missing space before <"));
        }
        let gt = lt
            + 1
            + line[lt + 1..]
                .find(['<', '>'])
                .ok_or_else(|| bad("Missing >"))?;
        if line.as_bytes()[gt] != b'>' {
            return Err(bad("Missing >"));
        }
        if line.as_bytes().get(gt + 1) != Some(&b' ') {
            return Err(bad("Missing space after >"));
        }
        let (who, date) = (&line[..gt + 2], &line[gt + 2..]);
        let date = match self.when {
            When::Raw | When::Permissive => {
                let ok = date.split_once(' ').is_some_and(|(t, z)| {
                    t.bytes().all(|b| b.is_ascii_digit())
                        && !t.is_empty()
                        && z.len() == 5
                        && matches!(z.as_bytes()[0], b'+' | b'-')
                        && z[1..].bytes().all(|b| b.is_ascii_digit())
                });
                if !ok && self.when == When::Raw {
                    return Err(die(format!("Invalid raw date \"{date}\" in ident: {line}")));
                }
                date.to_owned()
            }
            When::Rfc2822 => {
                let (t, z) = crate::plumbing::parse_git_date(date, 0).ok_or_else(|| {
                    die(format!("Invalid rfc2822 date \"{date}\" in ident: {line}"))
                })?;
                zone(t, z)
            }
            When::Now => {
                if date != "now" {
                    return Err(die(format!("Date in ident must be 'now': {line}")));
                }
                let now = git2::Signature::now("x", "x")?.when();
                zone(now.seconds(), now.offset_minutes())
            }
        };
        Ok(format!("{who}{date}"))
    }

    fn commit(&mut self, name: String) -> Result<(), GitError> {
        self.branch(&name)?;
        let mark = self.mark_line()?;
        let (mut author, mut committer, mut encoding, mut sig) = (None, None, None, None);
        let msg = loop {
            let l = self
                .line()?
                .ok_or_else(|| die("Expected committer but didn't get one"))?;
            if l.starts_with("original-oid ") {
            } else if let Some(a) = l.strip_prefix("author ") {
                author = Some(self.ident(a)?);
            } else if let Some(c) = l.strip_prefix("committer ") {
                committer = Some(self.ident(c)?);
            } else if let Some(e) = l.strip_prefix("encoding ") {
                encoding = Some(e.to_owned());
            } else if l.starts_with("gpgsig ") {
                sig = Some(self.data(None)?);
            } else if l.starts_with("data ") {
                break self.data(Some(l))?;
            } else {
                return Err(die(format!("Expected committer but didn't get one: {l}")));
            }
        };
        let committer = committer.ok_or_else(|| die("Expected committer but didn't get one"))?;
        let mut parents = Vec::new();
        let mut next = self.line()?;
        if let Some(from) = next.as_deref().and_then(|l| l.strip_prefix("from ")) {
            let from = from.to_owned();
            self.set_from(&name, &from)?;
            next = self.line()?;
        }
        let b = self.branches[&name].clone();
        parents.extend(b.tip);
        let mut files = b.files;
        while let Some(m) = next.as_deref().and_then(|l| l.strip_prefix("merge ")) {
            let id = self
                .commitish(m)?
                .ok_or_else(|| die(format!("Invalid ref name or SHA1 expression: {m}")))?;
            parents.push(id);
            next = self.line()?;
        }
        while let Some(l) = next.take() {
            if l.is_empty() {
                break;
            }
            if !self.file_change(&l, &mut files)? {
                self.unread(l);
                break;
            }
            next = self.line()?;
        }
        let tree = self.build_tree(&files)?;
        let mut buf = format!("tree {tree}\n");
        for p in &parents {
            buf.push_str(&format!("parent {p}\n"));
        }
        let author = author.unwrap_or_else(|| committer.clone());
        buf.push_str(&format!("author {author}\ncommitter {committer}\n"));
        if let Some(e) = &encoding {
            buf.push_str(&format!("encoding {e}\n"));
        }
        let mut bytes = buf.into_bytes();
        if let Some(sig) = &sig {
            bytes.extend_from_slice(b"gpgsig ");
            let text = sig.strip_suffix(b"\n").unwrap_or(sig);
            for (i, line) in text.split(|&b| b == b'\n').enumerate() {
                if i > 0 {
                    bytes.extend_from_slice(b"\n ");
                }
                bytes.extend_from_slice(line);
            }
            bytes.push(b'\n');
        }
        bytes.push(b'\n');
        bytes.extend_from_slice(&msg);
        let id = self.write(ObjectType::Commit, &bytes)?;
        if let Some(m) = mark {
            self.marks.insert(m, id);
        }
        let b = self.branch(&name)?;
        b.tip = Some(id);
        b.files = files;
        b.delete = false;
        Ok(())
    }

    /// Apply one file change of a commit; false when `l` is not one.
    fn file_change(&mut self, l: &str, files: &mut Files) -> Result<bool, GitError> {
        let (cmd, rest) = l.split_once(' ').unwrap_or((l, ""));
        match cmd {
            "M" => {
                let mut parts = rest.splitn(3, ' ');
                let (Some(mode), Some(dref), Some(path)) =
                    (parts.next(), parts.next(), parts.next())
                else {
                    return Err(die(format!("Missing space after mode: {l}")));
                };
                let mode = match u32::from_str_radix(mode, 8) {
                    Ok(0o644) => 0o100644,
                    Ok(0o755) => 0o100755,
                    Ok(m @ (0o100644 | 0o100755 | 0o120000 | 0o040000 | 0o160000)) => m,
                    _ => return Err(die(format!("Corrupt mode: {l}"))),
                };
                let path = unquote(path);
                let id = if dref == "inline" {
                    let data = self.data(None)?;
                    self.write(ObjectType::Blob, &data)?
                } else {
                    self.dataref(dref)?
                };
                remove(files, &path);
                if mode == 0o040000 {
                    let prefix = if path.is_empty() {
                        String::new()
                    } else {
                        format!("{path}/")
                    };
                    self.tree_files(id, &prefix, files)?;
                } else {
                    files.insert(path, (mode, id));
                }
            }
            "D" => remove(files, &unquote(rest)),
            "C" | "R" => {
                let (src, dst) = split_paths(rest)
                    .ok_or_else(|| die(format!("Missing space after source: {l}")))?;
                let moved: Vec<(String, (u32, Oid))> = files
                    .iter()
                    .filter(|(p, _)| **p == src || p.starts_with(&format!("{src}/")))
                    .map(|(p, v)| (format!("{dst}{}", &p[src.len()..]), *v))
                    .collect();
                if moved.is_empty() {
                    return Err(die(format!("Path {src} not in branch")));
                }
                if cmd == "R" {
                    remove(files, &src);
                }
                remove(files, &dst);
                files.extend(moved);
            }
            "deleteall" => files.clear(),
            "N" => {
                let (dref, target) = rest
                    .split_once(' ')
                    .ok_or_else(|| die(format!("Missing space after source: {l}")))?;
                let id = if dref == "inline" {
                    let data = self.data(None)?;
                    self.write(ObjectType::Blob, &data)?
                } else {
                    self.dataref(dref)?
                };
                let commit = self
                    .commitish(target)?
                    .ok_or_else(|| die(format!("Invalid ref name or SHA1 expression: {target}")))?;
                // ponytail: notes stay flat (no fanout), as git keeps them under 256 notes.
                files.insert(commit.to_string(), (0o100644, id));
            }
            "ls" | "cat-blob" => self.query(l, Some(files))?,
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// `cat-blob`, `ls` and `get-mark`, answered on `out`.
    fn query(&mut self, l: &str, files: Option<&Files>) -> Result<(), GitError> {
        let (cmd, rest) = l.split_once(' ').unwrap_or((l, ""));
        match cmd {
            "get-mark" => {
                let id = self.dataref(rest)?;
                writeln!(self.out, "{id}")?;
            }
            "cat-blob" => {
                let id = self.dataref(rest)?;
                let blob = self.repo.find_blob(id)?;
                writeln!(self.out, "{id} blob {}", blob.content().len())?;
                self.out.write_all(blob.content())?;
                writeln!(self.out)?;
            }
            _ => {
                let (tree_files, path) = if rest.starts_with('"') {
                    let files = files.ok_or_else(|| die(format!("Not in a commit: {l}")))?;
                    (files.clone(), unquote(rest))
                } else {
                    let (dref, path) = rest
                        .split_once(' ')
                        .ok_or_else(|| die(format!("Missing space after tree-ish: {l}")))?;
                    let id = match self.dataref(dref) {
                        Ok(id) => id,
                        Err(_) => self
                            .commitish(dref)?
                            .ok_or_else(|| die(format!("Invalid dataref: {l}")))?,
                    };
                    let obj = self.repo.find_object(id, None)?;
                    let tree = obj.peel_to_tree()?;
                    let mut f = Files::new();
                    self.tree_files(tree.id(), "", &mut f)?;
                    (f, unquote(path))
                };
                let quoted = crate::quote_path(&path);
                if let Some((mode, id)) = tree_files.get(&path) {
                    let kind = match *mode {
                        0o160000 => "commit",
                        _ => "blob",
                    };
                    writeln!(self.out, "{mode:06o} {kind} {id}\t{quoted}")?;
                } else if tree_files
                    .keys()
                    .any(|p| p.starts_with(&format!("{path}/")))
                    || path.is_empty()
                {
                    let prefix = if path.is_empty() {
                        String::new()
                    } else {
                        format!("{path}/")
                    };
                    let sub: Files = tree_files
                        .iter()
                        .filter_map(|(p, v)| Some((p.strip_prefix(&prefix)?.to_owned(), *v)))
                        .collect();
                    let id = self.build_tree(&sub)?;
                    writeln!(self.out, "040000 tree {id}\t{quoted}")?;
                } else {
                    writeln!(self.out, "missing {quoted}")?;
                }
            }
        }
        self.out.flush()?;
        Ok(())
    }

    // ponytail: rebuilds every tree of a commit from its flat file list;
    // cache unchanged subtrees if big imports get slow.
    fn build_tree(&mut self, files: &Files) -> Result<Oid, GitError> {
        let mut dirs: BTreeMap<&str, Files> = BTreeMap::new();
        let mut tb = self.repo.treebuilder(None)?;
        for (path, (mode, id)) in files {
            match path.split_once('/') {
                Some((dir, rest)) => {
                    dirs.entry(dir)
                        .or_default()
                        .insert(rest.to_owned(), (*mode, *id));
                }
                None => {
                    tb.insert(path, *id, *mode as i32)?;
                }
            }
        }
        for (dir, sub) in dirs {
            let id = self.build_tree(&sub)?;
            tb.insert(dir, id, 0o040000)?;
        }
        let id = tb.write()?;
        if self.trees.insert(id) {
            self.counts.trees += 1;
        }
        Ok(id)
    }

    fn tag(&mut self, name: String) -> Result<(), GitError> {
        let mark = self.mark_line()?;
        let from = self.line()?.unwrap_or_default();
        let Some(from) = from.strip_prefix("from ") else {
            return Err(die(format!("Expected from command, got {from}")));
        };
        let target = self
            .commitish(from)?
            .ok_or_else(|| die(format!("Invalid ref name or SHA1 expression: {from}")))?;
        self.skip_original_oid()?;
        let mut tagger = None;
        let mut l = self.line()?;
        if let Some(t) = l.as_deref().and_then(|l| l.strip_prefix("tagger ")) {
            tagger = Some(self.ident(t)?);
            l = None;
        }
        let msg = self.data(l)?;
        let kind = self.repo.odb()?.read_header(target)?.1;
        let mut buf = format!("object {target}\ntype {kind}\ntag {name}\n");
        if let Some(t) = tagger {
            buf.push_str(&format!("tagger {t}\n"));
        }
        let mut bytes = buf.into_bytes();
        bytes.push(b'\n');
        bytes.extend_from_slice(&msg);
        let id = self.write(ObjectType::Tag, &bytes)?;
        if let Some(m) = mark {
            self.marks.insert(m, id);
        }
        let refname = format!("refs/tags/{name}");
        self.tags.retain(|(n, _)| *n != refname);
        self.tags.push((refname, id));
        Ok(())
    }

    fn reset(&mut self, name: String) -> Result<(), GitError> {
        *self.branch(&name)? = Branch::default();
        match self.line()? {
            Some(l) if l.starts_with("from ") => {
                self.set_from(&name, &l[5..])?;
                self.optional_lf()?;
            }
            Some(l) if l.is_empty() => {}
            Some(l) => self.unread(l),
            None => {}
        }
        Ok(())
    }

    /// Write the refs and marks, as git does at a checkpoint and the end.
    fn checkpoint(&mut self) -> Result<(), GitError> {
        let repo = self.repo;
        for name in self.order.clone() {
            let b = &self.branches[&name];
            let Some(new) = b.tip else {
                if b.delete
                    && let Ok(mut r) = repo.find_reference(&name)
                {
                    r.delete()?;
                }
                continue;
            };
            if let Ok(old) = repo.refname_to_id(&name) {
                if old == new {
                    continue;
                }
                if !self.force && !repo.graph_descendant_of(new, old).unwrap_or(false) {
                    eprintln!(
                        "warning: Not updating {name} (new tip {new} does not contain {old})"
                    );
                    self.failed = true;
                    continue;
                }
            }
            repo.reference(&name, new, true, "fast-import")?;
        }
        for (name, id) in &self.tags {
            repo.reference(name, *id, true, "fast-import")?;
        }
        if let Some(file) = &self.export_marks {
            let mut marks: Vec<(&u32, &Oid)> = self.marks.iter().collect();
            marks.sort();
            let text: String = marks.iter().map(|(m, id)| format!(":{m} {id}\n")).collect();
            std::fs::write(file, text)?;
        }
        Ok(())
    }
}

fn zone(t: i64, z: i32) -> String {
    let sign = if z < 0 { '-' } else { '+' };
    format!("{t} {sign}{:02}{:02}", z.abs() / 60, z.abs() % 60)
}

fn remove(files: &mut Files, path: &str) {
    if path.is_empty() {
        files.clear();
        return;
    }
    let dir = format!("{path}/");
    files.retain(|p, _| p != path && !p.starts_with(&dir));
}

fn unquote(s: &str) -> String {
    if s.starts_with('"') {
        crate::apply::unquote(s)
    } else {
        s.to_owned()
    }
}

/// `C`/`R` arguments: a source (quoted, or up to a space) and the rest.
fn split_paths(rest: &str) -> Option<(String, String)> {
    if rest.starts_with('"') {
        let b = rest.as_bytes();
        let mut i = 1;
        while i < b.len() {
            match b[i] {
                b'\\' => i += 2,
                b'"' => break,
                _ => i += 1,
            }
        }
        let src = &rest[..=i.min(rest.len() - 1)];
        let dst = rest.get(i + 1..)?.strip_prefix(' ')?;
        return Some((unquote(src), unquote(dst)));
    }
    let (src, dst) = rest.split_once(' ')?;
    Some((src.to_owned(), unquote(dst)))
}
