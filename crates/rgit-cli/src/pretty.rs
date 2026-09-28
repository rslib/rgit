//! git's commit formats for `log` and `show`: the built-in pretty formats,
//! `format:`/`tformat:` strings, `--date` styles and ref decorations, byte for
//! byte as git prints them.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rgit_git::{GitBackend, Ident, RawObject, SignatureCheck};

use crate::cli::{CliError, PrettyArgs};

/// A commit as git stores it.
#[derive(Clone)]
pub struct Commit {
    pub id: String,
    pub tree: String,
    pub parents: Vec<String>,
    pub author: Ident,
    pub committer: Ident,
    /// The header lines, each newline-terminated, for `--pretty=raw`.
    pub header: String,
    pub message: String,
    /// The parent a `-m` diff section is against, shown as ` (from <id>)`.
    pub from: Option<String>,
    /// The walk's mark for it (see [`rgit_git::LogEntry::mark`]).
    pub mark: Option<char>,
    /// The starting ref that reached it, with `--source`.
    pub source: Option<String>,
    /// The reflog entry it was reached from, with `--walk-reflogs`.
    pub reflog: Option<Reflog>,
}

/// A reflog entry as `log -g` shows it.
#[derive(Clone)]
pub struct Reflog {
    /// `HEAD@{0}`, the ref as given (`refs/stash@{0}`).
    pub selector: String,
    /// The same with the ref shortened (`stash@{0}`), for `%gd`.
    pub short: String,
    pub who: Ident,
    pub message: String,
}

/// `Name <email> 1700000000 +0100` from a commit header.
fn ident(s: &str) -> Ident {
    let (name, rest) = s.split_once(" <").unwrap_or((s, ""));
    let (email, rest) = rest.split_once("> ").unwrap_or((rest, ""));
    let (time, tz) = rest.split_once(' ').unwrap_or((rest, "+0000"));
    let n: i32 = tz.get(1..).and_then(|n| n.parse().ok()).unwrap_or(0);
    let sign = if tz.starts_with('-') { -1 } else { 1 };
    Ident {
        name: name.to_owned(),
        email: email.to_owned(),
        time: time.parse().unwrap_or(0),
        offset: sign * (n / 100 * 60 + n % 100),
    }
}

pub fn parse(obj: &RawObject) -> Commit {
    let text = String::from_utf8_lossy(&obj.data);
    let (head, message) = text.split_once("\n\n").unwrap_or((&text, ""));
    let mut c = Commit {
        id: obj.id.clone(),
        tree: String::new(),
        parents: Vec::new(),
        author: ident(""),
        committer: ident(""),
        header: String::new(),
        message: message.to_owned(),
        from: None,
        mark: None,
        source: None,
        reflog: None,
    };
    for line in head.lines() {
        c.header.push_str(line);
        c.header.push('\n');
        if let Some(t) = line.strip_prefix("tree ") {
            c.tree = t.to_owned();
        } else if let Some(p) = line.strip_prefix("parent ") {
            c.parents.push(p.to_owned());
        } else if let Some(a) = line.strip_prefix("author ") {
            c.author = ident(a);
        } else if let Some(a) = line.strip_prefix("committer ") {
            c.committer = ident(a);
        }
    }
    c
}

#[derive(Clone, PartialEq)]
pub enum Fmt {
    Oneline,
    Short,
    Medium,
    Full,
    Fuller,
    Raw,
    User(String),
}

