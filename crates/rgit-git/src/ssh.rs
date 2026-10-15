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
use std::sync::{Arc, OnceLock, RwLock};
use std::task::{Context, Poll};

use git2::transport::{Service, SmartSubtransport, SmartSubtransportStream, Transport};
use russh::client::{self, Handle};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::runtime::Runtime;
use tokio_util::io::SyncIoBridge;

/// A prompt callback that returns a password for `prompt`, or `None` to give up.
/// The frontend (CLI/TUI) installs one; without it, password auth is skipped.
type PasswordFn = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;
static PASSWORD_PROVIDER: RwLock<Option<PasswordFn>> = RwLock::new(None);

/// Install the password prompt used when key/agent auth fails and the server
/// offers password auth. Call once at startup.
pub fn set_password_provider(f: impl Fn(&str) -> Option<String> + Send + Sync + 'static) {
    *PASSWORD_PROVIDER
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(f));
}

thread_local! {
    static NO_PROMPT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `f` with the password prompt off on this thread. libgit2 drives the
/// ssh transport on its calling thread, so a background check stays silent.
pub(crate) fn without_prompt<T>(f: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            NO_PROMPT.set(self.0);
        }
    }
    let _restore = Restore(NO_PROMPT.replace(true));
    f()
}

/// The installed password prompt, if any.
fn password_provider() -> Option<PasswordFn> {
    if NO_PROMPT.get() {
        return None;
    }
    PASSWORD_PROVIDER
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
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

/// A byte stream to the target host: either a russh exec channel, or a plain
/// Unix socket served by a reused OpenSSH ControlMaster (no russh session).
enum SshInner {
    Channel(Box<SyncIoBridge<russh::ChannelStream<russh::client::Msg>>>),
    #[cfg(unix)]
    Mux(std::os::unix::net::UnixStream),
}

struct SshStream {
    inner: Option<SshInner>,
    session: Option<Handle<HostKeyVerifier>>,
    jump: Option<Handle<HostKeyVerifier>>,
    // The ControlMaster connection, kept open so its session/forward stays
    // alive. Plain socket - drops fine off-runtime, unlike the russh parts.
    #[cfg(unix)]
    _master: Option<std::os::unix::net::UnixStream>,
}

impl Read for SshStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.inner.as_mut().expect("ssh stream open") {
            SshInner::Channel(s) => s.read(buf),
            #[cfg(unix)]
            SshInner::Mux(s) => s.read(buf),
            #[cfg(not(unix))]
            _ => unreachable!("mux streams exist only on unix"),
        }
    }
}
impl Write for SshStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self.inner.as_mut().expect("ssh stream open") {
            SshInner::Channel(s) => s.write(buf),
            #[cfg(unix)]
            SshInner::Mux(s) => s.write(buf),
            #[cfg(not(unix))]
            _ => unreachable!("mux streams exist only on unix"),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self.inner.as_mut().expect("ssh stream open") {
            SshInner::Channel(s) => s.flush(),
            #[cfg(unix)]
            SshInner::Mux(s) => s.flush(),
            #[cfg(not(unix))]
            _ => unreachable!("mux streams exist only on unix"),
        }
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
    control_master: Option<String>,
    control_persist: Option<String>,
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
            "controlmaster" => {
                cfg.control_master
                    .get_or_insert_with(|| value.to_ascii_lowercase());
            }
            "controlpersist" => {
                cfg.control_persist.get_or_insert_with(|| value.to_owned());
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
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = home::home_dir()
    {
        return home.join(rest);
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

    // A live ControlMaster of the target itself: run the git service through
    // it with no handshake at all, and keep the control connection alive.
    // Liveness is a connect, not an exists(): a dead master leaves a socket
    // file behind, and a failed start or dead socket must reach the start
    // path, which clears the file.
    let cmd = format!("{command} '{}'", target.path.trim_start_matches('/'));
    #[cfg(unix)]
    if let Some(control_path) = resolve_control_path(&cfg, &host_name, port, &user) {
        let live = control_master_alive(&control_path)
            || controlmaster_start_and_check(&cfg, &control_path, port, &user, &target.host)
                .is_some();
        if live && let Some((stream, master)) = controlmaster_session(&control_path, &cmd) {
            return Ok(SshStream {
                inner: Some(SshInner::Mux(stream)),
                session: None,
                jump: None,
                _master: Some(master),
            });
        }
    }
    #[cfg(not(unix))]
    let _ = cmd;

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
        inner: Some(SshInner::Channel(Box::new(bridge))),
        session: Some(session),
        jump: jump_handle,
        #[cfg(unix)]
        _master: master_keepalive,
    })
}

fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "git".to_owned())
}

/// The socket for one askpass bridge: a unique private dir per start
/// (`rgit-askpass-<pid>-<n>/askpass.sock`). Pure: no directory is created
/// here; the start path prepares it.
#[cfg(unix)]
fn askpass_socket_path() -> Option<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    Some(
        std::env::temp_dir()
            .join(format!(
                "rgit-askpass-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ))
            .join("askpass.sock"),
    )
}

/// Create the socket's private parent dir, mode 0700, failing if it already
/// exists, so a stale or foreign dir is never reused or chmod'ed.
#[cfg(unix)]
fn askpass_prepare_dir(sock: &Path) -> Option<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let dir = sock.parent()?;
    std::fs::DirBuilder::new().mode(0o700).create(dir).ok()?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).ok()?;
    Some(())
}

