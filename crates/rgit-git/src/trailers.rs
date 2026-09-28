//! `git interpret-trailers`: git's trailer.c - finding the trailer block,
//! parsing it, merging in configured and command-line trailers, and
//! printing the message back.

/// Where a new trailer goes (`--where`, trailer.where).
#[derive(Clone, Copy, PartialEq)]
pub enum Where {
    End,
    After,
    Before,
    Start,
}

/// What to do when the trailer is already there (`--if-exists`).
#[derive(Clone, Copy, PartialEq)]
pub enum IfExists {
    AddIfDifferent,
    AddIfDifferentNeighbor,
    Add,
    Replace,
    DoNothing,
}

/// What to do when the trailer is not there (`--if-missing`).
#[derive(Clone, Copy, PartialEq)]
pub enum IfMissing {
    DoNothing,
    Add,
}

pub fn parse_where(s: &str) -> Option<Where> {
    Some(match s.to_ascii_lowercase().as_str() {
        "end" => Where::End,
        "after" => Where::After,
        "before" => Where::Before,
        "start" => Where::Start,
        _ => return None,
    })
}

pub fn parse_if_exists(s: &str) -> Option<IfExists> {
    Some(match s.to_ascii_lowercase().as_str() {
        "addifdifferent" => IfExists::AddIfDifferent,
        "addifdifferentneighbor" => IfExists::AddIfDifferentNeighbor,
        "add" => IfExists::Add,
        "replace" => IfExists::Replace,
        "donothing" => IfExists::DoNothing,
        _ => return None,
    })
}

pub fn parse_if_missing(s: &str) -> Option<IfMissing> {
    Some(match s.to_ascii_lowercase().as_str() {
        "donothing" => IfMissing::DoNothing,
        "add" => IfMissing::Add,
        _ => return None,
    })
}

/// A `--trailer` with the placement options in force where it was given.
pub struct NewTrailer {
    pub text: String,
    pub where_: Option<Where>,
    pub if_exists: Option<IfExists>,
    pub if_missing: Option<IfMissing>,
}

#[derive(Default)]
pub struct TrailerOpts {
    pub trim_empty: bool,
    pub only_trailers: bool,
    pub only_input: bool,
    pub unfold: bool,
    pub no_divider: bool,
    pub trailers: Vec<NewTrailer>,
}

#[derive(Clone)]
struct Conf {
    name: String,
    key: Option<String>,
    command: Option<String>,
    cmd: Option<String>,
    where_: Where,
    if_exists: IfExists,
    if_missing: IfMissing,
}

struct Config {
    separators: String,
    items: Vec<Conf>,
}

/// C's isspace: space, \t, \n, \v, \f, \r.
fn space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

fn trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_ascii() && space(c as u8))
}

/// trailer.* settings from `config` (name, value) pairs in file order.
fn load(config: &[(String, Option<String>)]) -> Config {
    let mut default = Conf {
        name: String::new(),
        key: None,
        command: None,
        cmd: None,
        where_: Where::End,
        if_exists: IfExists::AddIfDifferentNeighbor,
        if_missing: IfMissing::Add,
    };
    let mut separators = ":".to_owned();
    let entries = config.iter().filter_map(|(k, v)| {
        let rest = k.strip_prefix("trailer.").or_else(|| {
            k.to_ascii_lowercase()
                .starts_with("trailer.")
                .then(|| &k["trailer.".len()..])
        })?;
        Some((rest, v.as_deref()))
    });
    for (item, value) in entries.clone() {
        if item.contains('.') {
            continue;
        }
        let v = value.unwrap_or("");
        match item.to_ascii_lowercase().as_str() {
            "where" => {
                if let Some(w) = parse_where(v) {
                    default.where_ = w;
                }
            }
            "ifexists" => {
                if let Some(x) = parse_if_exists(v) {
                    default.if_exists = x;
                }
            }
            "ifmissing" => {
                if let Some(x) = parse_if_missing(v) {
                    default.if_missing = x;
                }
            }
            "separators" => {
                if let Some(v) = value {
                    separators = v.to_owned();
                }
            }
            _ => {}
        }
    }
    let mut items: Vec<Conf> = Vec::new();
    for (item, value) in entries {
        let Some((name, var)) = item.rsplit_once('.') else {
            continue;
        };
        let var = var.to_ascii_lowercase();
        if !["key", "command", "cmd", "where", "ifexists", "ifmissing"].contains(&var.as_str()) {
            continue;
        }
        let i = match items.iter().position(|c| c.name.eq_ignore_ascii_case(name)) {
            Some(i) => i,
            None => {
                let mut c = default.clone();
                c.name = name.to_owned();
                items.push(c);
                items.len() - 1
            }
        };
        let c = &mut items[i];
        let v = value.unwrap_or("");
        match var.as_str() {
            "key" => c.key = value.map(str::to_owned),
            "command" => c.command = value.map(str::to_owned),
            "cmd" => c.cmd = value.map(str::to_owned),
            "where" => c.where_ = parse_where(v).unwrap_or(c.where_),
            "ifexists" => c.if_exists = parse_if_exists(v).unwrap_or(c.if_exists),
            _ => c.if_missing = parse_if_missing(v).unwrap_or(c.if_missing),
        }
    }
    items.insert(0, default);
    Config { separators, items }
}