/// How `log` and `show` print each commit.
pub struct Pretty {
    pub fmt: Fmt,
    /// Each commit ends with a newline (oneline, tformat) instead of the
    /// commits being separated by one.
    pub terminator: bool,
    abbrev: bool,
    date: String,
    decorate: bool,
    color: bool,
    decorations: HashMap<String, Vec<(&'static str, String)>>,
    /// The notes refs shown and the objects each annotates.
    notes: Vec<(String, HashSet<String>)>,
    show_signature: bool,
    /// The last commit whose signature was checked, for the %G placeholders.
    checked: std::cell::RefCell<Option<(String, SignatureCheck)>>,
    backend: Arc<dyn GitBackend>,
    graph: bool,
    /// Print the parents after each commit's id.
    pub parents: bool,
    /// How the log walked, for its marks.
    pub walk: crate::cli::WalkArgs,
    /// Inside a format after `%C(auto)`.
    auto: std::cell::Cell<bool>,
}

impl Pretty {
    /// The format the arguments ask for, else `fallback`; None when neither
    /// names one.
    pub fn new(
        backend: &Arc<dyn GitBackend>,
        args: &PrettyArgs,
        fallback: Option<&str>,
    ) -> anyhow::Result<Option<Pretty>> {
        let spec = args
            .format
            .as_deref()
            .or(args.pretty.as_deref())
            .or(args.oneline.then_some("oneline"))
            .or((args.graph || args.no_notes || !args.notes.is_empty()).then_some("medium"))
            .or(args.show_signature.then_some("medium"))
            .or(fallback);
        let Some(spec) = spec else {
            return Ok(None);
        };
        let mut date = args.date.clone();
        let (fmt, terminator) = match spec {
            "oneline" => (Fmt::Oneline, true),
            "short" => (Fmt::Short, false),
            "medium" | "" => (Fmt::Medium, false),
            "full" => (Fmt::Full, false),
            "fuller" => (Fmt::Fuller, false),
            "raw" => (Fmt::Raw, false),
            "reference" => {
                date.get_or_insert_with(|| "short".to_owned());
                (Fmt::User("%C(auto)%h (%s, %ad)".to_owned()), true)
            }
            s => match (s.strip_prefix("format:"), s.strip_prefix("tformat:")) {
                (Some(f), _) => {
                    crate::cli::print_as_is();
                    (Fmt::User(f.to_owned()), false)
                }
                (_, Some(f)) => (Fmt::User(f.to_owned()), true),
                _ if s.contains('%') => (Fmt::User(s.to_owned()), true),
                _ => return Err(CliError::usage(format!("invalid --pretty format: {s}"))),
            },
        };
        let terminal = crate::globals::stdout_tty();
        let decorate = match (&args.decorate, args.no_decorate) {
            (_, true) => false,
            (Some(d), _) => d != "no",
            (None, _) => terminal,
        };
        // Like git, notes show only in the built-in formats unless asked for
        // (or named by %N).
        let show_notes = if args.no_notes || !args.notes.is_empty() {
            !args.notes.is_empty()
        } else {
            !(args.format.is_some() || args.pretty.is_some() || args.oneline)
                || matches!(&fmt, Fmt::User(f) if f.contains("%N"))
        };
        let notes = if show_notes {
            let use_default = if args.notes.iter().any(String::is_empty) {
                Some(true)
            } else {
                args.no_notes.then_some(false)
            };
            let extra: Vec<String> = args
                .notes
                .iter()
                .filter(|r| !r.is_empty())
                .map(|r| crate::cli::notes_ref_name(r))
                .collect();
            display_notes_refs(backend, use_default, &extra)
                .into_iter()
                .map(|r| {
                    let objs = backend.notes(Some(&r)).unwrap_or_default();
                    (r, objs.into_iter().map(|(_, commit)| commit).collect())
                })
                .collect()
        } else {
            Vec::new()
        };
        Ok(Some(Pretty {
            fmt,
            terminator,
            abbrev: args.oneline || args.abbrev_commit,
            date: date.unwrap_or_else(|| "default".to_owned()),
            decorate,
            color: crate::render::color_on(),
            decorations: decorations(backend),
            notes,
            show_signature: args.show_signature,
            checked: Default::default(),
            backend: backend.clone(),
            graph: args.graph,
            parents: false,
            walk: Default::default(),
            auto: std::cell::Cell::new(false),
        }))
    }

    /// Whether a blank line goes between the message and the diff. A merge's
    /// combined diff gets one even after a oneline.
    pub fn blank_before_diff(&self, merge: bool) -> bool {
        match &self.fmt {
            Fmt::Oneline => merge,
            Fmt::User(f) => !f.is_empty(),
            _ => true,
        }
    }

    fn abbrev(&self, id: &str) -> String {
        self.backend
            .abbrev_id(id, 0)
            .unwrap_or_else(|_| id[..7.min(id.len())].to_owned())
    }

    fn paint(&self, s: &str, code: &str) -> String {
        if self.color && !s.is_empty() {
            format!("\x1b[{code}m{s}\x1b[m")
        } else {
            s.to_owned()
        }
    }

    /// ` (HEAD -> main, tag: v1)`, or empty.
    fn decor(&self, id: &str) -> String {
        self.decor_in(id, false, true)
    }

    /// [`Self::decor`] in git's decoration colors when `color`, without the
    /// ` (` and `)` around it unless `wrap` (`%D`).
    fn decor_in(&self, id: &str, color: bool, wrap: bool) -> String {
        let Some(labels) = self.decorations.get(id) else {
            return String::new();
        };
        let paint = |s: &str, code: &str| {
            if color {
                format!("\x1b[{code}m{s}\x1b[m")
            } else {
                s.to_owned()
            }
        };
        let mut out = String::new();
        for (i, (code, label)) in labels.iter().enumerate() {
            match (i, wrap) {
                (0, false) => {}
                (0, true) => out.push_str(&paint(" (", "33")),
                _ => out.push_str(&paint(", ", "33")),
            }
            if let Some(branch) = label.strip_prefix("HEAD -> ") {
                out.push_str(&paint("HEAD", code));
                out.push_str(&paint(" -> ", "33"));
                out.push_str(&paint(branch, "1;32"));
            } else if let Some(tag) = label.strip_prefix("tag: ") {
                out.push_str(&paint("tag: ", code));
                out.push_str(&paint(tag, code));
            } else {
                out.push_str(&paint(label, code));
            }
        }
        if wrap {
            out.push_str(&paint(")", "33"));
        }
        out
    }

