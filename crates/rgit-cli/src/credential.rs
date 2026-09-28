//! git's credential protocol, natively: `credential` (fill, approve, reject,
//! capability), the `store` helper over `~/.git-credentials` and the `cache`
//! helper with its unix-socket daemon, which speaks git's wire format so either
//! side can be git's own. These always print git's protocol, in every output
//! mode, and fail as git does (`fatal:`, exit 128).

use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio, exit};

fn fatal(message: &str) -> ! {
    let _ = std::io::stdout().flush();
    eprintln!("fatal: {message}");
    exit(128)
}

fn usage(text: &str) -> ! {
    eprintln!("{text}");
    exit(129)
}

fn strerror(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error") {
        Some(i) => s[..i].to_owned(),
        None => s,
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn git_bool(key: &str, value: &str) -> bool {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" => true,
        "false" | "no" | "off" | "" => false,
        n => match n.parse::<i64>() {
            Ok(n) => n != 0,
            Err(_) => fatal(&format!("bad boolean config value '{value}' for '{key}'")),
        },
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Initial,
    Helper,
    Response,
}

/// Whether the caller (`initial`) and the helper that answered (`helper`)
/// understand a capability; a response carries it only when both do.
#[derive(Default, Clone, Copy)]
struct Capa {
    initial: bool,
    helper: bool,
}

impl Capa {
    fn set(&mut self, op: Op) {
        match op {
            Op::Initial => self.initial = true,
            Op::Helper | Op::Response => self.helper = true,
        }
    }

    fn has(self, op: Op) -> bool {
        match op {
            Op::Initial => false,
            Op::Helper => self.initial,
            Op::Response => self.initial && self.helper,
        }
    }
}

/// git's `struct credential`.
#[derive(Clone)]
struct Cred {
    protocol: Option<String>,
    host: Option<String>,
    path: Option<String>,
    username: Option<String>,
    password: Option<String>,
    credential: Option<String>,
    authtype: Option<String>,
    oauth_refresh_token: Option<String>,
    password_expiry_utc: Option<i64>,
    ephemeral: bool,
    multistage: bool,
    quit: bool,
    username_from_proto: bool,
    wwwauth: Vec<String>,
    state: Vec<String>,
    state_to_send: Vec<String>,
    capa_authtype: Capa,
    capa_state: Capa,
    helpers: Vec<String>,
    configured: bool,
    use_http_path: bool,
    sanitize_prompt: bool,
    protect_protocol: bool,
}

impl Default for Cred {
    fn default() -> Self {
        Cred {
            protocol: None,
            host: None,
            path: None,
            username: None,
            password: None,
            credential: None,
            authtype: None,
            oauth_refresh_token: None,
            password_expiry_utc: None,
            ephemeral: false,
            multistage: false,
            quit: false,
            username_from_proto: false,
            wwwauth: Vec::new(),
            state: Vec::new(),
            state_to_send: Vec::new(),
            capa_authtype: Capa::default(),
            capa_state: Capa::default(),
            helpers: Vec::new(),
            configured: false,
            use_http_path: false,
            sanitize_prompt: true,
            protect_protocol: true,
        }
    }
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && b[i + 1].is_ascii_hexdigit()
            && b[i + 2].is_ascii_hexdigit()
        {
            let v = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("0"), 16)
                .unwrap_or(0);
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// git's strbuf_add_percentencode, for prompts and config matching.
fn percent(s: &str, slash: bool, host: bool) -> String {
    let mut out = String::new();
    for &c in s.as_bytes() {
        let unsafe_char = if host {
            !c.is_ascii_alphanumeric() && !b"-.:[]".contains(&c)
        } else {
            b" <>\"%{}|\\^`".contains(&c)
        };
        if c <= 0x1f || c >= 0x7f || (c == b'/' && slash) || unsafe_char {
            out.push_str(&format!("%{c:02X}"));
        } else {
            out.push(c as char);
        }
    }
    out
}

/// credential-store's strbuf_addstr_urlencode: lowercase hex.
fn urlencode(s: &str, reserved_ok: bool) -> String {
    let mut out = String::new();
    for &c in s.as_bytes() {
        let keep = c.is_ascii_alphanumeric()
            || b"-_.~".contains(&c)
            || (reserved_ok && b"!*'();:@&=+$,/?#[]".contains(&c));
        if keep {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02x}"));
        }
    }
    out
}

impl Cred {
    /// git's credential_from_url_1: `proto://[user[:pass]@]host[/path]`.
    fn from_url(url: &str, partial: bool, quiet: bool) -> Option<Cred> {
        let mut c = Cred::default();
        let proto_end = url.find("://");
        if !partial && proto_end.is_none_or(|p| p == 0) {
            if !quiet {
                eprintln!("warning: url has no scheme: {url}");
            }
            return None;
        }
        let rest = &url[proto_end.map_or(0, |p| p + 3)..];
        let slash = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let host_at = match rest.find('@') {
            Some(at) if at < slash => {
                let user = match rest.find(':') {
                    Some(colon) if colon < at => {
                        c.password = Some(url_decode(&rest[colon + 1..at]));
                        &rest[..colon]
                    }
                    _ => &rest[..at],
                };
                let user = url_decode(user);
                c.username_from_proto = !user.is_empty();
                c.username = Some(user);
                at + 1
            }
            _ => 0,
        };
        if let Some(p) = proto_end.filter(|&p| p > 0) {
            c.protocol = Some(url[..p].to_owned());
        }
        if !partial || slash > host_at {
            c.host = Some(url_decode(&rest[host_at..slash]));
        }
        let path = rest[slash..].trim_start_matches('/');
        if !path.is_empty() {
            let mut p = url_decode(path);
            while p.len() > 1 && p.ends_with('/') {
                p.pop();
            }
            c.path = Some(p);
        }
        for (what, v) in [
            ("username", &c.username),
            ("password", &c.password),
            ("protocol", &c.protocol),
            ("host", &c.host),
            ("path", &c.path),
        ] {
            if v.as_ref().is_some_and(|v| v.contains('\n')) {
                if !quiet {
                    eprintln!("warning: url contains a newline in its {what} component: {url}");
                }
                return None;
            }
        }
        Some(c)
    }

    /// git's credential_read; Err on a line without `=`.
    fn read(&mut self, input: &mut dyn BufRead, op: Op) -> Result<(), ()> {
        let mut raw = Vec::new();
        loop {
            raw.clear();
            if input.read_until(b'\n', &mut raw).unwrap_or(0) == 0 {
                return Ok(());
            }
            let text = String::from_utf8_lossy(&raw);
            let line = text.strip_suffix('\n').unwrap_or(&text);
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.is_empty() {
                return Ok(());
            }
            let Some((key, value)) = line.split_once('=') else {
                eprintln!("warning: invalid credential line: {line}");
                return Err(());
            };
            let v = || Some(value.to_owned());
            match key {
                "username" => {
                    self.username = v();
                    self.username_from_proto = true;
                }
                "password" => self.password = v(),
                "credential" => self.credential = v(),
                "authtype" => self.authtype = v(),
                "protocol" => self.protocol = v(),
                "host" => self.host = v(),
                "path" => self.path = v(),
                "ephemeral" => self.ephemeral = git_bool("ephemeral", value),
                "wwwauth[]" => self.wwwauth.push(value.to_owned()),
                "state[]" => self.state.push(value.to_owned()),
                "capability[]" => match value {
                    "authtype" => self.capa_authtype.set(op),
                    "state" => self.capa_state.set(op),
                    _ => {}
                },
                "continue" => self.multistage = git_bool("continue", value),
                "password_expiry_utc" => {
                    let digits: String = value
                        .trim_start()
                        .chars()
                        .take_while(char::is_ascii_digit)
                        .collect();
                    self.password_expiry_utc = digits.parse::<i64>().ok().filter(|&t| t != 0);
                }
                "oauth_refresh_token" => self.oauth_refresh_token = v(),
                "url" => match Cred::from_url(value, false, false) {
                    Some(c) => *self = c,
                    None => fatal(&format!("credential url cannot be parsed: {value}")),
                },
                "quit" => self.quit = git_bool("quit", value),
                _ => {}
            }
        }
    }

    fn write(&self, out: &mut dyn Write, op: Op) {
        let mut item = |key: &str, value: Option<&str>| {
            let Some(value) = value else { return };
            if value.contains('\n') {
                let _ = out.flush();
                fatal(&format!("credential value for {key} contains newline"));
            }
            if self.protect_protocol && value.contains('\r') {
                let _ = out.flush();
                fatal(&format!(
                    "credential value for {key} contains carriage return\n\
                     If this is intended, set `credential.protectProtocol=false`"
                ));
            }
            let _ = writeln!(out, "{key}={value}");
        };
        let authtype = self.capa_authtype.has(op);
        let state = self.capa_state.has(op);
        if authtype {
            item("capability[]", Some("authtype"));
        }
        if state {
            item("capability[]", Some("state"));
        }
        if authtype {
            item("authtype", self.authtype.as_deref());
            item("credential", self.credential.as_deref());
            if self.ephemeral {
                item("ephemeral", Some("1"));
            }
        }
        item("protocol", self.protocol.as_deref());
        item("host", self.host.as_deref());
        item("path", self.path.as_deref());
        item("username", self.username.as_deref());
        item("password", self.password.as_deref());
        item("oauth_refresh_token", self.oauth_refresh_token.as_deref());
        if let Some(t) = self.password_expiry_utc {
            item("password_expiry_utc", Some(&t.to_string()));
        }
        for w in &self.wwwauth {
            item("wwwauth[]", Some(w));
        }
        if state {
            if self.multistage {
                item("continue", Some("1"));
            }
            for s in &self.state_to_send {
                item("state[]", Some(s));
            }
        }
    }

    fn set_all_capabilities(&mut self, op: Op) {
        self.capa_authtype.set(op);
        self.capa_state.set(op);
    }

    fn next_state(&mut self) {
        self.state_to_send = std::mem::take(&mut self.state);
    }

    fn clear_secrets(&mut self) {
        self.password = None;
        self.credential = None;
    }

    fn complete(&self) -> bool {
        (self.username.is_some() && self.password.is_some()) || self.credential.is_some()
    }

    /// git's credential_match: every field `self` (the pattern) has, `have`
    /// has the same.
    fn matches(&self, have: &Cred, password: bool) -> bool {
        let check = |w: &Option<String>, h: &Option<String>| w.is_none() || w == h;
        check(&self.protocol, &have.protocol)
            && check(&self.host, &have.host)
            && check(&self.path, &have.path)
            && check(&self.username, &have.username)
            && (!password || check(&self.password, &have.password))
            && (!password || check(&self.credential, &have.credential))
    }

    /// git's credential_format (sanitized) or credential_describe.
    fn describe(&self, sanitize: bool) -> String {
        let Some(proto) = &self.protocol else {
            return String::new();
        };
        let mut s = format!("{proto}://");
        if let Some(u) = self.username.as_deref().filter(|u| !u.is_empty()) {
            s.push_str(&if sanitize {
                percent(u, true, false)
            } else {
                u.to_owned()
            });
            s.push('@');
        }
        if let Some(h) = &self.host {
            s.push_str(&if sanitize {
                percent(h, false, true)
            } else {
                h.clone()
            });
        }
        if let Some(p) = &self.path {
            s.push('/');
            s.push_str(&if sanitize {
                percent(p, false, false)
            } else {
                p.clone()
            });
        }
        s
    }

    /// Read `credential.*` (and `credential.<url>.*` for matching urls) in
    /// config order, as git's credential_apply_config.
    fn apply_config(&mut self) {
        if self.host.is_none() {
            fatal("refusing to work with credential missing host field");
        }
        if self.protocol.is_none() {
            fatal("refusing to work with credential missing protocol field");
        }
        if self.configured {
            return;
        }
        let url = Url::parse(&self.describe(true));
        for e in rgit_git::config_all() {
            let Some(rest) = e.name.strip_prefix("credential.") else {
                continue;
            };
            let (pattern, key) = match rest.rsplit_once('.') {
                Some((p, k)) => (Some(p), k),
                None => (None, rest),
            };
            if let Some(p) = pattern {
                let hit = match Url::parse(p) {
                    Some(pat) => url.as_ref().is_some_and(|u| u.matches(&pat)),
                    None => match Cred::from_url(p, true, false) {
                        Some(want) => want.matches(self, false),
                        None => {
                            eprintln!(
                                "warning: skipping credential lookup for key: credential.{p}"
                            );
                            false
                        }
                    },
                };
                if !hit {
                    continue;
                }
            }
            let var = format!("credential.{key}");
            let Some(value) = e.value else {
                eprintln!("error: missing value for '{var}'");
                fatal(&format!("bad config variable '{}'", e.name));
            };
            match key {
                "helper" if value.is_empty() => self.helpers.clear(),
                "helper" => self.helpers.push(value),
                "username" if !self.username_from_proto => self.username = Some(value),
                "usehttppath" => self.use_http_path = git_bool(&var, &value),
                "sanitizeprompt" => self.sanitize_prompt = git_bool(&var, &value),
                "protectprotocol" => self.protect_protocol = git_bool(&var, &value),
                _ => {}
            }
        }
        self.configured = true;
        if !self.use_http_path && matches!(self.protocol.as_deref(), Some("http" | "https")) {
            self.path = None;
        }
    }

    /// Run `helper <operation>` through the shell as git's credential_do:
    /// `!cmd` as is, an absolute path as is, else `git credential-<name>`
    /// (rgit's own for `store` and `cache`).
    fn run_helper(&mut self, helper: &str, operation: &str) {
        let cmd = if let Some(c) = helper.strip_prefix('!') {
            c.to_owned()
        } else if helper.starts_with('/') {
            helper.to_owned()
        } else {
            let exe = std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "rgit".to_owned());
            format!("'{}' credential-{helper}", exe.replace('\'', "'\\''"))
        };
        let cmd = format!("{cmd} {operation}");
        let get = operation == "get";
        let Ok(mut child) = Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .arg(&cmd)
            .stdin(Stdio::piped())
            .stdout(if get { Stdio::piped() } else { Stdio::null() })
            .spawn()
        else {
            eprintln!("error: cannot run {cmd}");
            return;
        };
        let mut buf = Vec::new();
        self.write(&mut buf, if get { Op::Helper } else { Op::Response });
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(&buf);
        }
        if let Some(out) = child.stdout.take() {
            let _ = self.read(&mut std::io::BufReader::new(out), Op::Response);
        }
        let _ = child.wait();
    }

    fn fill(&mut self) {
        if self.complete() {
            return;
        }
        self.next_state();
        self.multistage = false;
        self.apply_config();
        for helper in self.helpers.clone() {
            self.run_helper(&helper, "get");
            if self.password_expiry_utc.is_some_and(|t| t < now()) {
                self.clear_secrets();
                self.password_expiry_utc = None;
            }
            if self.complete() {
                self.wwwauth.clear();
                return;
            }
            if self.quit {
                fatal(&format!("credential helper '{helper}' told us to quit"));
            }
        }
        if !self.getpass()
            || (self.username.is_none() && self.password.is_none() && self.credential.is_none())
        {
            fatal("unable to get password from user");
        }
    }

    fn getpass(&mut self) -> bool {
        if let Some(v) = rgit_git::config_get("credential.interactive")
            && (v == "never"
                || matches!(
                    v.to_ascii_lowercase().as_str(),
                    "false" | "no" | "off" | "0"
                ))
        {
            return false;
        }
        if self.username.is_none() {
            self.username = Some(self.ask("Username", true));
        }
        if self.password.is_none() {
            self.password = Some(self.ask("Password", false));
        }
        true
    }

    fn ask(&self, what: &str, echo: bool) -> String {
        let desc = self.describe(self.sanitize_prompt);
        let prompt = if desc.is_empty() {
            format!("{what}: ")
        } else {
            format!("{what} for '{desc}': ")
        };
        git_prompt(&prompt, echo)
    }

    fn approve(&mut self) {
        if (self.username.is_none() || self.password.is_none()) && self.credential.is_none() {
            return;
        }
        if self.password_expiry_utc.is_some_and(|t| t < now()) {
            return;
        }
        self.next_state();
        self.apply_config();
        for helper in self.helpers.clone() {
            self.run_helper(&helper, "store");
        }
    }

    fn reject(&mut self) {
        self.next_state();
        self.apply_config();
        for helper in self.helpers.clone() {
            self.run_helper(&helper, "erase");
        }
    }
}