/// A running askpass bridge. Dropping it removes the socket and its dir, so
/// every exit path cleans up.
#[cfg(unix)]
struct AskpassBridge {
    sock: PathBuf,
    stop_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(unix)]
impl AskpassBridge {
    fn stop(&self) {
        self.stop_flag
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // A no-op connect wakes the accept loop so it sees the flag.
        let _ = std::os::unix::net::UnixStream::connect(&self.sock);
        let _ = std::fs::remove_file(&self.sock);
        if let Some(parent) = self.sock.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

#[cfg(unix)]
impl Drop for AskpassBridge {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Serve askpass prompts over a Unix socket until stopped or dropped. Each
/// connection sends one length-prefixed prompt (a 4-byte big-endian byte
/// count, then the prompt: ssh's host-key question spans several lines), and
/// receives either the provider's answer line or an empty line when the
/// provider declined (the client then exits 1). A dropped client cancels the
/// prompt; a hung provider is bounded by the client's read timeout.
#[cfg(unix)]
fn askpass_bridge_start(sock: &Path, provider: PasswordFn) -> Option<AskpassBridge> {
    use std::io::{BufReader, Read, Write};
    use std::sync::atomic::AtomicBool;

    let listener = std::os::unix::net::UnixListener::bind(sock).ok()?;
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            if stop_thread.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let Ok(conn) = conn else {
                continue;
            };
            let provider = provider.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(conn);
                let mut len_bytes = [0u8; 4];
                if reader.read_exact(&mut len_bytes).is_err() {
                    return; // client went away: prompt cancelled
                }
                let len = u32::from_be_bytes(len_bytes) as usize;
                if len > 64 * 1024 {
                    return;
                }
                let mut prompt_bytes = vec![0u8; len];
                if reader.read_exact(&mut prompt_bytes).is_err() {
                    return;
                }
                let prompt = String::from_utf8_lossy(&prompt_bytes);
                let answer = provider(&prompt);
                let mut conn = reader.into_inner();
                // An empty line means failure: the client exits 1 and ssh
                // treats the prompt as declined.
                let line = answer.unwrap_or_default();
                let _ = writeln!(conn, "{line}");
                let _ = conn.flush();
            });
        }
    });
    Some(AskpassBridge {
        sock: sock.to_path_buf(),
        stop_flag: stop,
    })
}

/// One askpass exchange with the bridge: send the length-prefixed prompt, read
/// the answer. Returns `None` when the bridge declined or the exchange failed,
/// so ssh falls back to no password.
#[cfg(unix)]
pub fn askpass_client(sock: &Path, prompt: &str) -> Option<String> {
    use std::io::{BufRead, BufReader, Write};

    let mut conn = std::os::unix::net::UnixStream::connect(sock).ok()?;
    // A stuck bridge must not wedge ssh forever; ssh itself gives up long
    // before this.
    conn.set_read_timeout(Some(std::time::Duration::from_secs(600)))
        .ok()?;
    let len = (prompt.len() as u32).to_be_bytes();
    conn.write_all(&len).ok()?;
    conn.write_all(prompt.as_bytes()).ok()?;
    conn.flush().ok()?;
    let mut line = String::new();
    BufReader::new(conn).read_line(&mut line).ok()?;
    let answer = line.trim_end_matches(['\n', '\r']);
    (!answer.is_empty()).then(|| answer.to_owned())
}

/// The askpass re-exec entry point: rgit's main() calls this before argument
/// parsing when RGIT_ASKPASS_SOCKET is set. Prints the answer plus a newline
/// (what ssh's ssh_askpass reads) and exits 0, or exits 1 on failure. On
/// non-unix there is no bridge, so it always fails and the caller falls back
/// to the direct connect.
#[cfg(all(feature = "ssh", not(unix)))]
pub fn askpass_reexec() -> ! {
    std::process::exit(1);
}