    /// The character `--graph` draws for the commit.
    pub fn graph_mark(&self, c: &Commit) -> char {
        match c.mark {
            Some('-') => 'o',
            Some('=') => '=',
            m if self.walk.left_right => m.unwrap_or('>'),
            _ => '*',
        }
    }

    /// One commit as git's show_log prints it, without the separator or
    /// terminator.
    pub fn show(&self, c: &Commit) -> String {
        let (head, msg) = self.parts(c);
        head + &msg
    }

    /// The commit's header line (`commit <id>\n`, or the oneline's `<id> `)
    /// and the message that follows it.
    pub fn parts(&self, c: &Commit) -> (String, String) {
        let id = |id: &str| {
            if self.abbrev {
                self.abbrev(id)
            } else {
                id.to_owned()
            }
        };
        let mut hash = id(&c.id);
        if let Some(from) = &c.from {
            hash.push_str(&format!(" (from {})", id(from)));
        }
        if self.parents {
            for p in &c.parents {
                hash.push(' ');
                hash.push_str(&id(p));
            }
        }
        let mut decor = if self.decorate {
            self.decor_in(&c.id, self.color, true)
        } else {
            String::new()
        };
        if let Some(source) = &c.source {
            decor.insert_str(0, &format!("\t{source}"));
        }
        // With --graph the mark is drawn in the graph instead.
        let mark = match self.walk.mark(c.mark) {
            Some(m) if !self.graph => format!("{m} "),
            _ => String::new(),
        };
        let sig = if self.show_signature {
            self.signature(c).output
        } else {
            String::new()
        };
        let (head, mut out) = match &self.fmt {
            Fmt::User(f) => return (sig, self.expand(f, c)),
            Fmt::Oneline => (
                format!("{mark}{}{decor} {sig}", self.paint(&hash, "33")),
                match &c.reflog {
                    Some(r) => format!("{}: {}", r.selector, r.message),
                    None => subject(&c.message, " "),
                },
            ),
            fmt => {
                let head = format!(
                    "{}{decor}\n{sig}",
                    self.paint(&format!("commit {mark}{hash}"), "33")
                );
                let mut out = String::new();
                if let Some(r) = &c.reflog {
                    out.push_str(&format!(
                        "Reflog: {} ({} <{}>)\nReflog message: {}\n",
                        r.selector, r.who.name, r.who.email, r.message
                    ));
                }
                if *fmt == Fmt::Raw {
                    out.push_str(&c.header);
                } else {
                    if c.parents.len() > 1 {
                        out.push_str("Merge:");
                        for p in &c.parents {
                            out.push(' ');
                            out.push_str(&self.abbrev(p));
                        }
                        out.push('\n');
                    }
                    let pad = if *fmt == Fmt::Fuller { "    " } else { "" };
                    let who = |i: &Ident| format!("{pad}{} <{}>\n", i.name, i.email);
                    out.push_str(&format!("Author: {}", who(&c.author)));
                    match fmt {
                        Fmt::Medium => out.push_str(&format!("Date:   {}\n", self.date(&c.author))),
                        Fmt::Fuller => {
                            out.push_str(&format!("AuthorDate: {}\n", self.date(&c.author)))
                        }
                        _ => {}
                    }
                    if matches!(fmt, Fmt::Full | Fmt::Fuller) {
                        out.push_str(&format!("Commit: {}", who(&c.committer)));
                    }
                    if *fmt == Fmt::Fuller {
                        out.push_str(&format!("CommitDate: {}\n", self.date(&c.committer)));
                    }
                }
                out.push('\n');
                let tabs = matches!(fmt, Fmt::Medium | Fmt::Full | Fmt::Fuller);
                let mut first = true;
                for line in c.message.lines() {
                    let line = line.trim_end();
                    if line.is_empty() {
                        if first {
                            continue;
                        }
                        if *fmt == Fmt::Short {
                            break;
                        }
                    }
                    first = false;
                    out.push_str("    ");
                    out.push_str(&if tabs {
                        expand_tabs(line)
                    } else {
                        line.to_owned()
                    });
                    out.push('\n');
                }
                out.truncate(out.trim_end().len());
                out.push('\n');
                (head, out)
            }
        };
        if !matches!(self.fmt, Fmt::User(_)) {
            out.push_str(&self.note_block(&c.id, false));
        }
        (head, out)
    }