/// git's git_prompt: GIT_ASKPASS, core.askPass or SSH_ASKPASS, else the
/// terminal unless GIT_TERMINAL_PROMPT is off.
fn git_prompt(prompt: &str, echo: bool) -> String {
    let askpass = std::env::var("GIT_ASKPASS")
        .ok()
        .or_else(|| rgit_git::config_get("core.askpass"))
        .or_else(|| std::env::var("SSH_ASKPASS").ok())
        .filter(|a| !a.is_empty());
    if let Some(cmd) = askpass {
        match Command::new(&cmd)
            .arg(prompt)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .output()
        {
            Ok(out) if out.status.success() => {
                let text = String::from_utf8_lossy(&out.stdout);
                return text.split(['\r', '\n']).next().unwrap_or("").to_owned();
            }
            Ok(_) => eprintln!("error: unable to read askpass response from '{cmd}'"),
            Err(e) => eprintln!("error: cannot run {cmd}: {}", strerror(&e)),
        }
    }
    let on =
        std::env::var("GIT_TERMINAL_PROMPT").map_or(true, |v| git_bool("GIT_TERMINAL_PROMPT", &v));
    if !on {
        fatal(&format!("could not read {prompt}terminal prompts disabled"));
    }
    match tty_prompt(prompt, echo) {
        Ok(v) => v,
        Err(e) => fatal(&format!("could not read {prompt}{e}")),
    }
}