impl Config {
    fn default(&self) -> &Conf {
        &self.items[0]
    }
    fn named(&self) -> &[Conf] {
        &self.items[1..]
    }
}

/// Where the separator is in a `token: value` line, if the line starts with a
/// token (letters, digits, `-`, then optional blanks).
fn find_separator(line: &[u8], separators: &[u8]) -> Option<usize> {
    let mut ws = false;
    for (i, &c) in line.iter().enumerate() {
        if separators.contains(&c) {
            return Some(i);
        }
        if !ws && (c.is_ascii_alphanumeric() || c == b'-') {
            continue;
        }
        if i > 0 && (c == b' ' || c == b'\t') {
            ws = true;
            continue;
        }
        break;
    }
    None
}

fn token_len(tok: &str) -> usize {
    tok.bytes()
        .rposition(|b| b.is_ascii_alphanumeric())
        .map_or(0, |i| i + 1)
}

/// C's `!strncasecmp(a, b, n)`.
fn ncase_eq(a: &str, b: &str, n: usize) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    for i in 0..n {
        let (x, y) = (a.get(i).copied(), b.get(i).copied());
        if x.map(|c| c.to_ascii_lowercase()) != y.map(|c| c.to_ascii_lowercase()) {
            return false;
        }
        if x.is_none() {
            return true;
        }
    }
    true
}

fn matches_item(tok: &str, c: &Conf, len: usize) -> bool {
    ncase_eq(tok, &c.name, len) || c.key.as_deref().is_some_and(|k| ncase_eq(tok, k, len))
}

/// git's `parse_trailer`: the trimmed token (or its configured key) and value,
/// and the matching trailer.<name> settings.
fn parse_trailer(cfg: &Config, text: &str, sep: Option<usize>) -> (String, String, Conf) {
    let (tok, val) = match sep {
        Some(p) => (trim(&text[..p]), trim(&text[p + 1..])),
        None => (trim(text), ""),
    };
    let len = token_len(tok);
    for c in cfg.named() {
        if matches_item(tok, c, len) {
            let tok = c.key.clone().unwrap_or_else(|| tok.to_owned());
            return (tok, val.to_owned(), c.clone());
        }
    }
    (tok.to_owned(), val.to_owned(), cfg.default().clone())
}

fn unfold(val: &str) -> String {
    let b = val.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        i += 1;
        if c == b'\n' {
            while i < b.len() && space(b[i]) {
                i += 1;
            }
            out.push(b' ');
        } else {
            out.push(c);
        }
    }
    trim(&String::from_utf8_lossy(&out)).to_owned()
}

