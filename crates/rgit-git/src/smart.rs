//! The client side of git's smart protocol over git://, ssh and http(s):
//! pkt-lines, the connection to a remote service, and protocol v2's `ls-refs`
//! and `fetch` (with v0's fetch for a server that answers in v0), for what
//! libgit2's transports cannot do: object filters, `unborn` HEADs and
//! `archive --remote`.

use std::io::{Read, Write};

use git2::Oid;

use crate::error::GitError;
use crate::model::OpProgress;

const AGENT: &str = concat!("git/rgit-", env!("CARGO_PKG_VERSION"));

/// Write one pkt-line.
pub(crate) fn pkt(w: &mut impl Write, data: &[u8]) -> std::io::Result<()> {
    w.write_all(format!("{:04x}", data.len() + 4).as_bytes())?;
    w.write_all(data)
}

/// One pkt-line read: data, or one of the special packets.
#[derive(Debug, PartialEq)]
pub(crate) enum Pkt {
    Data(Vec<u8>),
    Flush,
    Delim,
    End,
}

/// Read one pkt-line; the end of the stream reads as a flush.
pub(crate) fn read_line(r: &mut impl Read) -> Result<Pkt, GitError> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(Pkt::Flush),
        Err(e) => return Err(e.into()),
    }
    let len = std::str::from_utf8(&len)
        .ok()
        .and_then(|s| usize::from_str_radix(s, 16).ok())
        .ok_or_else(|| GitError::Other("protocol error: bad line length character".into()))?;
    match len {
        0 => return Ok(Pkt::Flush),
        1 => return Ok(Pkt::Delim),
        2 => return Ok(Pkt::End),
        3 => return Err(GitError::Other("protocol error: bad line length 3".into())),
        _ => {}
    }
    let mut data = vec![0u8; len - 4];
    r.read_exact(&mut data)?;
    Ok(Pkt::Data(data))
}

/// Read one pkt-line; None is a flush (or any special packet, or the end).
pub(crate) fn read_pkt(r: &mut impl Read) -> Result<Option<Vec<u8>>, GitError> {
    Ok(match read_line(r)? {
        Pkt::Data(d) => Some(d),
        _ => None,
    })
}

/// A data line as text, without its newline; a remote `ERR` is an error.
fn text(line: Vec<u8>) -> Result<String, GitError> {
    let s = String::from_utf8_lossy(&line).into_owned();
    let s = s.strip_suffix('\n').map(str::to_owned).unwrap_or(s);
    match s.strip_prefix("ERR ") {
        Some(m) => Err(GitError::Other(format!("remote error: {m}"))),
        None => Ok(s),
    }
}