fn tty_prompt(prompt: &str, echo: bool) -> Result<String, String> {
    use std::os::fd::AsRawFd;
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|e| strerror(&e))?;
    let _ = tty.write_all(prompt.as_bytes());
    let fd = tty.as_raw_fd();
    // SAFETY: termios is plain data, filled by tcgetattr on an open fd.
    let mut old: libc::termios = unsafe { std::mem::zeroed() };
    let hide = !echo && unsafe { libc::tcgetattr(fd, &mut old) } == 0;
    if hide {
        let mut t = old;
        t.c_lflag &= !libc::ECHO;
        // SAFETY: fd is open and t came from tcgetattr.
        unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &t) };
    }
    let mut line = String::new();
    let read = std::io::BufReader::new(&tty).read_line(&mut line);
    if hide {
        // SAFETY: restores what tcgetattr read.
        unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &old) };
        let _ = (&tty).write_all(b"\n");
    }
    match read {
        Ok(0) => Err("end of file".to_owned()),
        Ok(_) => Ok(line.trim_end_matches(['\n', '\r']).to_owned()),
        Err(e) => Err(strerror(&e)),
    }
}

/// A url as git's url_normalize sees it, for `credential.<url>.*` matching.
struct Url {
    scheme: String,
    user: Option<String>,
    host: String,
    port: String,
    path: String,
}