    /// The commit's notes as git's format_display_notes writes them: under a
    /// `Notes (<ref>):` header each, or with `raw` (%N) bare.
    pub fn note_block(&self, id: &str, raw: bool) -> String {
        let mut out = String::new();
        for (r, objs) in &self.notes {
            let Some(note) = objs
                .contains(id)
                .then(|| self.backend.note_show(Some(r), id).ok())
                .flatten()
            else {
                continue;
            };
            if !raw {
                if r == "refs/notes/commits" {
                    out.push_str("\nNotes:\n");
                } else {
                    let short = r.strip_prefix("refs/").unwrap_or(r);
                    let short = short.strip_prefix("notes/").unwrap_or(short);
                    out.push_str(&format!("\nNotes ({short}):\n"));
                }
            }
            for line in note.strip_suffix('\n').unwrap_or(&note).split('\n') {
                if !raw {
                    out.push_str("    ");
                }
                out.push_str(line);
                out.push('\n');
            }
        }
        out
    }

    /// An annotated tag as `git show` prints it before the tagged object.
    pub fn tag(&self, obj: &RawObject) -> String {
        let text = String::from_utf8_lossy(&obj.data);
        let (head, message) = text.split_once("\n\n").unwrap_or((&text, ""));
        let mut out = String::new();
        for line in head.lines() {
            if let Some(name) = line.strip_prefix("tag ") {
                out.insert_str(
                    0,
                    &format!("{}\n", self.paint(&format!("tag {name}"), "33")),
                );
            } else if let Some(who) = line.strip_prefix("tagger ") {
                let who = ident(who);
                let name = format!("{} <{}>", who.name, who.email);
                out.push_str(&match self.fmt {
                    Fmt::Oneline => String::new(),
                    Fmt::Medium => format!("Tagger: {name}\nDate:   {}\n", self.date(&who)),
                    Fmt::Fuller => {
                        format!("Tagger:     {name}\nTaggerDate: {}\n", self.date(&who))
                    }
                    _ => format!("Tagger: {name}\n"),
                });
            }
        }
        out.push('\n');
        out.push_str(message);
        out
    }

    fn date(&self, who: &Ident) -> String {
        format_date(who.time, who.offset, &self.date)
    }

    /// The check of `c`'s signature, run once per commit.
    fn signature(&self, c: &Commit) -> SignatureCheck {
        let mut checked = self.checked.borrow_mut();
        if let Some((id, check)) = checked.as_ref()
            && *id == c.id
        {
            return check.clone();
        }
        let check = self
            .backend
            .signature_check(&c.id, false)
            .unwrap_or_else(|_| SignatureCheck {
                result: 'N',
                ..SignatureCheck::default()
            });
        *checked = Some((c.id.clone(), check.clone()));
        check
    }

    /// Expand a `format:` string, as git's format_commit_message does.
    fn expand(&self, fmt: &str, c: &Commit) -> String {
        let mut out = String::new();
        let mut rest = fmt;
        self.auto.set(false);
        while let Some(i) = rest.find('%') {
            out.push_str(&rest[..i]);
            rest = &rest[i + 1..];
            // %C(auto): color %h, %H, %d and %D from here on, as git does.
            if let Some(r) = rest.strip_prefix("C(auto)") {
                self.auto.set(self.color);
                if self.color && !out.is_empty() {
                    out.push_str("\x1b[m");
                }
                rest = r;
                continue;
            }
            let magic = rest.chars().next().filter(|m| matches!(m, '+' | '-' | ' '));
            let spec = &rest[magic.map_or(0, char::len_utf8)..];
            let Some((value, used)) = self.placeholder(spec, c) else {
                out.push('%');
                continue;
            };
            rest = &spec[used..];
            match magic {
                Some('+') if !value.is_empty() => out.push('\n'),
                Some('-') if value.is_empty() => out.truncate(out.trim_end_matches('\n').len()),
                Some(' ') if !value.is_empty() => out.push(' '),
                _ => {}
            }
            out.push_str(&value);
        }
        out.push_str(rest);
        out
    }

