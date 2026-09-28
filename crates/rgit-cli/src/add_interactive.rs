//! git's `add -i` menu (add-interactive.c): status, update, revert, add
//! untracked, patch, diff, quit and help, with git's screens and prompts.

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::Arc;

use rgit_git::GitBackend;

use crate::add_patch::{Colors, colors, read_stdin_line};

#[derive(Clone, Copy, Default)]
struct AddDel {
    add: usize,
    del: usize,
    seen: bool,
    unmerged: bool,
    binary: bool,
}

#[derive(Clone, Default)]
struct FileItem {
    index: AddDel,
    worktree: AddDel,
}

/// A list the user picks from by number, range or unique prefix.
struct Items {
    names: Vec<String>,
    prefix: Vec<usize>,
    selected: Vec<bool>,
}

impl Items {
    fn new(names: Vec<String>) -> Self {
        let n = names.len();
        Self {
            names,
            prefix: vec![0; n],
            selected: vec![false; n],
        }
    }

    fn sorted(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.names.len()).collect();
        order.sort_by(|&a, &b| self.names[a].as_bytes().cmp(self.names[b].as_bytes()));
        order
    }

    /// git's find_unique_prefixes, with prefixes of 1 to 4 characters.
    fn find_unique_prefixes(&mut self) {
        let order = self.sorted();
        for (k, &i) in order.iter().enumerate() {
            let s = self.names[i].as_bytes();
            let mut len = match s.first() {
                Some(c) if c.is_ascii() => 1,
                _ => 0,
            };
            let extend = |other: &[u8], len: &mut usize| {
                if *len == 0 || s.get(..*len) != other.get(..*len) {
                    return;
                }
                loop {
                    let c = s.get(*len).copied().unwrap_or(0);
                    *len += 1;
                    if c == 0 || *len > 4 || !c.is_ascii() {
                        *len = 0;
                        break;
                    }
                    if Some(&c) != other.get(*len - 1) {
                        break;
                    }
                }
            };
            if k > 0 {
                extend(self.names[order[k - 1]].as_bytes(), &mut len);
            }
            if k + 1 < order.len() {
                extend(self.names[order[k + 1]].as_bytes(), &mut len);
            }
            self.prefix[i] = len;
        }
    }

    /// The item `s` names by a unique prefix (or in full).
    fn find_unique(&self, s: &str) -> Option<usize> {
        let order = self.sorted();
        let names: Vec<&str> = order.iter().map(|&i| self.names[i].as_str()).collect();
        match names.binary_search_by(|n| n.as_bytes().cmp(s.as_bytes())) {
            Ok(k) => Some(order[k]),
            Err(k) => {
                if k > 0 && names[k - 1].starts_with(s) {
                    return None;
                }
                if k + 1 < names.len() && names[k + 1].starts_with(s) {
                    return None;
                }
                (k < names.len() && names[k].starts_with(s)).then(|| order[k])
            }
        }
    }
}

/// git's is_valid_prefix: one that cannot be read as another command.
fn valid_prefix(name: &str, len: usize) -> bool {
    let b = name.as_bytes();
    len > 0
        && b[..len.min(b.len())]
            .iter()
            .all(|c| !b" \t\r\n,".contains(c))
        && b[0] != b'-'
        && !b[0].is_ascii_digit()
        && (len != 1 || (b[0] != b'*' && b[0] != b'?'))
}

const QUIT: isize = -2;
const ERROR: isize = -1;

struct Ui<'a> {
    backend: &'a Arc<dyn GitBackend>,
    c: Colors,
    hl: (String, String),
    paths: Vec<String>,
}

enum Print<'a> {
    Command,
    File(&'a BTreeMap<String, FileItem>),
    Name,
}