impl Url {
    fn parse(url: &str) -> Option<Url> {
        let (scheme, rest) = url.split_once("://")?;
        let mut chars = scheme.chars();
        if !chars.next()?.is_ascii_alphabetic()
            || !chars.all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        {
            return None;
        }
        let scheme = scheme.to_ascii_lowercase();
        let (user, rest) = match rest.find(['@', '/', '?', '#']) {
            Some(i) if rest.as_bytes()[i] == b'@' => {
                let info = &rest[..i];
                let user = info.split_once(':').map_or(info, |(u, _)| u);
                (Some(url_decode(user)), &rest[i + 1..])
            }
            _ => (None, rest),
        };
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let hostport = &rest[..end];
        let bracket = hostport.rfind(']').unwrap_or(0);
        let (host, port) = match hostport.rfind(':').filter(|&c| c > bracket) {
            Some(c) => (&hostport[..c], &hostport[c + 1..]),
            None => (hostport, ""),
        };
        if !port.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let port = port.trim_start_matches('0');
        let port = match (scheme.as_str(), port) {
            ("http", "80") | ("https", "443") => "",
            (_, p) => p,
        };
        if host.is_empty() && scheme != "file" {
            return None;
        }
        let path = match &rest[end..] {
            "" => "/",
            p => p,
        };
        Some(Url {
            scheme,
            user,
            host: url_decode(host).to_ascii_lowercase(),
            port: port.to_owned(),
            path: path.to_owned(),
        })
    }