/// `s` in single quotes for a POSIX shell, as git's sq_quote_buf writes it.
pub(crate) fn sq_quote(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        match c {
            '\'' | '!' => {
                out.push_str("'\\");
                out.push(c);
                out.push('\'');
            }
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// A two-way connection to a service on a remote, over git:// or ssh.
pub(crate) struct Stream {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    child: Option<std::process::Child>,
}

impl Stream {
    /// Close our end and wait for the ssh process; false if it failed.
    pub fn finish(self) -> Result<bool, GitError> {
        drop(self.writer);
        Ok(match self.child {
            Some(mut c) => c.wait()?.success(),
            None => true,
        })
    }
}

/// Connect to the service `exec` of the repository at `url` (git:// or ssh),
/// asking for protocol v2 when `v2`. `ssh` is core.sshCommand; GIT_SSH_COMMAND
/// and GIT_SSH win over it.
pub(crate) fn connect(
    url: &str,
    exec: &str,
    ssh: Option<&str>,
    v2: bool,
) -> Result<Stream, GitError> {
    // [user@]host[:port] and the path, with `/~user` read as `~user`.
    let (scheme, authority, path) = match url.split_once("://") {
        Some((scheme, rest)) => {
            let (a, p) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
            (scheme, a, p)
        }
        None => {
            let (a, p) = url.split_once(':').unwrap_or((url, ""));
            ("ssh", a, p)
        }
    };
    let path = path
        .strip_prefix('/')
        .filter(|p| p.starts_with('~'))
        .unwrap_or(path);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if url.contains("://") && !p.contains(']') => (h, Some(p)),
        _ => (authority, None),
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match scheme {
        "git" => {
            let port: u16 = port.and_then(|p| p.parse().ok()).unwrap_or(9418);
            let stream = std::net::TcpStream::connect((host, port))?;
            let mut w = stream.try_clone()?;
            let extra = if v2 { "\0version=2\0" } else { "" };
            pkt(
                &mut w,
                format!("{exec} {path}\0host={authority}\0{extra}").as_bytes(),
            )?;
            Ok(Stream {
                reader: Box::new(stream),
                writer: Box::new(w),
                child: None,
            })
        }
        "ssh" | "git+ssh" | "ssh+git" => {
            if host.starts_with('-') {
                return Err(GitError::Other(format!(
                    "strange hostname '{host}' blocked"
                )));
            }
            let command = format!("{exec} {}", sq_quote(path));
            let git_ssh = std::env::var("GIT_SSH").ok();
            let shell = std::env::var("GIT_SSH_COMMAND")
                .ok()
                .or_else(|| ssh.filter(|_| git_ssh.is_none()).map(str::to_owned));
            // Only OpenSSH is known to pass GIT_PROTOCOL on, as in git.
            let program = shell.as_deref().or(git_ssh.as_deref()).unwrap_or("ssh");
            let openssh = program
                .split_whitespace()
                .next()
                .and_then(|p| std::path::Path::new(p).file_name())
                .is_some_and(|n| n == "ssh");
            let mut ssh_args: Vec<String> = Vec::new();
            if v2 && openssh {
                ssh_args.extend(["-o".to_owned(), "SendEnv=GIT_PROTOCOL".to_owned()]);
            }
            if let Some(p) = port {
                ssh_args.extend(["-p".to_owned(), p.to_owned()]);
            }
            ssh_args.extend([host.to_owned(), command]);
            let mut cmd = match (shell, git_ssh) {
                (Some(sh), _) => {
                    let mut c = std::process::Command::new("sh");
                    c.args(["-c", &format!("{sh} \"$@\""), &sh]);
                    c
                }
                (None, Some(prog)) => std::process::Command::new(prog),
                (None, None) => std::process::Command::new("ssh"),
            };
            if v2 {
                cmd.env("GIT_PROTOCOL", "version=2");
            }
            let mut c = cmd
                .args(&ssh_args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()?;
            let reader = c.stdout.take().expect("piped stdout");
            let writer = c.stdin.take().expect("piped stdin");
            Ok(Stream {
                reader: Box::new(reader),
                writer: Box::new(writer),
                child: Some(c),
            })
        }
        _ => Err(GitError::Other(
            "operation not supported by protocol".into(),
        )),
    }
}

/// A ref a remote advertises; an unborn HEAD has a zero id.
#[derive(Debug, Clone)]
pub(crate) struct RemoteRef {
    pub name: String,
    pub id: Oid,
    pub peeled: Option<Oid>,
    pub symref: Option<String>,
}

/// What to ask a fetch for.
#[derive(Debug, Default)]
pub(crate) struct FetchRequest {
    pub wants: Vec<Oid>,
    pub haves: Vec<Oid>,
    /// The commits the repository is shallow at now.
    pub shallow: Vec<Oid>,
    pub depth: i32,
    pub deepen_since: Option<i64>,
    pub deepen_relative: bool,
    pub filter: Option<String>,
    pub include_tag: bool,
}

/// How the fetch moved the shallow boundary.
#[derive(Debug, Default)]
pub(crate) struct Shallow {
    pub shallow: Vec<Oid>,
    pub unshallow: Vec<Oid>,
}

enum Wire {
    Stream(Stream),
    Http {
        agent: ureq::Agent,
        base: String,
        auth: Option<String>,
    },
}

/// A connection to a remote's upload-pack.
pub(crate) struct Session {
    wire: Wire,
    /// v2's capabilities, or v0's advertised refs and capabilities.
    caps: Vec<String>,
    v0: Option<Vec<RemoteRef>>,
}

impl Session {
    /// Connect to the upload-pack of `url` (git://, ssh or http(s)) and read
    /// what it advertises. `config` finds http credentials and core.sshCommand.
    pub fn open(url: &str, config: Option<&git2::Config>) -> Result<Self, GitError> {
        if url.starts_with("http://") || url.starts_with("https://") {
            return Self::open_http(url, config);
        }
        let ssh = config.and_then(|c| c.get_string("core.sshCommand").ok());
        let mut stream = connect(url, "git-upload-pack", ssh.as_deref(), true)?;
        let first = match read_line(&mut stream.reader)? {
            Pkt::Data(d) => text(d)?,
            _ => {
                return Err(GitError::Other(
                    "the remote end hung up unexpectedly".into(),
                ));
            }
        };
        let mut session = Session {
            wire: Wire::Stream(stream),
            caps: Vec::new(),
            v0: None,
        };
        session.advertisement(first)?;
        Ok(session)
    }

    fn open_http(url: &str, config: Option<&git2::Config>) -> Result<Self, GitError> {
        let base = url.trim_end_matches('/').to_owned();
        let agent: ureq::Agent = ureq::Agent::config_builder().user_agent(AGENT).build().into();
        let info = format!("{base}/info/refs?service=git-upload-pack");
        let mut auth: Option<String> = None;
        let resp = loop {
            let mut req = agent.get(&info).header("Git-Protocol", "version=2");
            if let Some(a) = &auth {
                req = req.header("Authorization", a);
            }
            match req.call() {
                Err(ureq::Error::StatusCode(401)) if auth.is_none() => {
                    auth = Some(http_auth(url, config).ok_or_else(|| {
                        GitError::Other(format!("Authentication failed for '{url}'"))
                    })?);
                }
                r => break r.map_err(http_error)?,
            }
        };
        let mut body = resp.into_body().into_reader();
        let mut first = match read_line(&mut body)? {
            Pkt::Data(d) => text(d)?,
            _ => return Err(GitError::Other("invalid server response".into())),
        };
        if first.starts_with("# service=") {
            read_line(&mut body)?;
            first = match read_line(&mut body)? {
                Pkt::Data(d) => text(d)?,
                _ => return Err(GitError::Other("invalid server response".into())),
            };
        }
        if first != "version 2" {
            return Err(GitError::Other(format!(
                "{url}: the server does not speak protocol v2"
            )));
        }
        let mut session = Session {
            wire: Wire::Http { agent, base, auth },
            caps: Vec::new(),
            v0: None,
        };
        while let Some(line) = read_pkt(&mut body)? {
            session.caps.push(text(line)?);
        }
        Ok(session)
    }

    /// Take in what the server said first: v2's capabilities, or v0's refs.
    fn advertisement(&mut self, first: String) -> Result<(), GitError> {
        let Wire::Stream(s) = &mut self.wire else {
            return Ok(());
        };
        if first == "version 2" {
            while let Some(line) = read_pkt(&mut s.reader)? {
                self.caps.push(text(line)?);
            }
            return Ok(());
        }
        let mut refs: Vec<RemoteRef> = Vec::new();
        let mut line = Some(first);
        while let Some(l) = line {
            let (l, caps) = l.split_once('\0').unwrap_or((&l, ""));
            if !caps.is_empty() {
                self.caps = caps.split(' ').map(str::to_owned).collect();
            }
            if let Some((id, name)) = l.split_once(' ')
                && let Ok(id) = Oid::from_str(id)
            {
                match name.strip_suffix("^{}") {
                    Some(base) => {
                        if let Some(r) = refs.iter_mut().rev().find(|r| r.name == base) {
                            r.peeled = Some(id);
                        }
                    }
                    None if name != "capabilities^{}" => refs.push(RemoteRef {
                        name: name.to_owned(),
                        id,
                        peeled: None,
                        symref: None,
                    }),
                    None => {}
                }
            }
            line = read_pkt(&mut s.reader)?.map(text).transpose()?;
        }
        for cap in &self.caps {
            if let Some((from, to)) = cap.strip_prefix("symref=").and_then(|c| c.split_once(':'))
                && let Some(r) = refs.iter_mut().find(|r| r.name == from)
            {
                r.symref = Some(to.to_owned());
            }
        }
        self.v0 = Some(refs);
        Ok(())
    }

    /// Whether the server's `command` (v2) takes `feature`, or (v0) the
    /// server has the capability `feature`.
    fn supports(&self, command: &str, feature: &str) -> bool {
        if self.v0.is_some() {
            return self.caps.iter().any(|c| c == feature);
        }
        self.caps.iter().any(|c| {
            c.strip_prefix(command)
                .and_then(|r| r.strip_prefix('='))
                .is_some_and(|v| v.split(' ').any(|f| f == feature))
        })
    }

    /// Whether the server can filter what it sends.
    pub fn filters(&self) -> bool {
        self.supports("fetch", "filter")
    }

    /// Send a v2 command and return the reader of its response.
    fn command(&mut self, name: &str, args: &[String]) -> Result<Box<dyn Read + '_>, GitError> {
        let mut body = Vec::new();
        pkt(&mut body, format!("command={name}\n").as_bytes())?;
        pkt(&mut body, format!("agent={AGENT}\n").as_bytes())?;
        if self.caps.iter().any(|c| c.starts_with("object-format=")) {
            pkt(&mut body, b"object-format=sha1\n")?;
        }
        body.extend(b"0001");
        for a in args {
            pkt(&mut body, format!("{a}\n").as_bytes())?;
        }
        body.extend(b"0000");
        match &mut self.wire {
            Wire::Stream(s) => {
                s.writer.write_all(&body)?;
                s.writer.flush()?;
                Ok(Box::new(&mut s.reader))
            }
            Wire::Http { agent, base, auth } => {
                let mut req = agent
                    .post(&format!("{base}/git-upload-pack"))
                    .header("Git-Protocol", "version=2")
                    .header("Content-Type", "application/x-git-upload-pack-request")
                    .header("Accept", "application/x-git-upload-pack-result");
                if let Some(a) = auth {
                    req = req.header("Authorization", a.as_str());
                }
                Ok(Box::new(
                    req.send(&body).map_err(http_error)?.into_body().into_reader(),
                ))
            }
        }
    }

    /// The remote's refs under `prefixes` (all with none), HEAD's symref
    /// target included, and an unborn HEAD as a zero id where the server
    /// says what it points at.
    pub fn ls_refs(&mut self, prefixes: &[String]) -> Result<Vec<RemoteRef>, GitError> {
        if let Some(refs) = &self.v0 {
            return Ok(refs.clone());
        }
        let mut args = vec!["peel".to_owned(), "symrefs".to_owned()];
        if self.supports("ls-refs", "unborn") {
            args.push("unborn".to_owned());
        }
        args.extend(prefixes.iter().map(|p| format!("ref-prefix {p}")));
        let mut r = self.command("ls-refs", &args)?;
        let mut refs = Vec::new();
        while let Some(line) = read_pkt(&mut r)? {
            let line = text(line)?;
            let mut words = line.split(' ');
            let (Some(id), Some(name)) = (words.next(), words.next()) else {
                continue;
            };
            let id = match id {
                "unborn" => Oid::ZERO_SHA1,
                id => Oid::from_str(id)
                    .map_err(|_| GitError::Other(format!("invalid ls-refs response: {line}")))?,
            };
            let mut r = RemoteRef {
                name: name.to_owned(),
                id,
                peeled: None,
                symref: None,
            };
            for attr in words {
                if let Some(t) = attr.strip_prefix("symref-target:") {
                    r.symref = Some(t.to_owned());
                } else if let Some(p) = attr.strip_prefix("peeled:") {
                    r.peeled = Oid::from_str(p).ok();
                }
            }
            refs.push(r);
        }
        Ok(refs)
    }

    /// Fetch what `req` asks for, writing the pack to `sink`, and return how
    /// the shallow boundary moved. The server's messages go to `report`.
    pub fn fetch(
        &mut self,
        req: &FetchRequest,
        sink: &mut dyn Write,
        report: &dyn Fn(OpProgress),
    ) -> Result<Shallow, GitError> {
        if self.v0.is_some() {
            return self.fetch_v0(req, sink, report);
        }
        let mut args = vec!["ofs-delta".to_owned(), "no-progress".to_owned()];
        if req.include_tag {
            args.push("include-tag".to_owned());
        }
        args.extend(req.shallow.iter().map(|s| format!("shallow {s}")));
        if req.depth > 0 {
            args.push(format!("deepen {}", req.depth));
        }
        if let Some(t) = req.deepen_since {
            args.push(format!("deepen-since {t}"));
        }
        if req.deepen_relative {
            args.push("deepen-relative".to_owned());
        }
        if let Some(f) = &req.filter {
            args.push(format!("filter {f}"));
        }
        args.extend(req.wants.iter().map(|w| format!("want {w}")));
        args.extend(req.haves.iter().map(|h| format!("have {h}")));
        args.push("done".to_owned());
        let mut r = self.command("fetch", &args)?;
        let mut shallow = Shallow::default();
        loop {
            let section = match read_line(&mut r)? {
                Pkt::Data(d) => text(d)?,
                Pkt::Delim => continue,
                _ => return Err(GitError::Other("expected 'packfile'".into())),
            };
            if section == "packfile" {
                sideband(&mut r, sink, report)?;
                return Ok(shallow);
            }
            loop {
                let line = match read_line(&mut r)? {
                    Pkt::Data(d) => text(d)?,
                    Pkt::Delim => break,
                    _ => return Ok(shallow),
                };
                shallow_line(&line, &mut shallow);
            }
        }
    }

    fn fetch_v0(
        &mut self,
        req: &FetchRequest,
        sink: &mut dyn Write,
        report: &dyn Fn(OpProgress),
    ) -> Result<Shallow, GitError> {
        let mut caps = vec!["side-band-64k", "ofs-delta", "no-progress"];
        if req.include_tag {
            caps.push("include-tag");
        }
        let deepen = req.depth > 0 || req.deepen_since.is_some();
        if deepen || !req.shallow.is_empty() {
            caps.push("shallow");
        }
        if req.deepen_relative {
            caps.push("deepen-relative");
        }
        if req.filter.is_some() {
            caps.push("filter");
        }
        caps.retain(|c| self.caps.iter().any(|s| s == c));
        let caps = format!("{} agent={AGENT}", caps.join(" "));
        let Wire::Stream(s) = &mut self.wire else {
            unreachable!("v0 is only spoken over a stream");
        };
        let w = &mut s.writer;
        for (i, want) in req.wants.iter().enumerate() {
            match i {
                0 => pkt(w, format!("want {want} {caps}\n").as_bytes())?,
                _ => pkt(w, format!("want {want}\n").as_bytes())?,
            }
        }
        for id in &req.shallow {
            pkt(w, format!("shallow {id}\n").as_bytes())?;
        }
        if req.depth > 0 {
            pkt(w, format!("deepen {}\n", req.depth).as_bytes())?;
        }
        if let Some(t) = req.deepen_since {
            pkt(w, format!("deepen-since {t}\n").as_bytes())?;
        }
        if let Some(f) = &req.filter {
            pkt(w, format!("filter {f}\n").as_bytes())?;
        }
        w.write_all(b"0000")?;
        w.flush()?;
        let mut shallow = Shallow::default();
        if deepen || !req.shallow.is_empty() {
            while let Some(line) = read_pkt(&mut s.reader)? {
                shallow_line(&text(line)?, &mut shallow);
            }
        }
        for have in &req.haves {
            pkt(w, format!("have {have}\n").as_bytes())?;
        }
        pkt(w, b"done\n")?;
        w.flush()?;
        // Without multi_ack the server answers once: an ACK or a NAK.
        loop {
            let Some(line) = read_pkt(&mut s.reader)? else {
                return Err(GitError::Other(
                    "expected ACK/NAK, got a flush packet".into(),
                ));
            };
            let line = text(line)?;
            if line == "NAK" || line.starts_with("ACK ") {
                break;
            }
        }
        sideband(&mut s.reader, sink, report)?;
        Ok(shallow)
    }

    /// Close the connection.
    pub fn finish(self) -> Result<(), GitError> {
        if let Wire::Stream(mut s) = self.wire {
            if self.v0.is_none() {
                let _ = s.writer.write_all(b"0000");
            }
            s.finish()?;
        }
        Ok(())
    }
}

fn shallow_line(line: &str, shallow: &mut Shallow) {
    if let Some(id) = line.strip_prefix("shallow ") {
        shallow.shallow.extend(Oid::from_str(id).ok());
    } else if let Some(id) = line.strip_prefix("unshallow ") {
        shallow.unshallow.extend(Oid::from_str(id).ok());
    }
}

/// Copy a side-band stream's band 1 to `sink` up to its flush, reporting
/// band 2's messages and failing on band 3.
fn sideband(
    r: &mut dyn Read,
    sink: &mut dyn Write,
    report: &dyn Fn(OpProgress),
) -> Result<(), GitError> {
    let mut r = r;
    while let Some(p) = read_pkt(&mut r)? {
        let Some((&band, data)) = p.split_first() else {
            continue;
        };
        match band {
            1 => sink.write_all(data)?,
            2 => {
                for l in String::from_utf8_lossy(data).split(['\n', '\r']) {
                    if !l.trim().is_empty() {
                        report(OpProgress::Line(format!("remote: {l}")));
                    }
                }
            }
            3 => {
                return Err(GitError::Other(format!(
                    "remote error: {}",
                    String::from_utf8_lossy(data).trim_end()
                )));
            }
            b => return Err(GitError::Other(format!("protocol error: bad band #{b}"))),
        }
    }
    Ok(())
}

/// A Basic Authorization header for `url`: its own user and password, else
/// what git's credential helpers give.
fn http_auth(url: &str, config: Option<&git2::Config>) -> Option<String> {
    let (user, pass) = url_credentials(url).or_else(|| {
        let default;
        let cfg = match config {
            Some(c) => c,
            None => {
                default = git2::Config::open_default().ok()?;
                &default
            }
        };
        git2::CredentialHelper::new(url).config(cfg).execute()
    })?;
    use base64::Engine;
    let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
    Some(format!("Basic {token}"))
}

/// The user and password written into `url`, if both are.
fn url_credentials(url: &str) -> Option<(String, String)> {
    let rest = url.split_once("://")?.1;
    let auth = rest.split('/').next()?.rsplit_once('@')?.0;
    let (u, p) = auth.split_once(':')?;
    Some((u.to_owned(), p.to_owned()))
}

fn http_error(e: ureq::Error) -> GitError {
    match e {
        ureq::Error::StatusCode(code) => {
            GitError::Other(format!("the requested URL returned error: {code}"))
        }
        e => GitError::Other(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkt_lines() {
        let mut buf = Vec::new();
        pkt(&mut buf, b"a\n").unwrap();
        buf.extend(b"000100000002");
        let mut r = &buf[..];
        assert_eq!(read_line(&mut r).unwrap(), Pkt::Data(b"a\n".to_vec()));
        assert_eq!(read_line(&mut r).unwrap(), Pkt::Delim);
        assert_eq!(read_line(&mut r).unwrap(), Pkt::Flush);
        assert_eq!(read_line(&mut r).unwrap(), Pkt::End);
    }
}
