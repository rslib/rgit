//! An in-process, ssh-config-aware SSH transport for libgit2.
//!
//! libgit2's built-in ssh (libssh2) ignores `~/.ssh/config` entirely, so hosts
//! that rely on `ProxyJump`/`ProxyCommand`, a non-default `HostName`/`Port`, or
//! a specific `IdentityFile` fail. This registers a custom smart-subtransport
//! for `ssh://` that speaks SSH with the pure-Rust `russh`, honoring ssh config
//! (parsed here) - including a bastion hop done in-process, either by reusing a
//! live OpenSSH `ControlMaster` socket or opening a fresh `direct-tcpip` channel
//! while libgit2 still performs the whole git protocol over the stream. URL
//!   rewrites (`url.<base>.insteadOf`/`pushInsteadOf`) are applied by libgit2
//!   before the transport is invoked, so they work without anything extra here.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};

use git2::transport::{Service, SmartSubtransport, SmartSubtransportStream, Transport};
use russh::client::{self, Handle};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::runtime::Runtime;
use tokio_util::io::SyncIoBridge;

/// A prompt callback that returns a password for `prompt`, or `None` to give up.
/// The frontend (CLI/TUI) installs one; without it, password auth is skipped.
type PasswordFn = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;
static PASSWORD_PROVIDER: OnceLock<PasswordFn> = OnceLock::new();

/// Install the password prompt used when key/agent auth fails and the server
/// offers password auth. Call once at startup.
pub fn set_password_provider(f: impl Fn(&str) -> Option<String> + Send + Sync + 'static) {
    let _ = PASSWORD_PROVIDER.set(Box::new(f));
}

/// A dedicated runtime that hosts the ssh sessions' background tasks. libgit2
/// calls the transport from a blocking thread, so the stream's reads/writes
/// block on this runtime.
fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("ssh transport runtime")
    })
}

/// Register the ssh-config-aware transport for `ssh://`, replacing libgit2's
/// built-in. Idempotent; safe to call more than once.
pub fn register() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| unsafe {
        let _ = git2::transport::register("ssh", |remote| Transport::smart(remote, false, RgitSsh));
    });
}

struct RgitSsh;

impl SmartSubtransport for RgitSsh {
    fn action(
        &self,
        url: &str,
        service: Service,
    ) -> Result<Box<dyn SmartSubtransportStream>, git2::Error> {
        let command = match service {
            Service::UploadPackLs | Service::UploadPack => "git-upload-pack",
            Service::ReceivePackLs | Service::ReceivePack => "git-receive-pack",
        };
        let stream = runtime()
            .block_on(connect(url, command))
            .map_err(|e| git2::Error::from_str(&format!("ssh: {e}")))?;
        Ok(Box::new(stream))
    }

    fn close(&self) -> Result<(), git2::Error> {
        Ok(())
    }
}

/// The libgit2-facing stream: a synchronous bridge over the ssh exec channel,
/// keeping the session handles (and any bastion handle) alive for its lifetime.
/// The russh parts are `Option`s dropped inside the runtime context (their `Drop`
/// impls `tokio::spawn` a close message, which panics off-runtime).
struct SshStream {
    inner: Option<SyncIoBridge<russh::ChannelStream<russh::client::Msg>>>,
    session: Option<Handle<HostKeyVerifier>>,
    jump: Option<Handle<HostKeyVerifier>>,
    // The ControlMaster connection, kept open so its forward stays alive. Plain
    // socket - drops fine off-runtime, unlike the russh parts.
    #[cfg(unix)]
    _master: Option<std::os::unix::net::UnixStream>,
}

impl Read for SshStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.as_mut().expect("ssh stream open").read(buf)
    }
}
impl Write for SshStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.as_mut().expect("ssh stream open").write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.as_mut().expect("ssh stream open").flush()
    }
}

impl Drop for SshStream {
    fn drop(&mut self) {
        // Drop the channel/handles while a runtime is entered, so their Drop
        // impls (which tokio::spawn a Close) have a reactor to run on.
        let _enter = runtime().enter();
        self.inner.take();
        self.session.take();
        self.jump.take();
    }
}
// `SmartSubtransportStream` is blanket-implemented for any Read + Write + Send.