    /// git's match_urls against the config's `pat`: same scheme, port and
    /// user (when it names one), `*` host labels, a path prefix.
    fn matches(&self, pat: &Url) -> bool {
        if self.scheme != pat.scheme || self.port != pat.port {
            return false;
        }
        if pat.user.is_some() && self.user != pat.user {
            return false;
        }
        let (mut a, mut b) = (self.host.split('.'), pat.host.split('.'));
        loop {
            match (a.next(), b.next()) {
                (None, None) => break,
                (Some(x), Some(y)) if y == "*" || x == y => {}
                _ => return false,
            }
        }
        let prefix = pat.path.strip_suffix('/').unwrap_or(&pat.path);
        prefix.is_empty()
            || self
                .path
                .strip_prefix(prefix)
                .is_some_and(|r| r.is_empty() || r.starts_with('/'))
    }
}

fn read_stdin(c: &mut Cred, op: Op, what: &str) {
    if c.read(&mut std::io::stdin().lock(), op).is_err() {
        fatal(what);
    }
}

/// `git credential fill|approve|reject|capability`.
pub fn credential(op: &[String]) -> ! {
    const USAGE: &str = "usage: git credential (fill|approve|reject)";
    let [op] = op else { usage(USAGE) };
    let mut c = Cred::default();
    if op == "capability" {
        println!("version 0\ncapability authtype\ncapability state");
        exit(0);
    }
    if !matches!(op.as_str(), "fill" | "approve" | "reject") {
        usage(USAGE);
    }
    read_stdin(&mut c, Op::Initial, "unable to read credential from stdin");
    match op.as_str() {
        "fill" => {
            c.fill();
            c.next_state();
            let mut out = std::io::stdout().lock();
            c.write(&mut out, Op::Response);
            let _ = out.flush();
        }
        "approve" => {
            c.set_all_capabilities(Op::Helper);
            c.approve();
        }
        _ => {
            c.set_all_capabilities(Op::Helper);
            c.reject();
        }
    }
    exit(0)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// `git credential-store [--file <path>] get|store|erase`.
pub fn store(file: Option<String>, action: &[String]) -> ! {
    // SAFETY: single-threaded; the store file is private to the user.
    unsafe { libc::umask(0o077) };
    let [action] = action else {
        usage(
            "usage: git credential-store [<options>] <action>\n\n    \
             --[no-]file <path>    fetch and store credentials in <path>\n",
        )
    };
    let files: Vec<PathBuf> = match file {
        Some(f) => vec![PathBuf::from(f)],
        None => {
            let xdg = std::env::var_os("XDG_CONFIG_HOME")
                .filter(|x| !x.is_empty())
                .map(PathBuf::from)
                .or_else(|| home().map(|h| h.join(".config")))
                .map(|d| d.join("git/credentials"));
            home()
                .map(|h| h.join(".git-credentials"))
                .into_iter()
                .chain(xdg)
                .collect()
        }
    };
    if files.is_empty() {
        fatal("unable to set up default path; use --file");
    }
    let mut c = Cred::default();
    read_stdin(&mut c, Op::Helper, "unable to read credential");
    match action.as_str() {
        "get" => {
            for f in &files {
                let (hit, _) = parse_store(f, &c, false);
                if let Some(e) = hit {
                    let mut out = std::io::stdout().lock();
                    let _ = writeln!(
                        out,
                        "username={}\npassword={}",
                        e.username.unwrap_or_default(),
                        e.password.unwrap_or_default()
                    );
                    break;
                }
            }
        }
        "erase" => {
            if c.protocol.is_some() || c.host.is_some() || c.path.is_some() || c.username.is_some()
            {
                for f in files.iter().filter(|f| f.exists()) {
                    rewrite_store(f, &c, None, true);
                }
            }
        }
        "store" => {
            let (Some(proto), Some(user), Some(pass)) = (&c.protocol, &c.username, &c.password)
            else {
                exit(0)
            };
            if c.host.is_none() && c.path.is_none() {
                exit(0);
            }
            let mut line = format!(
                "{proto}://{}:{}@",
                urlencode(user, false),
                urlencode(pass, false)
            );
            if let Some(h) = &c.host {
                line.push_str(&urlencode(h, false));
            }
            if let Some(p) = &c.path {
                line.push('/');
                line.push_str(&urlencode(p, true));
            }
            let f = files.iter().find(|f| f.exists()).unwrap_or(&files[0]);
            rewrite_store(f, &c, Some(&line), false);
        }
        _ => {}
    }
    exit(0)
}

/// The first entry of `path` that `c` matches, and the lines that do not match.
fn parse_store(path: &std::path::Path, c: &Cred, password: bool) -> (Option<Cred>, Vec<String>) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            return (None, Vec::new());
        }
        Err(e) => fatal(&format!(
            "unable to open {}: {}",
            path.display(),
            strerror(&e)
        )),
    };
    let text = String::from_utf8_lossy(&data);
    let mut hit = None;
    let mut rest = Vec::new();
    for line in text.split_terminator('\n') {
        match Cred::from_url(line, false, true) {
            Some(e) if e.username.is_some() && e.password.is_some() && c.matches(&e, password) => {
                hit.get_or_insert(e);
            }
            _ => rest.push(line.to_owned()),
        }
    }
    (hit, rest)
}