impl Ui<'_> {
    fn print(&self, text: &str) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    }

    fn colored_ln(&self, color: &str, text: &str) -> String {
        if color.is_empty() {
            format!("{text}\n")
        } else {
            format!("{color}{text}{}\n", self.c.reset)
        }
    }

    fn highlighted(&self, items: &Items, i: usize) -> String {
        let (name, len) = (&items.names[i], items.prefix[i]);
        if len > 0 && valid_prefix(name, len) {
            format!("{}{}{}{}", self.hl.0, &name[..len], self.hl.1, &name[len..])
        } else {
            name.clone()
        }
    }

    fn item_text(&self, items: &Items, i: usize, how: &Print) -> String {
        let name = self.highlighted(items, i);
        match how {
            Print::Command => format!(" {:2}: {name}", i + 1),
            Print::Name => format!(
                "{}{:2}: {name}",
                if items.selected[i] { '*' } else { ' ' },
                i + 1
            ),
            Print::File(files) => {
                let f = &files[&items.names[i]];
                format!(
                    "{}{:2}: {:>12} {:>12} {name}",
                    if items.selected[i] { '*' } else { ' ' },
                    i + 1,
                    adddel(&f.index, "unchanged"),
                    adddel(&f.worktree, "nothing")
                )
            }
        }
    }

    fn list(&self, items: &Items, header: Option<&str>, columns: usize, how: &Print) {
        if items.names.is_empty() {
            return;
        }
        let mut out = String::new();
        if let Some(h) = header {
            out.push_str(&self.colored_ln(&self.c.header, h));
        }
        let mut last_lf = false;
        for i in 0..items.names.len() {
            out.push_str(&self.item_text(items, i, how));
            if columns > 0 && (i + 1) % columns != 0 {
                out.push('\t');
                last_lf = false;
            } else {
                out.push('\n');
                last_lf = true;
            }
        }
        if !last_lf {
            out.push('\n');
        }
        self.print(&out);
    }

    /// git's list_and_choose: the chosen index for a single choice, else how
    /// many are selected; ERROR, or QUIT at the end of input.
    #[allow(clippy::too_many_arguments)]
    fn list_and_choose(
        &self,
        items: &mut Items,
        header: Option<&str>,
        columns: usize,
        how: &Print,
        prompt: &str,
        singleton: bool,
        immediate: bool,
        help: &dyn Fn(&Self),
    ) -> isize {
        let mut res: isize = if singleton { ERROR } else { 0 };
        if !singleton {
            items.selected = vec![false; items.names.len()];
        }
        items.find_unique_prefixes();
        let n = items.names.len() as isize;
        loop {
            self.list(items, header, columns, how);
            let p = if self.c.prompt.is_empty() {
                prompt.to_owned()
            } else {
                format!("{}{prompt}{}", self.c.prompt, self.c.reset)
            };
            self.print(&format!("{p}{}", if singleton { "> " } else { ">> " }));
            let Some(input) = read_stdin_line() else {
                self.print("\n");
                if immediate {
                    res = QUIT;
                }
                break;
            };
            if input.is_empty() {
                break;
            }
            if input == "?" {
                help(self);
                continue;
            }
            let bytes = input.as_bytes();
            let mut at = 0;
            loop {
                let sep = bytes[at..]
                    .iter()
                    .position(|c| b" \t\r\n,".contains(c))
                    .unwrap_or(bytes.len() - at);
                if sep == 0 {
                    if at >= bytes.len() {
                        break;
                    }
                    at += 1;
                    continue;
                }
                let mut token = &input[at..at + sep];
                let mut choose = true;
                if let Some(rest) = token.strip_prefix('-') {
                    choose = false;
                    token = rest;
                }
                let (mut from, mut to): (isize, isize) = (-1, -1);
                if token == "*" {
                    from = 0;
                    to = n;
                } else if token.starts_with(|c: char| c.is_ascii_digit()) {
                    let digits = token.bytes().take_while(u8::is_ascii_digit).count();
                    from = token[..digits].parse::<isize>().unwrap_or(0) - 1;
                    let rest = &token[digits..];
                    if rest.is_empty() {
                        to = from + 1;
                    } else if let Some(r) = rest.strip_prefix('-') {
                        let d2 = r.bytes().take_while(u8::is_ascii_digit).count();
                        to = if d2 > 0 {
                            r[..d2].parse().unwrap_or(0)
                        } else {
                            n
                        };
                        if d2 != r.len() {
                            from = -1;
                        }
                    }
                }
                if from < 0
                    && let Some(i) = items.find_unique(token)
                {
                    from = i as isize;
                    to = from + 1;
                }
                if from < 0 || from >= n || (singleton && from + 1 != to) {
                    let msg = format!("Huh ({token})?");
                    let _ = std::io::stdout().flush();
                    eprint!("{}", self.colored_ln(&self.c.error, &msg));
                    break;
                } else if singleton {
                    res = from;
                    break;
                }
                let to = to.min(n);
                for i in from..to.max(from) {
                    let i = i as usize;
                    if items.selected[i] != choose {
                        items.selected[i] = choose;
                        res += if choose { 1 } else { -1 };
                    }
                }
                at += sep;
                if at >= bytes.len() {
                    break;
                }
            }
            if (immediate && res != ERROR) || input == "*" {
                break;
            }
        }
        res
    }

    fn choose_help(&self) {
        let h = &self.c.help;
        let mut out = String::new();
        for line in [
            "Prompt help:",
            "1          - select a single item",
            "3-5        - select a range of items",
            "2-3,6-9    - select multiple ranges",
            "foo        - select item based on unique prefix",
            "-...       - unselect specified items",
            "*          - choose all items",
            "           - (empty) finish selecting",
        ] {
            out.push_str(&self.colored_ln(h, line));
        }
        self.print(&out);
    }

    /// Paths with changes and their +/- counts; `filter` keeps only the
    /// worktree (1) or index (2) changes.
    fn modified(&self, filter: u8) -> anyhow::Result<(BTreeMap<String, FileItem>, usize, usize)> {
        let mut files: BTreeMap<String, FileItem> = BTreeMap::new();
        let (mut unmerged, mut binary) = (0, 0);
        let order: [bool; 2] = if filter == 2 {
            [true, false]
        } else {
            [false, true]
        };
        for (k, from_index) in order.into_iter().enumerate() {
            let skip_unseen = filter != 0 && k == 1;
            let text = if from_index {
                self.backend
                    .patch_diff(Some("HEAD"), true, false, None, &self.paths)?
            } else {
                self.backend
                    .patch_diff(None, false, false, None, &self.paths)?
            };
            for (name, stat) in numstat(&text) {
                if skip_unseen && !files.contains_key(&name) {
                    continue;
                }
                let item = files.entry(name).or_default();
                let (mine, other) = if from_index {
                    (&mut item.index, &item.worktree)
                } else {
                    (&mut item.worktree, &item.index)
                };
                if stat.binary && !other.binary {
                    binary += 1;
                }
                if stat.unmerged && !other.unmerged {
                    unmerged += 1;
                }
                *mine = stat;
            }
        }
        Ok((files, unmerged, binary))
    }

    fn file_header(&self) -> String {
        format!("     {:>12} {:>12} {}", "staged", "unstaged", "path")
    }

    fn run_status(&self) -> anyhow::Result<()> {
        let (files, ..) = self.modified(0)?;
        let items = Items::new(files.keys().cloned().collect());
        self.list(&items, Some(&self.file_header()), 0, &Print::File(&files));
        self.print("\n");
        Ok(())
    }

    fn run_update(&self) -> anyhow::Result<()> {
        let (files, ..) = self.modified(1)?;
        if files.is_empty() {
            self.print("\n");
            return Ok(());
        }
        let mut items = Items::new(files.keys().cloned().collect());
        let header = self.file_header();
        let count = self.list_and_choose(
            &mut items,
            Some(&header),
            0,
            &Print::File(&files),
            "Update",
            false,
            false,
            &|ui| ui.choose_help(),
        );
        if count <= 0 {
            self.print("\n");
            return Ok(());
        }
        // Tracked paths only: a deleted file's entry goes.
        self.backend.add(&selected(&items), true, false)?;
        let s = if count == 1 { "" } else { "s" };
        self.print(&format!("updated {count} path{s}\n\n"));
        Ok(())
    }

    fn run_revert(&self) -> anyhow::Result<()> {
        let (files, ..) = self.modified(2)?;
        if files.is_empty() {
            self.print("\n");
            return Ok(());
        }
        let mut items = Items::new(files.keys().cloned().collect());
        let header = self.file_header();
        let count = self.list_and_choose(
            &mut items,
            Some(&header),
            0,
            &Print::File(&files),
            "Revert",
            false,
            false,
            &|ui| ui.choose_help(),
        );
        if count > 0 {
            let picked = selected(&items);
            let head = self.backend.rev_parse("HEAD").is_ok();
            let mut notes = String::new();
            for p in &picked {
                let in_head = head && self.backend.read_blob("HEAD", p).is_ok();
                if in_head {
                    self.backend.reset_paths("HEAD", std::slice::from_ref(p))?;
                } else {
                    self.backend.unstage_file(p)?;
                    notes.push_str(&format!("note: {p} is untracked now.\n"));
                }
            }
            let s = if count == 1 { "" } else { "s" };
            self.print(&format!("{notes}reverted {count} path{s}\n"));
        }
        self.print("\n");
        Ok(())
    }

    fn untracked(&self) -> anyhow::Result<Vec<String>> {
        let opts = rgit_git::StatusOpts {
            format: Some(rgit_git::StatusFormat::Porcelain),
            null: true,
            untracked: Some("all".to_owned()),
            paths: self.paths.clone(),
            ..Default::default()
        };
        let text = self.backend.status_text(&opts)?.text;
        Ok(text
            .split(|&b| b == 0)
            .filter_map(|e| e.strip_prefix(b"?? "))
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect())
    }

    fn run_add_untracked(&self) -> anyhow::Result<()> {
        let names = self.untracked()?;
        if names.is_empty() {
            self.print("No untracked files.\n\n");
            return Ok(());
        }
        let mut items = Items::new(names);
        let count = self.list_and_choose(
            &mut items,
            Some(&self.file_header()),
            0,
            &Print::Name,
            "Add untracked",
            false,
            false,
            &|ui| ui.choose_help(),
        );
        if count > 0 {
            self.backend.add(&selected(&items), false, false)?;
            let s = if count == 1 { "" } else { "s" };
            self.print(&format!("added {count} path{s}\n"));
        }
        self.print("\n");
        Ok(())
    }

    fn run_patch(&self) -> anyhow::Result<()> {
        let (mut files, unmerged, binary) = self.modified(1)?;
        if unmerged > 0 || binary > 0 {
            let mut kept = BTreeMap::new();
            for (name, f) in files {
                if f.index.binary || f.worktree.binary {
                    continue;
                }
                if f.index.unmerged || f.worktree.unmerged {
                    let _ = std::io::stdout().flush();
                    eprint!(
                        "{}",
                        self.colored_ln(&self.c.error, &format!("ignoring unmerged: {name}"))
                    );
                    continue;
                }
                kept.insert(name, f);
            }
            files = kept;
        }
        if files.is_empty() {
            let _ = std::io::stdout().flush();
            eprintln!(
                "{}",
                if binary > 0 {
                    "Only binary files changed."
                } else {
                    "No changes."
                }
            );
            return Ok(());
        }
        let mut items = Items::new(files.keys().cloned().collect());
        let header = self.file_header();
        let count = self.list_and_choose(
            &mut items,
            Some(&header),
            0,
            &Print::File(&files),
            "Patch update",
            false,
            false,
            &|ui| ui.choose_help(),
        );
        if count > 0 {
            crate::add_patch::run(
                self.backend,
                crate::add_patch::Kind::Add,
                None,
                &selected(&items),
            )?;
        }
        Ok(())
    }

    fn run_diff(&self) -> anyhow::Result<()> {
        let (files, ..) = self.modified(2)?;
        if files.is_empty() {
            self.print("\n");
            return Ok(());
        }
        let mut items = Items::new(files.keys().cloned().collect());
        let header = self.file_header();
        let count = self.list_and_choose(
            &mut items,
            Some(&header),
            0,
            &Print::File(&files),
            "Review diff",
            false,
            true,
            &|ui| ui.choose_help(),
        );
        if count > 0 {
            let text =
                self.backend
                    .patch_diff(Some("HEAD"), true, false, None, &selected(&items))?;
            let text = if self.c.on {
                crate::add_patch::colorize(&text, &self.c)
            } else {
                text
            };
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(&text);
            let _ = out.flush();
        }
        self.print("\n");
        Ok(())
    }

    fn run_help(&self) {
        let mut out = String::new();
        for line in [
            "status        - show paths with changes",
            "update        - add working tree state to the staged set of changes",
            "revert        - revert staged set of changes back to the HEAD version",
            "patch         - pick hunks and update selectively",
            "diff          - view diff between HEAD and index",
            "add untracked - add contents of untracked files to the staged set of changes",
        ] {
            out.push_str(&self.colored_ln(&self.c.help, line));
        }
        self.print(&out);
    }

    fn command_help(&self) {
        let mut out = String::new();
        for line in [
            "Prompt help:",
            "1          - select a numbered item",
            "foo        - select item based on unique prefix",
            "           - (empty) select nothing",
        ] {
            out.push_str(&self.colored_ln(&self.c.help, line));
        }
        self.print(&out);
    }
}