/// A parsed `ssh://[user@]host[:port]/path` target.
struct Target {
    user: Option<String>,
    host: String,
    port: Option<u16>,
    path: String,
}

fn parse_url(url: &str) -> Result<Target, String> {
    let split_user = |s: &str| match s.split_once('@') {
        Some((u, rest)) => (Some(u.to_owned()), rest.to_owned()),
        None => (None, s.to_owned()),
    };
    if let Some(rest) = url.strip_prefix("ssh://") {
        // ssh://[user@]host[:port]/path
        let (authority, path) = match rest.split_once('/') {
            Some((a, p)) => (a, format!("/{p}")),
            None => (rest, String::new()),
        };
        let (user, host_port) = split_user(authority);
        let (host, port) = match host_port.rsplit_once(':') {
            Some((h, p)) => (h.to_owned(), p.parse().ok()),
            None => (host_port, None),
        };
        Ok(Target {
            user,
            host,
            port,
            path,
        })
    } else {
        // scp-like: [user@]host:path (relative path, no port) - e.g. GitHub.
        let (user, host_path) = split_user(url);
        let (host, path) = host_path
            .split_once(':')
            .ok_or_else(|| format!("not an ssh url: {url}"))?;
        Ok(Target {
            user,
            host: host.to_owned(),
            port: None,
            path: path.to_owned(),
        })
    }
}

/// Resolved ssh-config directives for one host.
#[derive(Default)]
struct SshHostConfig {
    host_name: Option<String>,
    port: Option<u16>,
    user: Option<String>,
    identity_files: Vec<PathBuf>,
    proxy_command: Option<String>,
    proxy_jump: Option<String>,
    control_path: Option<String>,
}

/// Parse `~/.ssh/config` and resolve the directives for `host`, the way ssh
/// does: a global block (before the first `Host`) plus every matching `Host`
/// block, first value wins for scalars, IdentityFiles accumulate. Wildcards
/// (`*`, `?`) and `!` negation are honored. `Match` blocks are skipped (their
/// directives are ignored). A missing file just yields defaults.
fn resolve_ssh_config(host: &str) -> SshHostConfig {
    let mut cfg = SshHostConfig::default();
    let Some(path) = home::home_dir().map(|h| h.join(".ssh").join("config")) else {
        return cfg;
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return cfg;
    };
    // The global block (before any Host/Match) applies to every host.
    let mut active = true;
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = match line.split_once(|c: char| c.is_whitespace() || c == '=') {
            Some((k, v)) => (
                k.to_ascii_lowercase(),
                v.trim_start_matches(['=', ' ', '\t']).trim(),
            ),
            None => continue,
        };
        if key == "host" {
            active = host_line_matches(host, value);
            continue;
        }
        if key == "match" {
            active = false; // Match conditions are not evaluated
            continue;
        }
        if !active {
            continue;
        }
        match key.as_str() {
            "hostname" => {
                cfg.host_name.get_or_insert_with(|| value.to_owned());
            }
            "user" => {
                cfg.user.get_or_insert_with(|| value.to_owned());
            }
            "port" => {
                if let Ok(p) = value.parse() {
                    cfg.port.get_or_insert(p);
                }
            }
            "identityfile" => cfg.identity_files.push(expand_home(value)),
            "proxycommand" => {
                cfg.proxy_command.get_or_insert_with(|| value.to_owned());
            }
            "proxyjump" => {
                cfg.proxy_jump.get_or_insert_with(|| value.to_owned());
            }
            "controlpath" => {
                cfg.control_path.get_or_insert_with(|| value.to_owned());
            }
            _ => {}
        }
    }
    cfg
}