    /// The value of the placeholder at the start of `s` and its length.
    fn placeholder(&self, s: &str, c: &Commit) -> Option<(String, usize)> {
        let mut chars = s.chars();
        let first = chars.next()?;
        let one = |v: String| Some((v, 1));
        match first {
            '%' => one("%".to_owned()),
            'n' => one("\n".to_owned()),
            'H' if self.auto.get() => one(self.paint(&c.id, "33")),
            'h' if self.auto.get() => one(self.paint(&self.abbrev(&c.id), "33")),
            'd' if self.auto.get() => one(self.decor_in(&c.id, true, true)),
            'D' if self.auto.get() => one(self.decor_in(&c.id, true, false)),
            'H' => one(c.id.clone()),
            'h' => one(self.abbrev(&c.id)),
            'T' => one(c.tree.clone()),
            't' => one(self.abbrev(&c.tree)),
            'P' => one(c.parents.join(" ")),
            'p' => one(c
                .parents
                .iter()
                .map(|p| self.abbrev(p))
                .collect::<Vec<_>>()
                .join(" ")),
            's' => one(subject(&c.message, " ")),
            'f' => one(sanitize(
                subject(&c.message, "\n").lines().next().unwrap_or(""),
            )),
            'b' => one(body(&c.message).to_owned()),
            'B' => one(c.message.clone()),
            'd' => one(self.decor(&c.id)),
            'D' => one(self.decor_in(&c.id, false, false)),
            'e' => one(String::new()),
            'N' => one(self.note_block(&c.id, true)),
            'm' => one(c.mark.unwrap_or('>').to_string()),
            'S' => one(c.source.clone().unwrap_or_default()),
            'g' => {
                let r = c.reflog.as_ref();
                let v = match s[1..].chars().next()? {
                    'd' => r.map(|r| r.short.clone()),
                    'D' => r.map(|r| r.selector.clone()),
                    's' => r.map(|r| r.message.clone()),
                    'n' | 'N' => r.map(|r| r.who.name.clone()),
                    'e' | 'E' => r.map(|r| r.who.email.clone()),
                    _ => return None,
                };
                Some((v.unwrap_or_default(), 2))
            }
            'G' => {
                let check = self.signature(c);
                let v = match chars.next()? {
                    '?' => check.letter().to_string(),
                    'S' => check.signer,
                    'K' => check.key,
                    'F' => check.fingerprint,
                    'P' => check.primary_key,
                    'T' => check.trust,
                    'G' => check.output,
                    _ => return None,
                };
                Some((v, 2))
            }
            'x' => {
                let hex = s.get(1..3)?;
                let b = u8::from_str_radix(hex, 16).ok()?;
                Some(((b as char).to_string(), 3))
            }
            'C' => {
                // An explicit color ends %C(auto)'s coloring, as in git.
                self.auto.set(false);
                let (name, used) = if let Some(spec) = s[1..].strip_prefix('(') {
                    let end = spec.find(')')?;
                    (&spec[..end], end + 3)
                } else {
                    let name = ["red", "green", "blue", "reset"]
                        .into_iter()
                        .find(|n| s[1..].starts_with(n))?;
                    (name, name.len() + 1)
                };
                let on = self.color || name.starts_with("always,");
                let code = match name
                    .trim_start_matches("auto,")
                    .trim_start_matches("always,")
                {
                    "reset" => "",
                    "red" => "31",
                    "green" => "32",
                    "yellow" => "33",
                    "blue" => "34",
                    "magenta" => "35",
                    "cyan" => "36",
                    "bold" => "1",
                    _ => return Some((String::new(), used)),
                };
                Some((
                    if on {
                        format!("\x1b[{code}m")
                    } else {
                        String::new()
                    },
                    used,
                ))
            }
            'a' | 'c' => {
                let who = if first == 'a' {
                    &c.author
                } else {
                    &c.committer
                };
                let v = match chars.next()? {
                    'n' | 'N' => who.name.clone(),
                    'e' | 'E' => who.email.clone(),
                    'l' | 'L' => who.email.split('@').next().unwrap_or_default().to_owned(),
                    'd' => self.date(who),
                    'D' => format_date(who.time, who.offset, "rfc"),
                    'r' => format_date(who.time, who.offset, "relative"),
                    't' => who.time.to_string(),
                    'i' => format_date(who.time, who.offset, "iso"),
                    'I' => format_date(who.time, who.offset, "iso-strict"),
                    's' => format_date(who.time, who.offset, "short"),
                    'h' => format_date(who.time, who.offset, "human"),
                    _ => return None,
                };
                Some((v, 2))
            }
            _ => None,
        }
    }
}

/// The first paragraph, its lines joined with `sep` (git's %s).
pub fn subject(message: &str, sep: &str) -> String {
    message
        .lines()
        .skip_while(|l| l.trim().is_empty())
        .take_while(|l| !l.trim().is_empty())
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join(sep)
}

/// Everything after the first paragraph and the blank lines that follow it
/// (git's %b).
fn body(message: &str) -> &str {
    let mut rest = message;
    // 0: leading blank lines, 1: the subject, 2: blank lines after it.
    let mut state = 0;
    while !rest.is_empty() {
        let (line, next) = rest.split_once('\n').unwrap_or((rest, ""));
        match (state, line.trim().is_empty()) {
            (0, false) => state = 1,
            (1, true) => state = 2,
            (2, false) => break,
            _ => {}
        }
        rest = next;
    }
    rest
}

/// git's %f: the subject with runs of other characters turned into `-`.
pub(crate) fn sanitize(subject: &str) -> String {
    let mut out = String::new();
    let mut space = 2;
    let b = subject.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_alphanumeric() || c == b'.' || c == b'_' {
            if space == 1 {
                out.push('-');
            }
            space = 0;
            out.push(c as char);
            if c == b'.' {
                while b.get(i + 1) == Some(&b'.') {
                    i += 1;
                }
            }
        } else {
            space |= 1;
        }
        i += 1;
    }
    out.trim_end_matches(['.', '-']).to_owned()
}