fn selected(items: &Items) -> Vec<String> {
    items
        .names
        .iter()
        .zip(&items.selected)
        .filter(|(_, s)| **s)
        .map(|(n, _)| n.clone())
        .collect()
}

fn adddel(ad: &AddDel, none: &str) -> String {
    if ad.binary {
        "binary".to_owned()
    } else if ad.seen {
        format!("+{}/-{}", ad.add, ad.del)
    } else {
        none.to_owned()
    }
}

/// Per path: lines added and removed, binary and unmerged, from a plain
/// patch as `patch_diff` prints it.
fn numstat(text: &[u8]) -> Vec<(String, AddDel)> {
    let mut out: Vec<(String, AddDel)> = Vec::new();
    let mut in_hunks = false;
    for line in text.split(|&b| b == b'\n') {
        if let Some(path) = line.strip_prefix(b"* Unmerged path ") {
            out.push((
                String::from_utf8_lossy(path).into_owned(),
                AddDel {
                    seen: true,
                    unmerged: true,
                    ..AddDel::default()
                },
            ));
            in_hunks = false;
        } else if let Some(rest) = line.strip_prefix(b"diff --git ") {
            let rest = String::from_utf8_lossy(rest);
            let name = rest
                .rsplit_once(" b/")
                .map_or(rest.as_ref(), |(_, b)| b)
                .to_owned();
            out.push((
                name,
                AddDel {
                    seen: true,
                    ..AddDel::default()
                },
            ));
            in_hunks = false;
        } else if let Some((_, ad)) = out.last_mut() {
            if line.starts_with(b"@@ ") {
                in_hunks = true;
            } else if !in_hunks && line.starts_with(b"Binary files ") {
                ad.binary = true;
            } else if in_hunks && line.starts_with(b"+") {
                ad.add += 1;
            } else if in_hunks && line.starts_with(b"-") {
                ad.del += 1;
            }
        }
    }
    out
}