#[cfg(unix)]
pub fn askpass_reexec() -> ! {
    let Some(path) = std::env::var_os("RGIT_ASKPASS_SOCKET").map(PathBuf::from) else {
        std::process::exit(1);
    };
    let prompt = std::env::args().nth(1).unwrap_or_default();
    match askpass_client(&path, &prompt) {
        Some(answer) => {
            println!("{answer}");
            std::process::exit(0);
        }
        None => std::process::exit(1),
    }
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

/// Whether ssh config asks this host to run a ControlMaster (`yes`, `auto` or
/// `autoask`; not `no`, `ask` or unset).
fn control_master_requested(cfg: &SshHostConfig) -> bool {
    matches!(
        cfg.control_master.as_deref(),
        Some("yes") | Some("auto") | Some("autoask")
    )
}

/// Whether a control socket accepts a connection, i.e. a master is listening.
/// A stale file from a dead master fails with ECONNREFUSED.
#[cfg(unix)]
fn control_master_alive(control_path: &Path) -> bool {
    control_path.exists() && std::os::unix::net::UnixStream::connect(control_path).is_ok()
}

/// The ssh argv plus the env vars to run it with.
#[cfg(unix)]
type MasterStart = (Vec<String>, Vec<(String, String)>);

/// The ssh argv that backgrounds a ControlMaster for the host, plus the env
/// to run it with. Pure: tests need no ssh, and no directory is created here.
/// With `with_provider`, the master may prompt for a password: BatchMode is
/// dropped and the askpass bridge (same binary, re-executed when
/// RGIT_ASKPASS_SOCKET is set) answers the prompts over a private Unix
/// socket. Without it, BatchMode=yes keeps ssh fully non-interactive.
#[cfg(unix)]
fn controlmaster_start_argv(
    control_path: &Path,
    control_persist: Option<&str>,
    port: Option<u16>,
    user: Option<&str>,
    host: &str,
    with_provider: bool,
) -> MasterStart {
    let mut argv = vec!["ssh".to_owned(), "-o".to_owned()];
    if with_provider {
        // No BatchMode: ssh may ask for a password, through the askpass bridge.
    } else {
        argv.push("BatchMode=yes".to_owned());
        argv.push("-o".to_owned());
    }
    argv.extend([
        "ControlMaster=yes".to_owned(),
        "-o".to_owned(),
        format!("ControlPath={}", control_path.display()),
        "-o".to_owned(),
        format!("ControlPersist={}", control_persist.unwrap_or("10m")),
        "-MNf".to_owned(),
    ]);
    if let Some(port) = port {
        argv.push("-p".to_owned());
        argv.push(port.to_string());
    }
    if let Some(user) = user {
        argv.push("-l".to_owned());
        argv.push(user.to_owned());
    }
    argv.push(host.to_owned());
    let env = if with_provider {
        let mut env = vec![
            (
                "SSH_ASKPASS".to_owned(),
                std::env::current_exe()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            ),
            ("SSH_ASKPASS_REQUIRE".to_owned(), "force".to_owned()),
            // readpass.c allows askpass only with DISPLAY set, even under
            // REQUIRE=force on some OpenSSH builds; a dummy value is enough.
            ("DISPLAY".to_owned(), "rgit-askpass".to_owned()),
        ];
        if let Some(sock) = askpass_socket_path() {
            env.push(("RGIT_ASKPASS_SOCKET".to_owned(), sock.display().to_string()));
        }
        env
    } else {
        Vec::new()
    };
    (argv, env)
}

/// A decided ControlMaster start plus the flock guarding it. The lock is held
/// until dropped, which must span the ssh -MNf run and the final liveness
/// check, so a second rgit process cannot start a second master meanwhile.
#[cfg(unix)]
struct MasterStartLocked {
    argv: Vec<String>,
    env: Vec<(String, String)>,
    _lock: nix::fcntl::Flock<std::fs::File>,
}

/// Decide whether to start a ControlMaster for the host, clearing a dead
/// socket (ECONNREFUSED only) out of the way first, serialized by an exclusive
/// flock on `<ControlPath>.lock` with a liveness re-check under the lock. The
/// returned guard keeps the lock; dropping it releases it. `None` when the
/// config does not ask for a master or a live one exists.
#[cfg(unix)]
fn controlmaster_start_decision(
    cfg: &SshHostConfig,
    control_path: &Path,
    port: u16,
    user: &str,
    host: &str,
) -> Option<MasterStartLocked> {
    use nix::fcntl::{Flock, FlockArg};

    if !control_master_requested(cfg) {
        return None;
    }
    if control_master_alive(control_path) {
        return None;
    }
    // The lock file lives beside the control socket, which must exist first.
    if let Some(parent) = control_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // One starter at a time per control path; the lock file itself is inert.
    let lock = Flock::lock(
        std::fs::File::create(lock_path(control_path)).ok()?,
        FlockArg::LockExclusive,
    )
    .ok()?;
    // A racing process may have started a master while we waited.
    if control_master_alive(control_path) {
        drop(lock);
        return None;
    }
    // A dead master leaves its socket file behind; connect fails with
    // ECONNREFUSED. Delete it so ssh can bind a fresh one. Other errors
    // (e.g. EACCES) are left alone for ssh to report.
    if control_path.exists()
        && let Err(e) = std::os::unix::net::UnixStream::connect(control_path)
        && e.raw_os_error() == Some(libc::ECONNREFUSED)
    {
        let _ = std::fs::remove_file(control_path);
    }
    let (argv, env) = controlmaster_start_argv(
        control_path,
        cfg.control_persist.as_deref(),
        Some(port),
        Some(user),
        host,
        password_provider().is_some(),
    );
    Some(MasterStartLocked {
        argv,
        env,
        _lock: lock,
    })
}

/// The flock file guarding a control path's startup.
#[cfg(unix)]
fn lock_path(control_path: &Path) -> PathBuf {
    let mut name = control_path.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// Start a backgrounded ControlMaster when ssh config asks for one and none is
/// live, then report whether a usable control socket now exists. Any failure
/// just returns `None` so the caller falls back to the direct connect.
#[cfg(unix)]
fn controlmaster_start_and_check(
    cfg: &SshHostConfig,
    control_path: &Path,
    port: u16,
    user: &str,
    host: &str,
) -> Option<()> {
    let start = controlmaster_start_decision(cfg, control_path, port, user, host)?;
    let MasterStartLocked { argv, env, .. } = start;
    // The flock stays held until start drops at function exit, covering the
    // ssh run and the final liveness check below.
    // With a password provider installed, answer ssh's prompts in-process:
    // the askpass program is this same binary, re-executed with
    // RGIT_ASKPASS_SOCKET set (see cli main.rs). The bridge binds the exact
    // socket path from the env; a prepare/bind failure leaves no bridge, ssh
    // prompts fail, and we fall back below. The Drop guard cleans up on every
    // exit path.
    let _bridge = env
        .iter()
        .find(|(k, _)| k == "RGIT_ASKPASS_SOCKET")
        .map(|(_, v)| PathBuf::from(v))
        .filter(|sock| askpass_prepare_dir(sock).is_some())
        .zip(password_provider())
        .and_then(|(sock, provider)| askpass_bridge_start(&sock, provider));
    let status = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .envs(env)
        .status()
        .ok()?;
    // _bridge drops here; if ssh failed or no socket appeared, the caller
    // falls back to the direct connect.
    if !status.success() || !control_master_alive(control_path) {
        return None;
    }
    Some(())
}

/// Mux message types shared by the ControlMaster helpers.
#[cfg(unix)]
mod mux {
    pub(crate) const MUX_HELLO: u32 = 0x0000_0001;
    pub(crate) const MUX_C_NEW_SESSION: u32 = 0x1000_0002;
    pub(crate) const MUX_C_NEW_STDIO_FWD: u32 = 0x1000_0008;
    pub(crate) const MUX_S_SESSION_OPENED: u32 = 0x8000_0006;
    // Only the fake mux server in tests sends failure replies.
    #[cfg(test)]
    pub(crate) const MUX_S_FAILURE: u32 = 0x8000_0003;
    pub(crate) const MUX_VERSION: u32 = 4;

    pub(crate) fn put_u32(v: &mut Vec<u8>, n: u32) {
        v.extend_from_slice(&n.to_be_bytes());
    }
    pub(crate) fn put_str(v: &mut Vec<u8>, s: &[u8]) {
        put_u32(v, s.len() as u32);
        v.extend_from_slice(s);
    }
    pub(crate) fn write_framed(
        sock: &mut std::os::unix::net::UnixStream,
        payload: &[u8],
    ) -> std::io::Result<()> {
        use std::io::Write;
        sock.write_all(&(payload.len() as u32).to_be_bytes())?;
        sock.write_all(payload)
    }
    pub(crate) fn read_framed(
        sock: &mut std::os::unix::net::UnixStream,
    ) -> std::io::Result<Vec<u8>> {
        use std::io::Read;
        let mut len = [0u8; 4];
        sock.read_exact(&mut len)?;
        let mut buf = vec![0u8; u32::from_be_bytes(len) as usize];
        sock.read_exact(&mut buf)?;
        Ok(buf)
    }
    pub(crate) fn msg_type(b: &[u8]) -> Option<u32> {
        (b.len() >= 4).then(|| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    /// OpenSSH reads passed fds from separate 1-byte SCM_RIGHTS messages
    /// (mm_send_fd / mm_receive_fd in monitor_fdpass.c).
    pub(crate) fn send_fd(
        sock: &std::os::unix::net::UnixStream,
        fd: std::os::fd::RawFd,
    ) -> Option<()> {
        use nix::sys::socket::{ControlMessage, MsgFlags};
        use std::io::IoSlice;
        use std::os::fd::AsRawFd;
        let byte = [0u8];
        let iov = [IoSlice::new(&byte)];
        let cmsgs = [ControlMessage::ScmRights(&[fd])];
        nix::sys::socket::sendmsg::<()>(sock.as_raw_fd(), &iov, &cmsgs, MsgFlags::empty(), None)
            .ok()
            .map(|_| ())
    }
}

/// Open a session channel through a live OpenSSH ControlMaster, running
/// `command` with fds passed by SCM_RIGHTS. Returns our end of the session's
/// socketpair (stdin+stdout; stderr is /dev/null) plus the control connection
/// to keep alive. Field order per OpenSSH mux.c
/// `mux_master_process_new_session`: rid, reserved, want_tty, want_x11,
/// want_agent, subsystem, escape_char, term, command, then env strings.
#[cfg(unix)]
fn controlmaster_session(
    control_path: &Path,
    command: &str,
) -> Option<(
    std::os::unix::net::UnixStream,
    std::os::unix::net::UnixStream,
)> {
    use mux::*;
    use nix::sys::socket::{AddressFamily, SockFlag, SockType, socketpair};
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    let mut master = UnixStream::connect(control_path).ok()?;

    let mut hello = Vec::new();
    put_u32(&mut hello, MUX_HELLO);
    put_u32(&mut hello, MUX_VERSION);
    write_framed(&mut master, &hello).ok()?;
    let reply = read_framed(&mut master).ok()?;
    if msg_type(&reply) != Some(MUX_HELLO) {
        return None;
    }

    // One socketpair end is the session's stdin and stdout, as two of the
    // three fds OpenSSH gathers; the other end is our byte stream.
    let (ours, theirs) = socketpair(
        AddressFamily::Unix,
        SockType::Stream,
        None,
        SockFlag::empty(),
    )
    .ok()?;
    let devnull = std::fs::File::open("/dev/null").ok()?;

    let mut msg = Vec::new();
    put_u32(&mut msg, MUX_C_NEW_SESSION);
    put_u32(&mut msg, 1); // request id
    put_str(&mut msg, b""); // reserved
    put_u32(&mut msg, 0); // want_tty
    put_u32(&mut msg, 0); // want_x11
    put_u32(&mut msg, 0); // want_agent
    put_u32(&mut msg, 0); // subsystem
    put_u32(&mut msg, 0xffff_ffff); // escape_char: none
    put_str(&mut msg, b""); // term
    put_str(&mut msg, command.as_bytes());
    write_framed(&mut master, &msg).ok()?;

    let tfd = theirs.as_raw_fd();
    send_fd(&master, tfd)?;
    send_fd(&master, tfd)?;
    send_fd(&master, devnull.as_raw_fd())?;

    let reply = read_framed(&mut master).ok()?;
    if msg_type(&reply) != Some(MUX_S_SESSION_OPENED) {
        return None;
    }
    drop(theirs);
    Some((UnixStream::from(ours), master))
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
    use mux::*;
    use nix::sys::socket::{AddressFamily, SockFlag, SockType, socketpair};
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    let mut master = UnixStream::connect(control_path).ok()?;

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
    send_fd(&master, tfd)?;
    send_fd(&master, tfd)?;

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
    if let Ok(mut agent) = russh::keys::agent::client::AgentClient::connect_env().await
        && let Ok(ids) = agent.request_identities().await
    {
        for id in ids {
            let russh::keys::agent::AgentIdentity::PublicKey { key, .. } = &id else {
                continue; // certificate identities: skip
            };
            let pubkey = key.clone();
            let hash_alg = pubkey.algorithm().is_rsa().then_some(HashAlg::Sha256);
            if let Ok(r) = session
                .authenticate_publickey_with(user, pubkey, hash_alg, &mut agent)
                .await
                && r.success()
            {
                return Ok(());
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
        if let Ok(r) = session.authenticate_publickey(user, keyh).await
            && r.success()
        {
            return Ok(());
        }
    }

    // 3. Password, if the frontend installed a prompt (up to 3 tries).
    if let Some(ask) = password_provider() {
        for _ in 0..3 {
            let Some(password) = ask(&format!("{user}'s password: ")) else {
                break;
            };
            if let Ok(r) = session.authenticate_password(user, &password).await
                && r.success()
            {
                return Ok(());
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

    #[cfg(unix)]
    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rgit-ssh-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    fn bare_repo_with_main(path: &Path) {
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        std::process::Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .arg(path)
            .output()
            .unwrap();
        let tree = run(&["mktree"]);
        let commit = run(&["commit-tree", &tree, "-m", "init"]);
        run(&["update-ref", "refs/heads/main", &commit]);
    }

    // A stand-in for the OpenSSH master: mux hello, then NEW_SESSION with 3
    // passed fds, which run git-upload-pack so the client sees real output.
    #[cfg(unix)]
    fn fake_master(sock: &Path, repo: PathBuf, reply_failure: bool) -> std::thread::JoinHandle<()> {
        use super::mux::{
            MUX_C_NEW_SESSION, MUX_HELLO, MUX_S_FAILURE, MUX_S_SESSION_OPENED, MUX_VERSION,
            msg_type, put_str, put_u32, read_framed, write_framed,
        };
        use nix::sys::socket::{ControlMessageOwned, MsgFlags, recvmsg};
        use std::io::IoSliceMut;
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

        let listener = std::os::unix::net::UnixListener::bind(sock).unwrap();
        std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let hello = read_framed(&mut conn).unwrap();
            assert_eq!(msg_type(&hello), Some(MUX_HELLO));
            let mut reply = Vec::new();
            put_u32(&mut reply, MUX_HELLO);
            put_u32(&mut reply, MUX_VERSION);
            write_framed(&mut conn, &reply).unwrap();

            let msg = read_framed(&mut conn).unwrap();
            assert_eq!(msg_type(&msg), Some(MUX_C_NEW_SESSION));
            if reply_failure {
                let mut reply = Vec::new();
                put_u32(&mut reply, MUX_S_FAILURE);
                put_u32(&mut reply, 1);
                put_str(&mut reply, b"refused");
                write_framed(&mut conn, &reply).unwrap();
                return;
            }

            // Layout per mux.c mux_master_process_new_session: rid, reserved,
            // want_tty, want_x11, want_agent, subsystem, escape, term, command.
            let mut p = &msg[8..];
            let take_u32 = |p: &mut &[u8]| {
                let v = u32::from_be_bytes([p[0], p[1], p[2], p[3]]);
                *p = &p[4..];
                v
            };
            let take_str = |p: &mut &[u8]| {
                let n = take_u32(p) as usize;
                let s = String::from_utf8(p[..n].to_vec()).unwrap();
                *p = &p[n..];
                s
            };
            let _reserved = take_str(&mut p);
            let _tty = take_u32(&mut p);
            let _x11 = take_u32(&mut p);
            let _agent = take_u32(&mut p);
            let _subsys = take_u32(&mut p);
            let _escape = take_u32(&mut p);
            let _term = take_str(&mut p);
            let command = take_str(&mut p);
            assert!(p.is_empty(), "no env strings expected");
            let repo_arg = command
                .strip_prefix("git-upload-pack '")
                .and_then(|c| c.strip_suffix("'"))
                .unwrap();
            assert_eq!(Path::new(repo_arg), repo.as_path());

            let mut fds: Vec<OwnedFd> = Vec::new();
            for _ in 0..3 {
                let mut buf = [0u8; 1];
                let mut iov = [IoSliceMut::new(&mut buf)];
                let mut cmsg = nix::cmsg_space!([RawFd; 1]);
                let m = recvmsg::<()>(
                    conn.as_raw_fd(),
                    &mut iov,
                    Some(&mut cmsg),
                    MsgFlags::empty(),
                )
                .unwrap();
                for c in m.cmsgs().unwrap() {
                    if let ControlMessageOwned::ScmRights(r) = c {
                        fds.extend(r.into_iter().map(|fd| unsafe { OwnedFd::from_raw_fd(fd) }));
                    }
                }
            }
            assert_eq!(fds.len(), 3);

            let mut reply = Vec::new();
            put_u32(&mut reply, MUX_S_SESSION_OPENED);
            put_u32(&mut reply, 1);
            put_u32(&mut reply, 0); // channel id
            write_framed(&mut conn, &reply).unwrap();

            let mut child = std::process::Command::new("git-upload-pack")
                .arg(&repo)
                .stdin(std::process::Stdio::from(fds.remove(0)))
                .stdout(std::process::Stdio::from(fds.remove(0)))
                .stderr(std::process::Stdio::from(fds.remove(0)))
                .spawn()
                .unwrap();
            // git-upload-pack exits only after the client sends the flush
            // packet; a stuck client would hang this thread forever.
            for _ in 0..50 {
                if child.try_wait().unwrap().is_some() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let _ = child.kill();
            let _ = child.wait();
            panic!("git-upload-pack did not exit within 5 s");
        })
    }

    #[cfg(unix)]
    #[test]
    fn controlmaster_session_reads_ref_advertisement() {
        let dir = test_dir("session-ok");
        let repo = dir.join("repo.git");
        bare_repo_with_main(&repo);
        let sock = dir.join("ctl");
        let server = fake_master(&sock, repo.clone(), false);

        let (mut ours, _master) =
            controlmaster_session(&sock, &format!("git-upload-pack '{}'", repo.display()))
                .expect("session through fake master");
        ours.set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 512];
        while !buf
            .windows(b"refs/heads/main".len())
            .any(|w| w == b"refs/heads/main")
        {
            let n = ours.read(&mut chunk).expect("advertisement read");
            assert!(n > 0, "EOF before refs/heads/main");
            buf.extend_from_slice(&chunk[..n]);
        }
        // git-upload-pack holds the connection open after the advertisement
        // until the client's flush packet, so send it and close our end.
        ours.write_all(b"0000").unwrap();
        drop(ours);
        drop(_master);
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn controlmaster_session_missing_socket_returns_none() {
        let dir = test_dir("session-missing");
        assert!(controlmaster_session(&dir.join("nope"), "git-upload-pack '/x'").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn controlmaster_session_failure_reply_returns_none() {
        let dir = test_dir("session-fail");
        let repo = dir.join("repo.git");
        bare_repo_with_main(&repo);
        let sock = dir.join("ctl");
        let server = fake_master(&sock, repo, true);
        assert!(controlmaster_session(&sock, "git-upload-pack '/x'").is_none());
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn controlmaster_start_argv_full() {
        let (argv, _env) = controlmaster_start_argv(
            &PathBuf::from("/tmp/ctl"),
            Some("15m"),
            Some(2222),
            Some("alice"),
            "git.example.com",
            false,
        );
        assert_eq!(
            argv,
            vec![
                "ssh".to_owned(),
                "-o".to_owned(),
                "BatchMode=yes".to_owned(),
                "-o".to_owned(),
                "ControlMaster=yes".to_owned(),
                "-o".to_owned(),
                "ControlPath=/tmp/ctl".to_owned(),
                "-o".to_owned(),
                "ControlPersist=15m".to_owned(),
                "-MNf".to_owned(),
                "-p".to_owned(),
                "2222".to_owned(),
                "-l".to_owned(),
                "alice".to_owned(),
                "git.example.com".to_owned(),
            ]
        );
    }

    #[test]
    fn controlmaster_start_argv_without_persist_port_user() {
        let (argv, _env) =
            controlmaster_start_argv(&PathBuf::from("/tmp/ctl"), None, None, None, "h", false);
        assert_eq!(argv[8], "ControlPersist=10m");
        assert!(!argv.iter().any(|a| a == "-p"));
        assert!(!argv.iter().any(|a| a == "-l"));
        assert_eq!(argv.last().unwrap(), "h");
    }

    #[test]
    fn no_prompt_hides_the_password_provider_on_this_thread_only() {
        set_password_provider(|_| Some("secret".to_owned()));
        assert!(without_prompt(|| password_provider().is_none()));
        assert!(without_prompt(|| {
            std::thread::spawn(|| password_provider().is_some())
                .join()
                .unwrap()
        }));
        assert!(password_provider().is_some());
        set_password_provider(|_| None);
    }

    #[cfg(unix)]
    #[test]
    fn controlmaster_askpass_provider_replace_last_install_wins() {
        // The last install wins: the CLI's terminal prompt is replaced by a
        // later one (the TUI's), so the second provider must answer.
        set_password_provider(|_| Some("first".to_owned()));
        set_password_provider(|_| Some("second".to_owned()));
        let answer = password_provider().and_then(|p| p("any"));
        assert_eq!(answer.as_deref(), Some("second"));
        // Restore a declining provider: other tests rely on that default.
        set_password_provider(|_| None);
    }

    #[cfg(unix)]
    #[test]
    fn controlmaster_askpass_round_trip() {
        let dir = test_dir("askpass");
        let sock = dir.join("askpass.sock");
        let handle = askpass_bridge_start(
            &sock,
            Arc::new(|p: &str| (!p.is_empty()).then(|| format!("secret-for-{p}"))),
        );
        let answer = askpass_client(&sock, "git@example.com's password: ");
        assert_eq!(
            answer.as_deref(),
            Some("secret-for-git@example.com's password: ")
        );
        let none = askpass_client(&sock, "");
        assert_eq!(none, None);
        drop(handle);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn controlmaster_askpass_none_answer_fails() {
        let dir = test_dir("askpass-none");
        let sock = dir.join("askpass.sock");
        let handle = askpass_bridge_start(&sock, Arc::new(|_: &str| None)).unwrap();
        assert_eq!(askpass_client(&sock, "any prompt"), None);
        handle.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn controlmaster_askpass_multiline_prompt_reaches_provider() {
        let dir = test_dir("askpass-multiline");
        let sock = dir.join("askpass.sock");
        // ssh's host-key question spans lines and ends with (yes/no)?.
        let prompt = "The authenticity of host 'h' can't be established.\nED25519 key fingerprint is SHA256:abc.\nAre you sure you want to continue connecting (yes/no)? ";
        let got = Arc::new(std::sync::Mutex::new(None::<String>));
        let got2 = got.clone();
        let handle = askpass_bridge_start(
            &sock,
            Arc::new(move |p: &str| {
                *got2.lock().unwrap() = Some(p.to_owned());
                Some("yes".to_owned())
            }),
        )
        .unwrap();
        assert_eq!(askpass_client(&sock, prompt).as_deref(), Some("yes"));
        assert_eq!(got.lock().unwrap().as_deref(), Some(prompt));
        handle.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn controlmaster_askpass_bind_failure_returns_none_and_no_dir() {
        let dir = test_dir("askpass-bindfail");
        let sock = dir.join("askpass.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        // The path is already bound: the second bind fails, and start must
        // return None instead of panicking.
        assert!(askpass_bridge_start(&sock, Arc::new(|_: &str| None)).is_none());
        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn controlmaster_askpass_unique_socket_per_start() {
        let a = askpass_socket_path().unwrap();
        let b = askpass_socket_path().unwrap();
        assert_ne!(a, b);
    }

    #[cfg(unix)]
    #[test]
    fn controlmaster_askpass_env_socket_pairs_with_the_bridge() {
        // The real pairing: the env's RGIT_ASKPASS_SOCKET from the argv
        // builder is the exact path the bridge binds, the client reaches it,
        // and the private dir is gone after drop.
        let (_, env) =
            controlmaster_start_argv(&PathBuf::from("/tmp/ctl"), None, None, None, "h", true);
        let sock = PathBuf::from(
            env.iter()
                .find(|(k, _)| k == "RGIT_ASKPASS_SOCKET")
                .map(|(_, v)| v.as_str())
                .unwrap(),
        );
        assert!(askpass_prepare_dir(&sock).is_some());
        let handle =
            askpass_bridge_start(&sock, Arc::new(|p: &str| Some(format!("answer-{p}")))).unwrap();
        assert_eq!(
            askpass_client(&sock, "prompt").as_deref(),
            Some("answer-prompt")
        );
        let dir = sock.parent().unwrap().to_path_buf();
        drop(handle);
        assert!(!sock.exists());
        assert!(!dir.exists());
    }

    #[cfg(unix)]
    #[test]
    fn controlmaster_askpass_prepare_dir_fails_on_existing_dir() {
        let dir = test_dir("askpass-prepare");
        let sock = dir.join("askpass.sock");
        // An existing dir must not be reused or chmod'ed: creation fails.
        assert!(askpass_prepare_dir(&sock).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn controlmaster_askpass_drop_cleans_up_socket_and_dir() {
        let dir = test_dir("askpass-drop");
        let sock = dir.join("sub").join("askpass.sock");
        std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
        let handle = askpass_bridge_start(&sock, Arc::new(|_: &str| None)).unwrap();
        assert!(sock.exists());
        drop(handle);
        assert!(!sock.exists());
        assert!(!sock.parent().unwrap().exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn controlmaster_askpass_argv_env_with_and_without_provider() {
        let (argv, env) =
            controlmaster_start_argv(&PathBuf::from("/tmp/ctl"), None, None, None, "h", true);
        assert!(!argv.iter().any(|a| a == "BatchMode=yes"));
        assert!(
            argv.iter().any(|a| a == "SSH_ASKPASS") || env.iter().any(|(k, _)| k == "SSH_ASKPASS")
        );
        assert_eq!(
            env.iter()
                .find(|(k, _)| k == "SSH_ASKPASS_REQUIRE")
                .map(|(_, v)| v.as_str()),
            Some("force")
        );
        assert!(env.iter().any(|(k, _)| k == "RGIT_ASKPASS_SOCKET"));
        assert_eq!(
            env.iter()
                .find(|(k, _)| k == "DISPLAY")
                .map(|(_, v)| v.as_str()),
            Some("rgit-askpass")
        );

        let (argv, env) =
            controlmaster_start_argv(&PathBuf::from("/tmp/ctl"), None, None, None, "h", false);
        assert!(argv.contains(&"BatchMode=yes".to_owned()));
        assert!(!env.iter().any(|(k, _)| k == "SSH_ASKPASS"));
        assert!(!env.iter().any(|(k, _)| k == "SSH_ASKPASS_REQUIRE"));
        assert!(!env.iter().any(|(k, _)| k == "RGIT_ASKPASS_SOCKET"));
        assert!(!env.iter().any(|(k, _)| k == "DISPLAY"));
    }

    #[test]
    fn controlmaster_start_decision_cases() {
        let dir = test_dir("start-decision");
        let sock = dir.join("ctl");
        let cfg = |cm: Option<&str>, cp: Option<&str>| SshHostConfig {
            control_master: cm.map(|s| s.to_owned()),
            control_path: Some(sock.display().to_string()),
            control_persist: cp.map(|s| s.to_owned()),
            ..Default::default()
        };
        // Unset, no and ask never start.
        assert!(controlmaster_start_decision(&cfg(None, None), &sock, 22, "u", "h").is_none());
        assert!(
            controlmaster_start_decision(&cfg(Some("no"), None), &sock, 22, "u", "h").is_none()
        );
        assert!(
            controlmaster_start_decision(&cfg(Some("ask"), None), &sock, 22, "u", "h").is_none()
        );
        // yes starts.
        assert!(
            controlmaster_start_decision(&cfg(Some("yes"), None), &sock, 22, "u", "h").is_some()
        );
        assert!(
            controlmaster_start_decision(&cfg(Some("auto"), None), &sock, 22, "u", "h").is_some()
        );
        assert!(
            controlmaster_start_decision(&cfg(Some("autoask"), None), &sock, 22, "u", "h")
                .is_some()
        );

        // A live socket suppresses the start.
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        assert!(
            controlmaster_start_decision(&cfg(Some("yes"), None), &sock, 22, "u", "h").is_none()
        );
        drop(listener);

        // A dead socket (bound and closed listener) is deleted, then a start
        // is chosen. macOS can complete one connect against the dropped
        // listener; wait for it to settle before asserting.
        assert!(sock.exists());
        for _ in 0..100 {
            if std::os::unix::net::UnixStream::connect(&sock).is_err() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(
            controlmaster_start_decision(&cfg(Some("yes"), None), &sock, 22, "u", "h").is_some()
        );
        assert!(!sock.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn controlmaster_start_dead_socket_reaches_start_path() {
        // A dead socket file must look not-alive, must lead to a start
        // decision, and must be gone after the decision cleared it.
        let dir = test_dir("start-dead-real");
        let sock = dir.join("ctl");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        drop(listener);
        assert!(sock.exists());
        // macOS can complete one connect against the dropped listener; wait
        // for it to settle before asserting.
        for _ in 0..100 {
            if !control_master_alive(&sock) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(!control_master_alive(&sock));

        let cfg = SshHostConfig {
            control_master: Some("yes".to_owned()),
            control_path: Some(sock.display().to_string()),
            ..Default::default()
        };
        let start = controlmaster_start_decision(&cfg, &sock, 22, "u", "h");
        assert!(start.is_some(), "a dead socket must lead to a start");
        assert!(!sock.exists(), "the dead socket file must be deleted");
        drop(start); // release the flock before cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn controlmaster_start_serializes_on_lock() {
        // The flock must be held across the decision: a second caller blocks
        // while the first holds it, and only after the release sees whatever
        // the first left behind (here a live master, suppressing the start).
        let dir = test_dir("start-lock");
        let sock = dir.join("ctl");
        std::fs::create_dir_all(&dir).unwrap();
        use nix::fcntl::{Flock, FlockArg};
        let first = Flock::lock(
            std::fs::File::create(lock_path(&sock)).unwrap(),
            FlockArg::LockExclusive,
        )
        .unwrap();
        let cfg = SshHostConfig {
            control_master: Some("yes".to_owned()),
            control_path: Some(sock.display().to_string()),
            ..Default::default()
        };
        let holder = {
            let sock = sock.clone();
            std::thread::spawn(move || {
                controlmaster_start_decision(&cfg, &sock, 22, "u", "h").is_some()
            })
        };
        // While the first lock is held, bind a live master, then release. The
        // second caller must have been blocked until now, seeing the live
        // socket under the lock and refusing to start its own.
        std::thread::sleep(std::time::Duration::from_millis(100));
        let live = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        assert!(
            !holder.is_finished(),
            "the second caller must still be blocked on the flock"
        );
        drop(first); // release the flock
        assert!(
            !holder.join().unwrap(),
            "a live master must suppress the start"
        );
        drop(live);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