fn next_line(buf: &[u8], s: usize) -> usize {
    buf[s..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(buf.len(), |i| s + i + 1)
}

/// The start of the last line in `buf[..len]` (a final newline belongs to it).
fn last_line(buf: &[u8], len: usize) -> Option<usize> {
    match len {
        0 => None,
        1 => Some(0),
        _ => Some(
            buf[..len - 1]
                .iter()
                .rposition(|&b| b == b'\n')
                .map_or(0, |i| i + 1),
        ),
    }
}

fn is_blank(buf: &[u8], at: usize) -> bool {
    buf[at..]
        .iter()
        .find(|&&b| b == b'\n' || !space(b))
        .is_none_or(|&b| b == b'\n')
}

/// commit.c's `ignored_log_message_bytes`: trailing comments, blank lines,
/// an old `Conflicts:` block and everything from the scissors line.
fn ignored_bytes(buf: &[u8], len: usize, comment: &str) -> usize {
    let pattern = format!("\n{comment} ------------------------ >8 ------------------------\n");
    let pattern = pattern.as_bytes();
    let cutoff = if buf.starts_with(&pattern[1..]) {
        0
    } else if let Some(p) = buf.windows(pattern.len()).position(|w| w == pattern) {
        (p + 1).min(len)
    } else {
        len
    };
    let (mut boc, mut bol, mut conflicts) = (0, 0, false);
    while bol < cutoff {
        let next = buf[bol..len]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(len, |i| bol + i + 1);
        if buf[bol..cutoff].starts_with(comment.as_bytes()) || buf[bol] == b'\n' {
            if boc == 0 {
                boc = bol;
            }
        } else if buf[bol..].starts_with(b"Conflicts:\n") {
            conflicts = true;
            if boc == 0 {
                boc = bol;
            }
        } else if conflicts && buf[bol] == b'\t' {
        } else if boc != 0 {
            boc = 0;
            conflicts = false;
        }
        bol = next;
    }
    if boc != 0 { len - boc } else { len - cutoff }
}

fn end_of_message(buf: &[u8], no_divider: bool, comment: &str) -> usize {
    let mut end = buf.len();
    if !no_divider {
        let mut s = 0;
        while s < buf.len() {
            if buf[s..].starts_with(b"---") && buf.get(s + 3).is_some_and(|&b| space(b)) {
                end = s;
                break;
            }
            s = next_line(buf, s);
        }
    }
    end - ignored_bytes(buf, end, comment)
}

const GIT_PREFIXES: [&[u8]; 2] = [b"Signed-off-by: ", b"(cherry picked from commit "];

fn block_start(buf: &[u8], len: usize, cfg: &Config, comment: &str) -> usize {
    let comment = comment.as_bytes();
    let seps = cfg.separators.as_bytes();
    let mut s = 0;
    while s < len {
        if !buf[s..].starts_with(comment) && is_blank(buf, s) {
            break;
        }
        s = next_line(buf, s);
    }
    let title_end = s;
    let (mut only_spaces, mut recognized) = (true, false);
    let (mut trailers, mut non_trailers, mut continuations) = (0, 0, 0);
    let mut l = last_line(buf, len);
    while let Some(bol) = l.filter(|&b| b >= title_end) {
        l = last_line(buf, bol);
        let line = &buf[bol..];
        if line.starts_with(comment) {
            non_trailers += continuations;
            continuations = 0;
            continue;
        }
        if is_blank(buf, bol) {
            if only_spaces {
                continue;
            }
            non_trailers += continuations;
            if recognized && trailers * 3 >= non_trailers || trailers > 0 && non_trailers == 0 {
                return next_line(buf, bol);
            }
            return len;
        }
        only_spaces = false;
        if GIT_PREFIXES.iter().any(|p| line.starts_with(p)) {
            trailers += 1;
            continuations = 0;
            recognized = true;
            continue;
        }
        match find_separator(line, seps) {
            Some(p) if p >= 1 && !space(line[0]) => {
                trailers += 1;
                continuations = 0;
                if !recognized {
                    let tok = String::from_utf8_lossy(line);
                    recognized = cfg.named().iter().any(|c| matches_item(&tok, c, p));
                }
            }
            _ if space(line[0]) => continuations += 1,
            _ => {
                non_trailers += 1 + continuations;
                continuations = 0;
            }
        }
    }
    len
}

struct Item {
    token: Option<String>,
    value: String,
}

struct Arg {
    token: String,
    value: String,
    conf: Conf,
}

fn same_token(a: &Item, b: &Arg) -> bool {
    let Some(tok) = &a.token else {
        return false;
    };
    ncase_eq(tok, &b.token, token_len(tok).min(token_len(&b.token)))
}

fn same_trailer(a: &Item, b: &Arg) -> bool {
    same_token(a, b) && a.value.eq_ignore_ascii_case(&b.value)
}

fn after_or_end(w: Where) -> bool {
    matches!(w, Where::After | Where::End)
}

/// A trailer.<name>.command or .cmd's output, trimmed, for `arg`.
fn run_command(c: &Conf, arg: &str) -> String {
    let mut sh = std::process::Command::new("sh");
    let shown = if let Some(cmd) = &c.cmd {
        sh.arg("-c").arg(format!("{cmd} \"$@\"")).arg(cmd).arg(arg);
        cmd.clone()
    } else {
        let cmd = c
            .command
            .clone()
            .unwrap_or_default()
            .replacen("$ARG", arg, 1);
        sh.arg("-c").arg(&cmd);
        cmd
    };
    match sh.stdin(std::process::Stdio::null()).output() {
        Ok(out) if out.status.success() => trim(&String::from_utf8_lossy(&out.stdout)).to_owned(),
        _ => {
            eprintln!("error: running trailer command '{shown}' failed");
            String::new()
        }
    }
}

fn apply_command(existing: Option<&Item>, arg: &mut Arg) {
    if arg.conf.command.is_some() || arg.conf.cmd.is_some() {
        let input = if arg.value.is_empty() {
            existing.map(|i| i.value.clone()).unwrap_or_default()
        } else {
            arg.value.clone()
        };
        arg.value = run_command(&arg.conf, &input);
    }
}

fn to_item(arg: &Arg) -> Item {
    Item {
        token: Some(arg.token.clone()),
        value: arg.value.clone(),
    }
}

/// Insert `arg` after (`--where after/end`) or before `on`; the new index.
fn insert_at(list: &mut Vec<Item>, on: usize, arg: &Arg) -> usize {
    let at = if after_or_end(arg.conf.where_) {
        on + 1
    } else {
        on
    };
    list.insert(at, to_item(arg));
    at
}

fn different(list: &[Item], mut i: usize, arg: &Arg, all: bool) -> bool {
    loop {
        if same_trailer(&list[i], arg) {
            return false;
        }
        let next = if after_or_end(arg.conf.where_) {
            i.checked_sub(1)
        } else {
            Some(i + 1).filter(|&n| n < list.len())
        };
        match next {
            Some(n) if all => i = n,
            _ => return true,
        }
    }
}

fn apply_arg(list: &mut Vec<Item>, mut arg: Arg) {
    let w = arg.conf.where_;
    let backwards = after_or_end(w);
    let found = if backwards {
        (0..list.len()).rev().find(|&i| same_token(&list[i], &arg))
    } else {
        (0..list.len()).find(|&i| same_token(&list[i], &arg))
    };
    if let Some(i) = found {
        let on = if matches!(w, Where::After | Where::Before) {
            i
        } else if backwards {
            list.len() - 1
        } else {
            0
        };
        match arg.conf.if_exists {
            IfExists::DoNothing => {}
            IfExists::Replace => {
                apply_command(Some(&list[i]), &mut arg);
                let at = insert_at(list, on, &arg);
                list.remove(if at <= i { i + 1 } else { i });
            }
            IfExists::Add => {
                apply_command(Some(&list[i]), &mut arg);
                insert_at(list, on, &arg);
            }
            IfExists::AddIfDifferent => {
                apply_command(Some(&list[i]), &mut arg);
                if different(list, i, &arg, true) {
                    insert_at(list, on, &arg);
                }
            }
            IfExists::AddIfDifferentNeighbor => {
                apply_command(Some(&list[i]), &mut arg);
                if different(list, on, &arg, false) {
                    insert_at(list, on, &arg);
                }
            }
        }
        return;
    }
    if arg.conf.if_missing == IfMissing::Add {
        apply_command(None, &mut arg);
        if backwards {
            list.push(to_item(&arg));
        } else {
            list.insert(0, to_item(&arg));
        }
    }
}

/// `git interpret-trailers` on one message. `config` is the (name, value)
/// config entries in order; `comment` is core.commentChar.
pub fn interpret_trailers(
    msg: &str,
    o: &TrailerOpts,
    config: &[(String, Option<String>)],
    comment: &str,
) -> String {
    let mut text = msg.to_owned();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    let buf = text.as_bytes();
    let cfg = load(config);
    let seps = cfg.separators.as_bytes();
    let end = end_of_message(buf, o.no_divider, comment);
    let start = block_start(buf, end, &cfg, comment);

    let mut lines: Vec<String> = Vec::new();
    let mut last_is_trailer = false;
    for line in text[start..end].split_inclusive('\n') {
        if last_is_trailer && space(line.as_bytes()[0]) {
            lines.last_mut().expect("a trailer").push_str(line);
            continue;
        }
        last_is_trailer = find_separator(line.as_bytes(), seps).is_some_and(|p| p >= 1);
        lines.push(line.to_owned());
    }
    let mut list = Vec::new();
    for line in &lines {
        if line.starts_with(comment) {
            continue;
        }
        match find_separator(line.as_bytes(), seps).filter(|&p| p >= 1) {
            Some(p) => {
                let (tok, mut val, _) = parse_trailer(&cfg, line, Some(p));
                if o.unfold {
                    val = unfold(&val);
                }
                list.push(Item {
                    token: Some(tok),
                    value: val,
                });
            }
            None if !o.only_trailers => list.push(Item {
                token: None,
                value: line.strip_suffix('\n').unwrap_or(line).to_owned(),
            }),
            None => {}
        }
    }

    let mut out = String::new();
    if !o.only_trailers {
        out.push_str(&text[..start]);
        let blank = last_line(buf, start).is_some_and(|ll| is_blank(buf, ll));
        if !blank {
            out.push('\n');
        }
    }
    if !o.only_input {
        let mut args = Vec::new();
        for c in cfg.named() {
            if c.command.is_some() {
                args.push(Arg {
                    token: c.key.clone().unwrap_or_else(|| c.name.clone()),
                    value: String::new(),
                    conf: c.clone(),
                });
            }
        }
        let cl_seps = format!("={}", cfg.separators);
        for t in &o.trailers {
            let sep = find_separator(t.text.as_bytes(), cl_seps.as_bytes());
            if sep == Some(0) {
                eprintln!("error: empty trailer token in trailer '{}'", trim(&t.text));
                continue;
            }
            let (token, value, mut conf) = parse_trailer(&cfg, &t.text, sep);
            conf.where_ = t.where_.unwrap_or(conf.where_);
            conf.if_exists = t.if_exists.unwrap_or(conf.if_exists);
            conf.if_missing = t.if_missing.unwrap_or(conf.if_missing);
            args.push(Arg { token, value, conf });
        }
        for arg in args {
            apply_arg(&mut list, arg);
        }
    }
    let sep0 = cfg.separators.chars().next().unwrap_or(':');
    for item in &list {
        match &item.token {
            Some(tok) => {
                if o.trim_empty && item.value.is_empty() {
                    continue;
                }
                out.push_str(tok);
                let last = tok.bytes().rev().find(|&b| !space(b));
                if last.is_some_and(|c| !seps.contains(&c)) {
                    out.push(sep0);
                    out.push(' ');
                }
                out.push_str(&item.value);
                out.push('\n');
            }
            None if !o.only_trailers => {
                out.push_str(&item.value);
                out.push('\n');
            }
            None => {}
        }
    }
    if !o.only_trailers {
        out.push_str(&text[end..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_after_existing_trailers() {
        let o = TrailerOpts {
            trailers: vec![NewTrailer {
                text: "Acked-by: B".into(),
                where_: None,
                if_exists: None,
                if_missing: None,
            }],
            ..Default::default()
        };
        assert_eq!(
            interpret_trailers("s\n\nSigned-off-by: A\n", &o, &[], "#"),
            "s\n\nSigned-off-by: A\nAcked-by: B\n"
        );
        assert_eq!(
            interpret_trailers("s", &TrailerOpts::default(), &[], "#"),
            "s\n\n"
        );
    }
}