/// git's `add -i`.
pub fn run(backend: &Arc<dyn GitBackend>, paths: &[String]) -> anyhow::Result<()> {
    let c = colors(backend);
    let hl = if c.on {
        (c.prompt.clone(), c.reset.clone())
    } else {
        ("[".to_owned(), "]".to_owned())
    };
    let ui = Ui {
        backend,
        c,
        hl,
        paths: paths.to_vec(),
    };
    const COMMANDS: [&str; 8] = [
        "status",
        "update",
        "revert",
        "add untracked",
        "patch",
        "diff",
        "quit",
        "help",
    ];
    let mut commands = Items::new(COMMANDS.iter().map(|s| (*s).to_owned()).collect());
    ui.run_status()?;
    loop {
        let i = ui.list_and_choose(
            &mut commands,
            Some("*** Commands ***"),
            4,
            &Print::Command,
            "What now",
            true,
            true,
            &|ui| ui.command_help(),
        );
        let cmd = (i >= 0).then(|| COMMANDS[i as usize]);
        if i == QUIT || cmd == Some("quit") {
            ui.print("Bye.\n");
            return Ok(());
        }
        match cmd {
            Some("status") => ui.run_status()?,
            Some("update") => ui.run_update()?,
            Some("revert") => ui.run_revert()?,
            Some("add untracked") => ui.run_add_untracked()?,
            Some("patch") => ui.run_patch()?,
            Some("diff") => ui.run_diff()?,
            Some("help") => ui.run_help(),
            _ => {}
        }
    }
}