/// Whether a `Host` line's patterns match `host` (case-insensitive, `*`/`?`
/// globs, `!` negation wins).
fn host_line_matches(host: &str, patterns: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let mut matched = false;
    for pat in patterns.split_whitespace() {
        if let Some(neg) = pat.strip_prefix('!') {
            if glob_match(&neg.to_ascii_lowercase(), &host) {
                return false;
            }
        } else if glob_match(&pat.to_ascii_lowercase(), &host) {
            matched = true;
        }
    }
    matched
}

/// A minimal shell-style glob for ssh host patterns: `*` any run, `?` one char.
fn glob_match(pattern: &str, text: &str) -> bool {
    fn m(p: &[u8], t: &[u8]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some(b'*'), _) => m(&p[1..], t) || (!t.is_empty() && m(p, &t[1..])),
            (Some(b'?'), Some(_)) => m(&p[1..], &t[1..]),
            (Some(a), Some(b)) if a == b => m(&p[1..], &t[1..]),
            _ => false,
        }
    }
    m(pattern.as_bytes(), text.as_bytes())
}

fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = home::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

/// The bastion to jump through for a host, from `ProxyJump` or a
/// `ProxyCommand ssh <jump> -W %h:%p` (the form ProxyJump desugars to).
fn jump_host(cfg: &SshHostConfig) -> Option<String> {
    if let Some(pj) = &cfg.proxy_jump {
        // ProxyJump may be "user@host:port"; keep just the host token.
        let last = pj.split(',').next_back().unwrap_or(pj).trim();
        let hostpart = last.rsplit('@').next().unwrap_or(last);
        let host = hostpart.split(':').next().unwrap_or(hostpart);
        if !host.is_empty() {
            return Some(host.to_owned());
        }
    }
    if let Some(pc) = &cfg.proxy_command {
        let toks: Vec<&str> = pc.split_whitespace().collect();
        if toks.first() == Some(&"ssh") && toks.contains(&"-W") {
            // First non-flag token after `ssh` is the jump host.
            if let Some(h) = toks.iter().skip(1).find(|t| !t.starts_with('-')) {
                return Some((*h).to_owned());
            }
        }
    }
    None
}

/// Establish the (possibly jumped) session, run the git service, and return a
/// synchronous stream for libgit2.
async fn connect(url: &str, command: &str) -> Result<SshStream, String> {
    let target = parse_url(url)?;
    let cfg = resolve_ssh_config(&target.host);

    let host_name = cfg.host_name.clone().unwrap_or_else(|| target.host.clone());
    let port = target.port.or(cfg.port).unwrap_or(22);
    let user = target
        .user
        .clone()
        .or(cfg.user.clone())
        .unwrap_or_else(|| "git".to_owned());
    let identities = cfg.identity_files.clone();

    let client_cfg = Arc::new(client_config());

    // The bastion hop, if any. Prefer reusing an existing ControlMaster (no
    // re-auth, matches the user's CLI); else open a fresh jump connection and a
    // direct-tcpip channel through it.
    let mut jump_handle = None;
    let mut master_keepalive = None;
    let mut session = if let Some(jump) = jump_host(&cfg) {
        let jcfg = resolve_ssh_config(&jump);
        let jhost = jcfg.host_name.clone().unwrap_or_else(|| jump.clone());
        let jport = jcfg.port.unwrap_or(22);
        let juser = jcfg.user.clone().unwrap_or_else(whoami);
        let jident = jcfg.identity_files.clone();

        // 1. Reuse a live ControlMaster socket for the jump, if any.
        let tunnel = match resolve_control_path(&jcfg, &jhost, jport, &juser)
            .filter(|p| p.exists())
            .and_then(|p| controlmaster_forward(&p, &host_name, port))
        {
            Some((ours, master)) => {
                master_keepalive = Some(master);
                // tokio requires the std socket to be non-blocking.
                ours.set_nonblocking(true)
                    .map_err(|e| format!("controlmaster socket: {e}"))?;
                let ustream = tokio::net::UnixStream::from_std(ours)
                    .map_err(|e| format!("controlmaster socket: {e}"))?;
                Tunnel::Unix(ustream)
            }
            // 2. Fresh jump connection + direct-tcpip channel.
            None => {
                let mut jump = client::connect(
                    client_cfg.clone(),
                    (jhost.as_str(), jport),
                    HostKeyVerifier::new(&jhost, jport),
                )
                .await
                .map_err(|e| format!("connect jump {jhost}:{jport}: {e}"))?;
                authenticate(&mut jump, &juser, &jident).await?;
                let ch = jump
                    .channel_open_direct_tcpip(host_name.clone(), port as u32, "127.0.0.1", 0)
                    .await
                    .map_err(|e| format!("open tunnel to {host_name}:{port}: {e}"))?
                    .into_stream();
                jump_handle = Some(jump);
                Tunnel::Channel(ch)
            }
        };

        client::connect_stream(
            client_cfg.clone(),
            tunnel,
            HostKeyVerifier::new(&host_name, port),
        )
        .await
        .map_err(|e| format!("connect {host_name} via jump: {e}"))?
    } else {
        client::connect(
            client_cfg.clone(),
            (host_name.as_str(), port),
            HostKeyVerifier::new(&host_name, port),
        )
        .await
        .map_err(|e| format!("connect {host_name}:{port}: {e}"))?
    };

    authenticate(&mut session, &user, &identities).await?;

    let channel = session
        .channel_open_session()
        .await
        .map_err(|e| format!("open session channel: {e}"))?;
    // git wants the repo path single-quoted after the service command.
    let path = target.path.trim_start_matches('/');
    channel
        .exec(true, format!("{command} '{path}'"))
        .await
        .map_err(|e| format!("exec {command}: {e}"))?;

    let stream = channel.into_stream();
    let bridge = SyncIoBridge::new_with_handle(stream, runtime().handle().clone());
    Ok(SshStream {
        inner: Some(bridge),
        session: Some(session),
        jump: jump_handle,
        #[cfg(unix)]
        _master: master_keepalive,
    })
}

fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "git".to_owned())
}

/// A client config that also offers the older `ecdh-sha2-nistp*` key exchanges
/// that enterprise servers (e.g. GitLab behind a bastion) still require but
/// russh omits from its modern defaults.
fn client_config() -> client::Config {
    use std::borrow::Cow;
    let mut config = client::Config::default();
    let mut kex = config.preferred.kex.to_vec();
    for k in [
        russh::kex::ECDH_SHA2_NISTP256,
        russh::kex::ECDH_SHA2_NISTP384,
        russh::kex::ECDH_SHA2_NISTP521,
    ] {
        if !kex.contains(&k) {
            kex.push(k);
        }
    }
    config.preferred.kex = Cow::Owned(kex);
    config
}

/// A byte stream to the target host: either a russh direct-tcpip channel (a
/// fresh bastion connection) or a Unix socket handed to us by an existing
/// OpenSSH ControlMaster. Both carry the target's SSH session inside.
enum Tunnel {
    Unix(tokio::net::UnixStream),
    Channel(russh::ChannelStream<russh::client::Msg>),
}

impl AsyncRead for Tunnel {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Tunnel::Unix(s) => Pin::new(s).poll_read(cx, buf),
            Tunnel::Channel(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}
impl AsyncWrite for Tunnel {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Tunnel::Unix(s) => Pin::new(s).poll_write(cx, buf),
            Tunnel::Channel(s) => Pin::new(s).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Tunnel::Unix(s) => Pin::new(s).poll_flush(cx),
            Tunnel::Channel(s) => Pin::new(s).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Tunnel::Unix(s) => Pin::new(s).poll_shutdown(cx),
            Tunnel::Channel(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// The `ControlPath` for a host, with `%h`/`%p`/`%r`/`%n`/`%u` expanded. `None`
/// if unset, `none`, or it uses a token we cannot compute (e.g. `%C`).
fn resolve_control_path(cfg: &SshHostConfig, host: &str, port: u16, user: &str) -> Option<PathBuf> {
    let raw = cfg.control_path.as_ref()?;
    if raw.eq_ignore_ascii_case("none") {
        return None;
    }
    let expanded = raw
        .replace("%h", host)
        .replace("%n", host)
        .replace("%p", &port.to_string())
        .replace("%r", user)
        .replace("%u", &whoami())
        .replace("%%", "%");
    if expanded.contains('%') {
        return None; // an unsupported token like %C/%L/%l
    }
    Some(expand_home(&expanded))
}

/// Reuse an existing OpenSSH ControlMaster to open a `direct-tcpip` forward to
/// `host:port`, entirely in-process by speaking the mux protocol over the
/// control socket. Returns our end of a socketpair (the byte stream to the
/// target) plus the control connection to keep alive for the forward's life.
#[cfg(unix)]
fn controlmaster_forward(
    control_path: &Path,
    host: &str,
    port: u16,
) -> Option<(
    std::os::unix::net::UnixStream,
    std::os::unix::net::UnixStream,
)> {
    use nix::sys::socket::{
        AddressFamily, ControlMessage, MsgFlags, SockFlag, SockType, socketpair,
    };
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    const MUX_HELLO: u32 = 0x0000_0001;
    const MUX_C_NEW_STDIO_FWD: u32 = 0x1000_0008;
    const MUX_S_SESSION_OPENED: u32 = 0x8000_0006;
    const MUX_VERSION: u32 = 4;

    fn put_u32(v: &mut Vec<u8>, n: u32) {
        v.extend_from_slice(&n.to_be_bytes());
    }
    fn put_str(v: &mut Vec<u8>, s: &[u8]) {
        put_u32(v, s.len() as u32);
        v.extend_from_slice(s);
    }
    fn write_framed(sock: &mut UnixStream, payload: &[u8]) -> std::io::Result<()> {
        sock.write_all(&(payload.len() as u32).to_be_bytes())?;
        sock.write_all(payload)
    }
    fn read_framed(sock: &mut UnixStream) -> std::io::Result<Vec<u8>> {
        let mut len = [0u8; 4];
        sock.read_exact(&mut len)?;
        let mut buf = vec![0u8; u32::from_be_bytes(len) as usize];
        sock.read_exact(&mut buf)?;
        Ok(buf)
    }
    fn msg_type(b: &[u8]) -> Option<u32> {
        (b.len() >= 4).then(|| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    let mut master = UnixStream::connect(control_path).ok()?;

    // Hello handshake.
    let mut hello = Vec::new();
    put_u32(&mut hello, MUX_HELLO);
    put_u32(&mut hello, MUX_VERSION);
    write_framed(&mut master, &hello).ok()?;
    let reply = read_framed(&mut master).ok()?;
    if msg_type(&reply) != Some(MUX_HELLO) {
        return None;
    }

    // A socketpair: one end is handed to the master (as both the forward's
    // stdin and stdout), the other is our byte stream to the target.
    let (ours, theirs) = socketpair(
        AddressFamily::Unix,
        SockType::Stream,
        None,
        SockFlag::empty(),
    )
    .ok()?;

    // OpenSSH wants the request framed on its own, then the forward's stdin and
    // stdout fds each in a separate 1-byte SCM_RIGHTS message (mm_send_fd). Both
    // fds are the same socketpair end - the master reads from and writes to it.
    let mut msg = Vec::new();
    put_u32(&mut msg, MUX_C_NEW_STDIO_FWD);
    put_u32(&mut msg, 1); // request id
    put_str(&mut msg, b""); // reserved
    put_str(&mut msg, host.as_bytes());
    put_u32(&mut msg, port as u32);
    write_framed(&mut master, &msg).ok()?;

    let tfd = theirs.as_raw_fd();
    for _ in 0..2 {
        let byte = [0u8];
        let iov = [std::io::IoSlice::new(&byte)];
        let fds = [tfd];
        let cmsgs = [ControlMessage::ScmRights(&fds)];
        nix::sys::socket::sendmsg::<()>(master.as_raw_fd(), &iov, &cmsgs, MsgFlags::empty(), None)
            .ok()?;
    }

    let reply = read_framed(&mut master).ok()?;
    if msg_type(&reply) != Some(MUX_S_SESSION_OPENED) {
        return None;
    }
    // `theirs` is now dup'd into the master; drop our copy. Keep `master` and
    // `ours` alive - the forward lives as long as the control connection does.
    drop(theirs);
    Some((UnixStream::from(ours), master))
}

#[cfg(not(unix))]
fn controlmaster_forward(
    _control_path: &Path,
    _host: &str,
    _port: u16,
) -> Option<(std::net::TcpStream, std::net::TcpStream)> {
    None
}

/// Try public-key auth against a session: ssh-agent first (the usual home for
/// passphrase-protected keys), then each configured `IdentityFile`, then the
/// default `~/.ssh/id_*`.
async fn authenticate(
    session: &mut Handle<HostKeyVerifier>,
    user: &str,
    identities: &[PathBuf],
) -> Result<(), String> {
    // 1. ssh-agent: offer every identity it holds.
    if let Ok(mut agent) = russh::keys::agent::client::AgentClient::connect_env().await {
        if let Ok(ids) = agent.request_identities().await {
            for id in ids {
                let russh::keys::agent::AgentIdentity::PublicKey { key, .. } = &id else {
                    continue; // certificate identities: skip
                };
                let pubkey = key.clone();
                let hash_alg = pubkey.algorithm().is_rsa().then_some(HashAlg::Sha256);
                if let Ok(r) = session
                    .authenticate_publickey_with(user, pubkey, hash_alg, &mut agent)
                    .await
                {
                    if r.success() {
                        return Ok(());
                    }
                }
            }
        }
    }

    // 2. On-disk keys: configured IdentityFiles, then the defaults.
    let mut candidates: Vec<PathBuf> = identities.to_vec();
    if let Some(home) = home::home_dir() {
        for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
            candidates.push(home.join(".ssh").join(name));
        }
    }
    for path in candidates {
        let key = match russh::keys::load_secret_key(&path, None) {
            Ok(k) => k,
            Err(_) => continue, // missing, or passphrase-protected (skip for now)
        };
        // Sha256 is kept only for RSA keys; ignored otherwise.
        let keyh = PrivateKeyWithHashAlg::new(Arc::new(key), Some(HashAlg::Sha256));
        if let Ok(r) = session.authenticate_publickey(user, keyh).await {
            if r.success() {
                return Ok(());
            }
        }
    }

    // 3. Password, if the frontend installed a prompt (up to 3 tries).
    if let Some(ask) = PASSWORD_PROVIDER.get() {
        for _ in 0..3 {
            let Some(password) = ask(&format!("{user}'s password: ")) else {
                break;
            };
            if let Ok(r) = session.authenticate_password(user, &password).await {
                if r.success() {
                    return Ok(());
                }
            }
        }
    }

    Err(format!(
        "auth failed for {user} (tried ssh-agent, keys, and password)"
    ))
}

/// Verifies the server's host key against `~/.ssh/known_hosts`.
struct HostKeyVerifier {
    host: String,
    port: u16,
}

impl HostKeyVerifier {
    fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.to_owned(),
            port,
        }
    }
}

impl client::Handler for HostKeyVerifier {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = match server_public_key {
            russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } => key,
            // Certificate host keys are uncommon; refuse rather than guess.
            russh::keys::PublicKeyOrCertificate::Certificate(_) => return Ok(false),
        };
        match russh::keys::check_known_hosts(&self.host, self.port, key) {
            Ok(true) => Ok(true), // known and matches
            Ok(false) => {
                // Unknown host: trust on first use, like ssh's accept-new.
                tracing::warn!(target: "git", "accepting unknown host key for {}", self.host);
                Ok(true)
            }
            Err(e) => {
                // A changed key: refuse (possible MITM).
                tracing::error!(target: "git", "host key check failed for {}: {e}", self.host);
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ssh_url_with_user_port_path() {
        let t = parse_url("ssh://git@example.com:2222/org/repo.git").unwrap();
        assert_eq!(t.user.as_deref(), Some("git"));
        assert_eq!(t.host, "example.com");
        assert_eq!(t.port, Some(2222));
        assert_eq!(t.path, "/org/repo.git");
    }

    #[test]
    fn parse_ssh_url_without_user_or_port() {
        let t = parse_url("ssh://example.com/x.git").unwrap();
        assert_eq!(t.user, None);
        assert_eq!(t.host, "example.com");
        assert_eq!(t.port, None);
        assert_eq!(t.path, "/x.git");
    }

    #[test]
    fn parse_scp_form_url() {
        let t = parse_url("git@github.com:owner/repo.git").unwrap();
        assert_eq!(t.user.as_deref(), Some("git"));
        assert_eq!(t.host, "github.com");
        assert_eq!(t.port, None);
        // scp-form paths are relative (no leading slash).
        assert_eq!(t.path, "owner/repo.git");
    }

    #[test]
    fn parse_url_rejects_scp_form_without_colon() {
        // The scp branch needs a `host:path` colon; without one there is no
        // path to run the git service against.
        assert!(parse_url("just-a-host").is_err());
    }

    #[test]
    fn glob_matches_star_and_question() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*.example.com", "gw.example.com"));
        assert!(glob_match("git?", "gitx"));
        assert!(!glob_match("git?", "gitxy"));
        assert!(!glob_match("*.example.com", "example.net"));
        assert!(glob_match("exact", "exact"));
    }

    #[test]
    fn host_line_matches_wildcards_and_negation() {
        assert!(host_line_matches("bastion", "bastion"));
        assert!(host_line_matches("gw.example.com", "*.example.com"));
        // Case-insensitive.
        assert!(host_line_matches("BASTION", "bastion"));
        // Negation wins even when another pattern matches.
        assert!(!host_line_matches(
            "secret.example.com",
            "*.example.com !secret.example.com"
        ));
        assert!(!host_line_matches("other", "bastion gitserver"));
        assert!(host_line_matches("gitserver", "bastion gitserver"));
    }

    #[test]
    fn jump_host_from_proxy_jump() {
        let cfg = SshHostConfig {
            proxy_jump: Some("user@bastion.example.com:2222".to_owned()),
            ..Default::default()
        };
        assert_eq!(jump_host(&cfg).as_deref(), Some("bastion.example.com"));
    }

    #[test]
    fn jump_host_uses_last_proxy_jump_hop() {
        let cfg = SshHostConfig {
            proxy_jump: Some("first,second".to_owned()),
            ..Default::default()
        };
        assert_eq!(jump_host(&cfg).as_deref(), Some("second"));
    }

    #[test]
    fn jump_host_from_proxy_command() {
        let cfg = SshHostConfig {
            proxy_command: Some("ssh bastion -W %h:7999".to_owned()),
            ..Default::default()
        };
        assert_eq!(jump_host(&cfg).as_deref(), Some("bastion"));
    }

    #[test]
    fn jump_host_none_without_proxy() {
        assert_eq!(jump_host(&SshHostConfig::default()), None);
    }

    #[test]
    fn control_path_expands_tokens() {
        let cfg = SshHostConfig {
            control_path: Some("/tmp/cm/%h:%p:%r".to_owned()),
            ..Default::default()
        };
        let p = resolve_control_path(&cfg, "gw.example.com", 22, "alice").unwrap();
        assert_eq!(p, PathBuf::from("/tmp/cm/gw.example.com:22:alice"));
    }

    #[test]
    fn control_path_none_keyword_disables() {
        let cfg = SshHostConfig {
            control_path: Some("none".to_owned()),
            ..Default::default()
        };
        assert_eq!(resolve_control_path(&cfg, "h", 22, "u"), None);
    }

    #[test]
    fn control_path_unsupported_token_bails() {
        // %C (a hash) is not expanded; a leftover % means we cannot match ssh.
        let cfg = SshHostConfig {
            control_path: Some("/tmp/%C".to_owned()),
            ..Default::default()
        };
        assert_eq!(resolve_control_path(&cfg, "h", 22, "u"), None);
    }
}