/// Rewrite `path` under its lock: `first`, then every line `c` does not match.
fn rewrite_store(path: &std::path::Path, c: &Cred, first: Option<&str>, password: bool) {
    let timeout = rgit_git::config_get("credentialstore.locktimeoutms")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(1000);
    let lock = PathBuf::from(format!("{}.lock", path.display()));
    let start = std::time::Instant::now();
    let mut file = loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(f) => break f,
            Err(e)
                if e.kind() == std::io::ErrorKind::AlreadyExists
                    && start.elapsed().as_millis() < u128::from(timeout) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => fatal(&format!(
                "unable to get credential storage lock in {timeout} ms: {}",
                strerror(&e)
            )),
        }
    };
    let (_, rest) = parse_store(path, c, password);
    let mut text = String::new();
    for line in first.into_iter().map(str::to_owned).chain(rest) {
        text.push_str(&line);
        text.push('\n');
    }
    if file.write_all(text.as_bytes()).is_err() || std::fs::rename(&lock, path).is_err() {
        let _ = std::fs::remove_file(&lock);
        fatal("unable to write credential store");
    }
}

fn socket_path() -> Option<PathBuf> {
    let old = home().map(|h| h.join(".git-credential-cache"));
    if let Some(dir) = old.filter(|d| d.is_dir()) {
        return Some(dir.join("socket"));
    }
    std::env::var_os("XDG_CACHE_HOME")
        .filter(|x| !x.is_empty())
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".cache")))
        .map(|d| d.join("git/credential/socket"))
}

/// Connect to `socket`; a path too long for a socket address is reached from
/// its folder, as git does.
fn connect(socket: &std::path::Path) -> std::io::Result<std::os::unix::net::UnixStream> {
    use std::os::unix::net::UnixStream;
    let (Some(dir), Some(name)) = (socket.parent(), socket.file_name()) else {
        return UnixStream::connect(socket);
    };
    if socket.as_os_str().len() < 100 {
        return UnixStream::connect(socket);
    }
    let cwd = std::env::current_dir()?;
    std::env::set_current_dir(dir)?;
    let stream = UnixStream::connect(name);
    let _ = std::env::set_current_dir(cwd);
    stream
}

/// Send `request` to the daemon and copy its answer to stdout; Err when it
/// cannot be reached.
fn send_request(socket: &std::path::Path, request: &[u8]) -> std::io::Result<()> {
    let mut stream = connect(socket)?;
    if let Err(e) = stream.write_all(request) {
        fatal(&format!(
            "unable to write to cache daemon: {}",
            strerror(&e)
        ));
    }
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut answer = Vec::new();
    match stream.read_to_end(&mut answer) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
        Err(e) => fatal(&format!("read error from cache daemon: {}", strerror(&e))),
    }
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(&answer);
    let _ = out.flush();
    Ok(())
}