/// Tabs to spaces at 8-column stops, as git's log does for indented formats.
fn expand_tabs(line: &str) -> String {
    let mut out = String::new();
    let mut col = 0;
    for ch in line.chars() {
        if ch == '\t' {
            let n = 8 - col % 8;
            out.push_str(&" ".repeat(n));
            col += n;
        } else {
            out.push(ch);
            col += 1;
        }
    }
    out
}

/// Ref labels per commit id, in git's --decorate order: HEAD first, then
/// tags, remote-tracking branches and branches, each in reverse name order.
pub fn decorations(backend: &Arc<dyn GitBackend>) -> HashMap<String, Vec<(&'static str, String)>> {
    let mut map: HashMap<String, Vec<(&'static str, String)>> = HashMap::new();
    let head = backend.symbolic_ref("HEAD").ok().flatten();
    let mut refs = backend.ref_details().unwrap_or_default();
    refs.sort_by(|a, b| b.name.cmp(&a.name));
    for r in &refs {
        if Some(&r.name) == head.as_ref() {
            continue;
        }
        let label = if let Some(n) = r.name.strip_prefix("refs/heads/") {
            ("1;32", n.to_owned())
        } else if let Some(n) = r.name.strip_prefix("refs/remotes/") {
            ("1;31", n.to_owned())
        } else if let Some(n) = r.name.strip_prefix("refs/tags/") {
            ("1;33", format!("tag: {n}"))
        } else if r.name == "refs/stash" {
            ("1;35", r.name.clone())
        } else {
            continue;
        };
        let id = r.peeled.clone().unwrap_or_else(|| r.id.clone());
        map.entry(id).or_default().push(label);
    }
    if let Ok(id) = backend.rev_parse("HEAD") {
        let label = match head.as_deref().and_then(|h| h.strip_prefix("refs/heads/")) {
            Some(branch) => format!("HEAD -> {branch}"),
            None => "HEAD".to_owned(),
        };
        map.entry(id).or_default().insert(0, ("1;36", label));
    }
    map
}

/// The local time zone's offset from UTC at `time`, in minutes.
pub fn local_offset(time: i64) -> i32 {
    #[cfg(unix)]
    {
        let t = time as libc::time_t;
        // SAFETY: localtime_r only writes the tm we own.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if !unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
            return (tm.tm_gmtoff / 60) as i32;
        }
    }
    let _ = time;
    0
}

/// `time` in the local time zone: year, month, day, hour, minute, second.
pub fn local_parts(time: i64) -> (i64, i64, i64, i64, i64, i64) {
    let t = time + i64::from(local_offset(time)) * 60;
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    (y, m, d, secs / 3600, secs / 60 % 60, secs % 60)
}

/// The unix time of a local wall-clock time; out-of-range fields roll over
/// (month 0 is December of the year before), as mktime does.
pub fn local_time(y: i64, m: i64, d: i64, h: i64, mi: i64, s: i64) -> i64 {
    #[cfg(unix)]
    {
        // SAFETY: mktime only reads and normalizes the tm we own.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        tm.tm_year = (y - 1900) as i32;
        tm.tm_mon = (m - 1) as i32;
        tm.tm_mday = d as i32;
        tm.tm_hour = h as i32;
        tm.tm_min = mi as i32;
        tm.tm_sec = s as i32;
        tm.tm_isdst = -1;
        let t = unsafe { libc::mktime(&mut tm) };
        if t != -1 {
            return t as i64;
        }
    }
    let (y, m) = (y + (m - 1).div_euclid(12), (m - 1).rem_euclid(12) + 1);
    ((days_from_civil(y, m, d) * 24 + h) * 60 + mi) * 60 + s
}