fn spawn_daemon(socket: &std::path::Path) {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("rgit"));
    let mut child = match Command::new(exe)
        .arg("credential-cache--daemon")
        .arg(socket)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => fatal(&format!("unable to start cache daemon: {}", strerror(&e))),
    };
    let mut buf = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_end(&mut buf);
    }
    if buf != b"ok\n" {
        fatal(&format!(
            "cache daemon did not start: {}",
            String::from_utf8_lossy(&buf)
        ));
    }
}

/// `git credential-cache [--timeout <n>] [--socket <path>] get|store|erase|exit`.
pub fn cache(timeout: i64, socket: Option<String>, action: &[String]) -> ! {
    let Some(action) = action.first() else {
        usage(
            "usage: git credential-cache [<options>] <action>\n\n    \
             --[no-]timeout <n>    number of seconds to cache credentials\n    \
             --[no-]socket <path>  path of cache-daemon socket\n",
        )
    };
    let Some(socket) = socket.map(PathBuf::from).or_else(socket_path) else {
        fatal("unable to find a suitable socket path; use --socket");
    };
    let (relay, spawn) = match action.as_str() {
        "exit" => (false, false),
        "get" | "erase" => (true, false),
        "store" => (true, true),
        "capability" => {
            println!("version 0\ncapability authtype");
            exit(0)
        }
        _ => exit(0),
    };
    let mut request = format!("action={action}\ntimeout={timeout}\n").into_bytes();
    if relay && let Err(e) = std::io::stdin().read_to_end(&mut request) {
        fatal(&format!("unable to relay credential: {}", strerror(&e)));
    }
    if let Err(e) = send_request(&socket, &request) {
        use std::io::ErrorKind::{ConnectionRefused, NotFound};
        if !matches!(e.kind(), NotFound | ConnectionRefused) {
            fatal(&format!(
                "unable to connect to cache daemon: {}",
                strerror(&e)
            ));
        }
        if spawn {
            spawn_daemon(&socket);
            if let Err(e) = send_request(&socket, &request) {
                fatal(&format!(
                    "unable to connect to cache daemon: {}",
                    strerror(&e)
                ));
            }
        }
    }
    exit(0)
}

struct Cache {
    entries: Vec<(Cred, i64)>,
    wait_until: i64,
}

impl Cache {
    /// Drop expired entries; the seconds until the next expiry, or 0 when
    /// the daemon should exit (nothing cached for 30 seconds).
    fn check(&mut self) -> i64 {
        let now = now();
        if self.wait_until == 0 {
            self.wait_until = now + 30;
        }
        let before = self.entries.len();
        self.entries.retain(|(_, t)| *t > now);
        if self.entries.len() != before {
            self.wait_until = now + 30;
        }
        match self.entries.iter().map(|(_, t)| *t).min() {
            Some(next) => next - now,
            None if self.wait_until <= now => 0,
            None => self.wait_until - now,
        }
    }

    fn remove(&mut self, c: &Cred, password: bool) {
        for (e, t) in &mut self.entries {
            if c.matches(e, password) {
                *t = 0;
            }
        }
    }

    fn serve(&mut self, stream: std::os::unix::net::UnixStream, socket: &std::path::Path) {
        let mut out = match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut input = std::io::BufReader::new(stream);
        let mut line = |prefix: &str| {
            let mut l = String::new();
            let _ = input.read_line(&mut l);
            let l = l.strip_suffix('\n').unwrap_or(&l).to_owned();
            match l.strip_prefix(prefix) {
                Some(v) => Some(v.to_owned()),
                None => {
                    eprintln!(
                        "error: client sent bogus {} line: {l}",
                        &prefix[..prefix.len() - 1]
                    );
                    None
                }
            }
        };
        let Some(action) = line("action=") else {
            return;
        };
        let Some(timeout) = line("timeout=") else {
            return;
        };
        let digits: String = timeout
            .trim_start()
            .chars()
            .enumerate()
            .take_while(|(i, c)| c.is_ascii_digit() || (*i == 0 && *c == '-'))
            .map(|(_, c)| c)
            .collect();
        let timeout: i64 = digits.parse().unwrap_or(0);
        let mut c = Cred::default();
        if c.read(&mut input, Op::Helper).is_err() {
            return;
        }
        match action.as_str() {
            "get" => {
                if let Some((e, _)) = self.entries.iter().find(|(e, _)| c.matches(e, false)) {
                    let authtype = c.capa_authtype.helper;
                    let mut text = String::from("capability[]=authtype\n");
                    for (key, value) in [
                        ("username", &e.username),
                        ("password", &e.password),
                        ("authtype", if authtype { &e.authtype } else { &None }),
                        ("credential", if authtype { &e.credential } else { &None }),
                        (
                            "password_expiry_utc",
                            &e.password_expiry_utc.map(|t| t.to_string()),
                        ),
                        ("oauth_refresh_token", &e.oauth_refresh_token),
                    ] {
                        if let Some(v) = value {
                            text.push_str(&format!("{key}={v}\n"));
                        }
                    }
                    let _ = out.write_all(text.as_bytes());
                }
            }
            "exit" => {
                let _ = std::fs::remove_file(socket);
                exit(0);
            }
            "erase" => self.remove(&c, true),
            "store" => {
                if timeout < 0 {
                    eprintln!("warning: cache client didn't specify a timeout");
                } else if (c.username.is_none() || c.password.is_none())
                    && c.authtype.is_none()
                    && c.credential.is_none()
                {
                    eprintln!("warning: cache client gave us a partial credential");
                } else if c.ephemeral {
                    eprintln!("warning: not storing ephemeral credential");
                } else {
                    self.remove(&c, false);
                    self.entries.push((c, now() + timeout));
                }
            }
            a => eprintln!("warning: cache client sent unknown action: {a}"),
        }
    }
}