/// The civil date of a day count since 1970-01-01 (Howard Hinnant, public domain).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant, public domain).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// A date as git prints it for `--date=<style>`: `default`, `short`, `iso`,
/// `iso-strict`, `rfc`, `unix`, `raw`, `relative`, `format:<strftime>`, and
/// any of them with `-local` (or plain `local`) for the local time zone.
pub fn format_date(time: i64, offset: i32, style: &str) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let local_tz =
        style == "local" || style.ends_with("-local") || style.starts_with("format-local:");
    let offset = if local_tz { local_offset(time) } else { offset };
    let style = match style.strip_suffix("-local") {
        Some(s) => s,
        None if style == "local" => "default",
        None => style,
    };
    let sign = if offset < 0 { '-' } else { '+' };
    let (oh, om) = (offset.abs() / 60, offset.abs() % 60);
    let local = time + i64::from(offset) * 60;
    let days = local.div_euclid(86400);
    let secs = local.rem_euclid(86400);
    let (h, mi, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    let (y, m, d) = civil_from_days(days);
    let wday = (days + 4).rem_euclid(7) as usize;
    let wd = DAYS[wday];
    let mon = MONTHS[(m - 1) as usize];
    if let Some(f) = style
        .strip_prefix("format:")
        .or(style.strip_prefix("format-local:"))
    {
        const LONG_DAYS: [&str; 7] = [
            "Sunday",
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
        ];
        const LONG_MONTHS: [&str; 12] = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        let yday = days - days_from_civil(y, 1, 1) + 1;
        let h12 = if h % 12 == 0 { 12 } else { h % 12 };
        let mut out = String::new();
        let mut it = f.chars();
        while let Some(ch) = it.next() {
            if ch != '%' {
                out.push(ch);
                continue;
            }
            let Some(spec) = it.next() else {
                out.push('%');
                break;
            };
            out.push_str(&match spec {
                'Y' => y.to_string(),
                'y' => format!("{:02}", y % 100),
                'm' => format!("{m:02}"),
                'd' => format!("{d:02}"),
                'e' => format!("{d:>2}"),
                'H' => format!("{h:02}"),
                'I' => format!("{h12:02}"),
                'M' => format!("{mi:02}"),
                'S' => format!("{s:02}"),
                'p' => (if h < 12 { "AM" } else { "PM" }).to_owned(),
                'j' => format!("{yday:03}"),
                'a' => wd.to_owned(),
                'A' => LONG_DAYS[wday].to_owned(),
                'b' | 'h' => mon.to_owned(),
                'B' => LONG_MONTHS[(m - 1) as usize].to_owned(),
                'u' => (if wday == 0 { 7 } else { wday }).to_string(),
                'w' => wday.to_string(),
                'z' => format!("{sign}{oh:02}{om:02}"),
                'Z' => String::new(),
                's' => time.to_string(),
                'F' => format!("{y:04}-{m:02}-{d:02}"),
                'T' => format!("{h:02}:{mi:02}:{s:02}"),
                'R' => format!("{h:02}:{mi:02}"),
                'D' => format!("{m:02}/{d:02}/{:02}", y % 100),
                'n' => "\n".to_owned(),
                't' => "\t".to_owned(),
                '%' => "%".to_owned(),
                other => format!("%{other}"),
            });
        }
        return out;
    }
    match style {
        "short" => format!("{y:04}-{m:02}-{d:02}"),
        "iso" | "iso8601" => {
            format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02} {sign}{oh:02}{om:02}")
        }
        "iso-strict" | "iso8601-strict" if offset == 0 => {
            format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
        }
        "iso-strict" | "iso8601-strict" => {
            format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}{sign}{oh:02}:{om:02}")
        }
        "rfc" | "rfc2822" => {
            format!("{wd}, {d} {mon} {y} {h:02}:{mi:02}:{s:02} {sign}{oh:02}{om:02}")
        }
        "unix" => time.to_string(),
        "raw" => format!("{time} {sign}{oh:02}{om:02}"),
        "relative" => relative(time),
        "human" => {
            // git's show_date_normal against the local time now: drop what
            // the reader already knows.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64);
            let (ny, nm, nd, ..) = local_parts(now);
            let same_year = y == ny;
            let (mut hide_date, mut hide_wday) = (false, false);
            if same_year && m == nm {
                if d == nd {
                    (hide_date, hide_wday) = (true, true);
                } else if d < nd && d + 5 > nd {
                    hide_date = true;
                }
            }
            if hide_wday {
                return relative(time);
            }
            let hide_tz = offset == local_offset(now) || !hide_date;
            let mut out = String::new();
            if same_year {
                out.push_str(&format!("{wd} "));
            }
            if !hide_date {
                out.push_str(&format!("{mon} {d} "));
            }
            if same_year {
                out.push_str(&format!("{h:02}:{mi:02}"));
            } else {
                out.truncate(out.trim_end().len());
                out.push_str(&format!(" {y}"));
            }
            if !hide_tz {
                out.push_str(&format!(" {sign}{oh:02}{om:02}"));
            }
            out
        }
        _ if local_tz => format!("{wd} {mon} {d} {h:02}:{mi:02}:{s:02} {y}"),
        _ => format!("{wd} {mon} {d} {h:02}:{mi:02}:{s:02} {y} {sign}{oh:02}{om:02}"),
    }
}