/// `git credential-cache--daemon [--debug] <socket>`: hold credentials in
/// memory, answering on the socket, until they all expire.
pub fn cache_daemon(debug: bool, socket: Option<String>) -> ! {
    let Some(socket) = socket.map(PathBuf::from) else {
        usage("usage: git credential-cache--daemon [--debug] <socket-path>")
    };
    if !socket.is_absolute() {
        fatal("socket directory must be an absolute path");
    }
    let dir = socket.parent().unwrap_or(std::path::Path::new("/"));
    match std::fs::metadata(dir) {
        Ok(m) => {
            use std::os::unix::fs::PermissionsExt;
            if m.permissions().mode() & 0o077 != 0 {
                fatal(&format!(
                    "The permissions on your socket directory are too loose; other\n\
                     users may be able to read your cached credentials. Consider running:\n\
                     \n\
                     \tchmod 0700 {}",
                    dir.display()
                ));
            }
        }
        Err(_) => {
            use std::os::unix::fs::DirBuilderExt;
            if let Some(parent) = dir.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::DirBuilder::new().mode(0o700).create(dir) {
                fatal(&format!(
                    "unable to mkdir '{}': {}",
                    dir.display(),
                    strerror(&e)
                ));
            }
        }
    }
    let _ = std::env::set_current_dir(dir);
    if rgit_git::config_get("credentialcache.ignoresighup")
        .is_some_and(|v| git_bool("credentialcache.ignoresighup", &v))
    {
        // SAFETY: ignoring a signal has no preconditions.
        unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) };
    }
    let _ = std::fs::remove_file(&socket);
    let name = socket
        .file_name()
        .map_or(socket.as_path(), std::path::Path::new);
    let listener = match std::os::unix::net::UnixListener::bind(name) {
        Ok(l) => l,
        Err(e) => fatal(&format!(
            "unable to bind to '{}': {}",
            socket.display(),
            strerror(&e)
        )),
    };
    println!("ok");
    let _ = std::io::stdout().flush();
    // SAFETY: points stdout (and stderr) at /dev/null so the client that
    // spawned us sees EOF and our output goes nowhere, as git's daemon.
    unsafe {
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if null >= 0 {
            libc::dup2(null, 1);
            if !debug {
                libc::dup2(null, 2);
            }
            libc::close(null);
        }
    }
    let state = std::sync::Arc::new(std::sync::Mutex::new(Cache {
        entries: Vec::new(),
        wait_until: 0,
    }));
    let watch = state.clone();
    let path = socket.clone();
    // ponytail: a once-a-second expiry check instead of poll() with git's
    // exact wakeup; exit timing is off by under a second.
    std::thread::spawn(move || {
        loop {
            if watch.lock().map(|mut s| s.check()).unwrap_or(0) == 0 {
                let _ = std::fs::remove_file(&path);
                exit(0);
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                if let Ok(mut st) = state.lock() {
                    st.check();
                    st.serve(s, &socket);
                }
            }
            Err(e) => eprintln!("warning: accept failed: {}", strerror(&e)),
        }
    }
    exit(0)
}

/// Run one of the credential commands.
pub fn run(cmd: crate::cli::Command) -> i32 {
    use crate::cli::Command;
    match cmd {
        Command::Credential { action } => credential(&Vec::from_iter(action)),
        Command::CredentialStore { file, action } => store(file, &Vec::from_iter(action)),
        Command::CredentialCache {
            timeout,
            socket,
            action,
        } => cache(
            i64::try_from(timeout).unwrap_or(i64::MAX),
            socket,
            &Vec::from_iter(action),
        ),
        Command::CredentialCacheDaemon { debug, socket } => cache_daemon(debug, Some(socket)),
        _ => unreachable!("not a credential command"),
    }
}