/// git's show_date_relative.
fn relative(time: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    if now < time {
        return "in the future".to_owned();
    }
    let plural = |n: i64, unit: &str| format!("{n} {unit}{}", if n == 1 { "" } else { "s" });
    let mut diff = now - time;
    if diff < 90 {
        return format!("{} ago", plural(diff, "second"));
    }
    diff = (diff + 30) / 60;
    if diff < 90 {
        return format!("{} ago", plural(diff, "minute"));
    }
    diff = (diff + 30) / 60;
    if diff < 36 {
        return format!("{} ago", plural(diff, "hour"));
    }
    diff = (diff + 12) / 24;
    if diff < 14 {
        return format!("{} ago", plural(diff, "day"));
    }
    if diff < 70 {
        return format!("{} ago", plural((diff + 3) / 7, "week"));
    }
    if diff < 365 {
        return format!("{} ago", plural((diff + 15) / 30, "month"));
    }
    if diff < 1825 {
        let total = (diff * 12 * 2 + 365) / (365 * 2);
        let (years, months) = (total / 12, total % 12);
        return if months > 0 {
            format!("{}, {} ago", plural(years, "year"), plural(months, "month"))
        } else {
            format!("{} ago", plural(years, "year"))
        };
    }
    format!("{} ago", plural((diff + 183) / 365, "year"))
}

/// The notes ref git reads and writes by default: $GIT_NOTES_REF, else
/// core.notesRef, else refs/notes/commits.
pub(crate) fn default_notes_ref(backend: &Arc<dyn GitBackend>) -> String {
    std::env::var("GIT_NOTES_REF")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| backend.config_get("core.notesRef").ok().flatten())
        .unwrap_or_else(|| "refs/notes/commits".to_owned())
}

/// The notes refs log and show display, in git's order (notes.c's
/// load_display_notes): the default ref and $GIT_NOTES_DISPLAY_REF or
/// notes.displayRef unless `use_default` is false (or unset while `extra`
/// names refs), then `extra`. Globs expand to the refs they match.
pub(crate) fn display_notes_refs(
    backend: &Arc<dyn GitBackend>,
    use_default: Option<bool>,
    extra: &[String],
) -> Vec<String> {
    let mut patterns = Vec::new();
    if use_default.unwrap_or(extra.is_empty()) {
        patterns.push(default_notes_ref(backend));
        match std::env::var("GIT_NOTES_DISPLAY_REF") {
            Ok(env) => patterns.extend(env.split(':').filter(|p| !p.is_empty()).map(str::to_owned)),
            Err(_) => patterns.extend(
                backend
                    .config_entries(rgit_git::ConfigScope::Any, Some("notes.displayRef"))
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(_, v)| v),
            ),
        }
    }
    patterns.extend(extra.iter().cloned());
    let mut refs: Vec<String> = Vec::new();
    let mut all = None;
    for p in patterns {
        let found = if p.contains(['*', '?', '[', '\\']) {
            let names = all.get_or_insert_with(|| {
                backend
                    .ref_details()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|r| r.name)
                    .collect::<Vec<_>>()
            });
            names
                .iter()
                .filter(|n| crate::plumbing::glob(p.as_bytes(), n.as_bytes()))
                .cloned()
                .collect()
        } else {
            vec![p]
        };
        for r in found {
            if !refs.contains(&r) {
                refs.push(r);
            }
        }
    }
    refs
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
        assert_eq!(
            format_date(t, -300, "format:%Y/%m/%d %H:%M %a %j %z"),
            "2026/09/27 14:17 Sun 270 -0500"
        );
    }

    #[test]
    fn messages_split_like_git() {
        let m = "\nsub\nject  \n\n\nbody\n\nmore\n";
        assert_eq!(subject(m, " "), "sub ject");
        assert_eq!(body(m), "body\n\nmore\n");
        assert_eq!(body("one\n"), "");
        assert_eq!(sanitize("fix: the  bug..now!"), "fix-the-bug.now");
    }
}
