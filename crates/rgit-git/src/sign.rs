//! Signing and verifying commits and tags the way git's gpg-interface does:
//! gpg (openpgp), gpgsm (x509) or ssh-keygen (ssh), per `gpg.format`.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use git2::{Oid, Repository};

use crate::GitError;

const PGP: [&str; 2] = [
    "-----BEGIN PGP SIGNATURE-----",
    "-----BEGIN PGP MESSAGE-----",
];
const X509: &str = "-----BEGIN SIGNED MESSAGE-----";
const SSH: &str = "-----BEGIN SSH SIGNATURE-----";

/// What checking one signature found, as git's `signature_check`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignatureCheck {
    /// git's %G? letter before trust: G, B, E, X, Y, R or N (no signature).
    pub result: char,
    /// The trust level word (undefined, never, marginal, fully, ultimate).
    pub trust: String,
    pub signer: String,
    pub key: String,
    pub fingerprint: String,
    pub primary_key: String,
    /// The verifier's report for people (gpg's stderr, ssh-keygen's output).
    pub output: String,
    /// The verifier's machine status lines (`--raw`).
    pub status: String,
    /// The signed data.
    pub payload: Vec<u8>,
    /// Whether the check passed, as `verify-commit`'s exit code.
    pub good: bool,
}

impl SignatureCheck {
    /// git's `%G?`: a good signature whose key is not trusted is `U`.
    pub fn letter(&self) -> char {
        if self.result == 'G' && matches!(self.trust.as_str(), "undefined" | "never") {
            'U'
        } else {
            self.result
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Format {
    OpenPgp,
    X509,
    Ssh,
}

fn format(repo: &Repository) -> Result<Format, GitError> {
    match cfg(repo, "gpg.format").as_deref() {
        None | Some("openpgp") => Ok(Format::OpenPgp),
        Some("x509") => Ok(Format::X509),
        Some("ssh") => Ok(Format::Ssh),
        Some(other) => Err(GitError::Other(format!(
            "invalid value for 'gpg.format': '{other}'"
        ))),
    }
}

fn format_of(signature: &str) -> Format {
    if signature.starts_with(SSH) {
        Format::Ssh
    } else if signature.starts_with(X509) {
        Format::X509
    } else {
        Format::OpenPgp
    }
}

fn cfg(repo: &Repository, key: &str) -> Option<String> {
    config(repo)?.get_string(key).ok()
}

/// The config git would read here, honouring GIT_CONFIG_GLOBAL.
fn config(repo: &Repository) -> Option<git2::Config> {
    crate::config::open_config(repo, crate::ConfigScope::Any, false).ok()
}

fn program(repo: &Repository, f: Format) -> String {
    match f {
        Format::OpenPgp => cfg(repo, "gpg.openpgp.program")
            .or_else(|| cfg(repo, "gpg.program"))
            .unwrap_or_else(|| "gpg".to_owned()),
        Format::X509 => cfg(repo, "gpg.x509.program").unwrap_or_else(|| "gpgsm".to_owned()),
        Format::Ssh => cfg(repo, "gpg.ssh.program").unwrap_or_else(|| "ssh-keygen".to_owned()),
    }
}

/// Whether `key` (from config or -u / -S) is set to sign by default.
pub(crate) fn config_bool(repo: &Repository, key: &str) -> bool {
    config(repo)
        .and_then(|c| c.get_bool(key).ok())
        .unwrap_or(false)
}

/// A temporary file removed when dropped.
struct Temp(PathBuf);

impl Temp {
    fn new(tag: &str, data: &[u8]) -> Result<Temp, GitError> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let path = std::env::temp_dir().join(format!(
            ".git_{tag}_tmp{}{nanos}{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, data)?;
        Ok(Temp(path))
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Run `prog args` with `input` on stdin: (success, stdout, stderr).
fn pipe(prog: &str, args: &[&str], input: &[u8]) -> Result<(bool, String, String), GitError> {
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| GitError::Other(format!("cannot run {prog}: {e}")))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let input = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child.wait_with_output()?;
    let _ = writer.join();
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

/// `Name <email>` of the committer, gpg's default key.
fn committer_ident(repo: &Repository) -> Result<String, GitError> {
    let sig = repo.signature()?;
    Ok(format!(
        "{} <{}>",
        sig.name().unwrap_or(""),
        sig.email().unwrap_or("")
    ))
}

/// The key to sign with: `key` when given, else user.signingKey, else the
/// committer (gpg) or gpg.ssh.defaultKeyCommand (ssh).
fn signing_key(repo: &Repository, f: Format, key: Option<&str>) -> Result<String, GitError> {
    if let Some(k) = key.filter(|k| !k.is_empty()) {
        return Ok(k.to_owned());
    }
    if let Some(k) = cfg(repo, "user.signingkey") {
        return Ok(k);
    }
    if f != Format::Ssh {
        return committer_ident(repo);
    }
    let Some(cmd) = cfg(repo, "gpg.ssh.defaultKeyCommand") else {
        return Err(GitError::Other(
            "either user.signingkey or gpg.ssh.defaultKeyCommand needs to be configured".into(),
        ));
    };
    let out = Command::new("sh").arg("-c").arg(&cmd).output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find(|l| l.starts_with("ssh-") || l.starts_with("key::"))
        .map(str::to_owned)
        .ok_or_else(|| {
            GitError::Other(format!(
                "gpg.ssh.defaultKeyCommand succeeded but returned no keys: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        })
}

fn expand_home(path: &str) -> String {
    match (path.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => format!("{home}/{rest}"),
        _ => path.to_owned(),
    }
}

/// Sign `payload` as git's sign_buffer does, with `key` or the default key;
/// returns the armored signature.
pub fn sign_buffer(
    repo: &Repository,
    payload: &[u8],
    key: Option<&str>,
) -> Result<String, GitError> {
    let f = format(repo)?;
    let prog = program(repo, f);
    let key = signing_key(repo, f, key)?;
    if f != Format::Ssh {
        let (ok, sig, err) = pipe(&prog, &["--status-fd=2", "-bsau", &key], payload)?;
        if !ok
            || !err.contains("\n[GNUPG:] SIG_CREATED ") && !err.starts_with("[GNUPG:] SIG_CREATED ")
        {
            let why = err
                .lines()
                .filter(|l| !l.starts_with("[GNUPG:]"))
                .collect::<Vec<_>>()
                .join("\n");
            return Err(GitError::Other(format!(
                "{why}\ngpg failed to sign the data"
            )));
        }
        return Ok(sig.replace("\r\n", "\n"));
    }
    let literal = key
        .strip_prefix("key::")
        .or_else(|| key.starts_with("ssh-").then_some(key.as_str()));
    let key_file = literal
        .map(|k| Temp::new("signing_key", k.as_bytes()))
        .transpose()?;
    let key_path = match &key_file {
        Some(t) => t.0.to_string_lossy().into_owned(),
        None => expand_home(&key),
    };
    let buffer = Temp::new("signing_buffer", payload)?;
    let buffer_path = buffer.0.to_string_lossy().into_owned();
    let mut args = vec!["-Y", "sign", "-n", "git", "-f", &key_path];
    if key_file.is_some() {
        args.push("-U");
    }
    args.push(&buffer_path);
    let (ok, _, err) = pipe(&prog, &args, b"")?;
    let sig_path = buffer.0.with_file_name(format!(
        "{}.sig",
        buffer.0.file_name().unwrap_or_default().to_string_lossy()
    ));
    let sig = std::fs::read_to_string(&sig_path);
    let _ = std::fs::remove_file(&sig_path);
    if !ok {
        let err = if err.contains("incorrect passphrase supplied to decrypt private key") {
            format!("{err}\nssh-keygen failed to sign the data")
        } else {
            format!("{}\nssh-keygen failed to sign the data", err.trim_end())
        };
        return Err(GitError::Other(err));
    }
    sig.map_err(|e| GitError::Other(format!("failed reading ssh signing data buffer: {e}")))
}

/// Split a tag's buffer into its payload and trailing signature.
pub fn split_tag(data: &[u8]) -> (&[u8], &[u8]) {
    let mut at = data.len();
    let mut pos = 0;
    for line in data.split_inclusive(|b| *b == b'\n') {
        if PGP
            .iter()
            .chain([&X509, &SSH])
            .any(|m| line.starts_with(m.as_bytes()))
        {
            at = pos;
        }
        pos += line.len();
    }
    data.split_at(at)
}

/// Check `signature` over `payload`, as git's check_signature.
pub fn check(
    repo: &Repository,
    payload: &[u8],
    signature: &str,
) -> Result<SignatureCheck, GitError> {
    let f = format_of(signature);
    let prog = program(repo, f);
    let mut c = SignatureCheck {
        result: 'B',
        trust: "undefined".to_owned(),
        payload: payload.to_vec(),
        ..SignatureCheck::default()
    };
    if f == Format::Ssh {
        return check_ssh(repo, &prog, payload, signature, c);
    }
    let sig = Temp::new("vtag", signature.as_bytes())?;
    let sig_path = sig.0.to_string_lossy().into_owned();
    let mut args = vec![];
    if f == Format::OpenPgp {
        args.push("--keyid-format=long");
    }
    args.extend(["--status-fd=1", "--verify", &sig_path, "-"]);
    let (ok, status, output) = pipe(&prog, &args, payload)?;
    c.output = output;
    c.status = status;
    for line in c.status.lines() {
        let Some(line) = line.strip_prefix("[GNUPG:] ") else {
            continue;
        };
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        let letter = match word {
            "GOODSIG" => Some('G'),
            "BADSIG" => Some('B'),
            "ERRSIG" => Some('E'),
            "EXPSIG" => Some('X'),
            "EXPKEYSIG" => Some('Y'),
            "REVKEYSIG" => Some('R'),
            _ => None,
        };
        if let Some(letter) = letter {
            c.result = letter;
            let (key, signer) = rest.split_once(' ').unwrap_or((rest, ""));
            c.key = key.to_owned();
            if letter != 'E' {
                c.signer = signer.to_owned();
            }
        } else if word == "VALIDSIG" {
            let fields: Vec<&str> = rest.split(' ').collect();
            c.fingerprint = fields.first().copied().unwrap_or("").to_owned();
            c.primary_key = fields.get(9).copied().unwrap_or("").to_owned();
        } else if let Some(level) = word.strip_prefix("TRUST_") {
            c.trust = level.to_ascii_lowercase();
        }
    }
    c.good = ok && c.result == 'G';
    Ok(c)
}

fn check_ssh(
    repo: &Repository,
    prog: &str,
    payload: &[u8],
    signature: &str,
    mut c: SignatureCheck,
) -> Result<SignatureCheck, GitError> {
    let allowed = cfg(repo, "gpg.ssh.allowedSignersFile").map(|p| expand_home(&p));
    let Some(allowed) = allowed.filter(|p| std::path::Path::new(p).exists()) else {
        c.output = "error: gpg.ssh.allowedSignersFile needs to be configured and exist for ssh signature verification\n".to_owned();
        c.result = 'N';
        return Ok(c);
    };
    let sig = Temp::new("vtag", signature.as_bytes())?;
    let sig_path = sig.0.to_string_lossy().into_owned();
    let (found, principals, principals_err) = pipe(
        prog,
        &["-Y", "find-principals", "-f", &allowed, "-s", &sig_path],
        b"",
    )?;
    let (mut good, mut out, mut err) = (false, String::new(), String::new());
    if !found || principals.trim().is_empty() {
        let (_, o, e) = pipe(
            prog,
            &["-Y", "check-novalidate", "-n", "git", "-s", &sig_path],
            payload,
        )?;
        (out, err) = (o, e);
    } else {
        let revocation = cfg(repo, "gpg.ssh.revocationFile").map(|p| expand_home(&p));
        for principal in principals.lines().filter(|l| !l.is_empty()) {
            let mut args = vec![
                "-Y", "verify", "-n", "git", "-f", &allowed, "-I", principal, "-s", &sig_path,
            ];
            if let Some(r) = revocation
                .as_deref()
                .filter(|r| std::path::Path::new(r).exists())
            {
                args.extend(["-r", r]);
            }
            let (ok, o, e) = pipe(prog, &args, payload)?;
            good = ok && o.starts_with("Good");
            (out, err) = (o, e);
            if good {
                break;
            }
        }
    }
    let mut output = stripspace(&out);
    output.push_str(&principals_err);
    output.push_str(&stripspace(&err));
    c.output = output.clone();
    c.status = output;
    let first = c.output.lines().next().unwrap_or("");
    c.trust = "never".to_owned();
    let rest = if let Some(rest) = first.strip_prefix("Good \"git\" signature for ") {
        match rest.rfind(" with ") {
            Some(i) => {
                c.result = 'G';
                c.trust = "fully".to_owned();
                c.signer = rest[..i].to_owned();
                Some(&rest[i + 1..])
            }
            None => None,
        }
    } else if let Some(rest) = first.strip_prefix("Good \"git\" signature with ") {
        c.result = 'G';
        c.trust = "undefined".to_owned();
        Some(rest)
    } else {
        None
    };
    if let Some(rest) = rest {
        match rest.find("key ") {
            Some(i) => {
                c.fingerprint = rest[i + 4..].to_owned();
                c.key = c.fingerprint.clone();
            }
            None => c.result = 'B',
        }
    }
    c.good = good && c.result == 'G';
    Ok(c)
}

/// git's strbuf_stripspace without comments: trailing blanks off every line,
/// runs of blank lines squeezed, no leading or trailing blank lines.
fn stripspace(text: &str) -> String {
    let mut out = String::new();
    let mut blank = false;
    for line in text.lines().map(str::trim_end) {
        if line.is_empty() {
            blank = !out.is_empty();
            continue;
        }
        if blank {
            out.push('\n');
            blank = false;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Check the signature of commit `id`; `N` when it has none.
pub fn check_commit(repo: &Repository, id: Oid) -> Result<SignatureCheck, GitError> {
    match repo.extract_signature(&id, None) {
        Ok((sig, data)) => check(repo, &data, &String::from_utf8_lossy(&sig)),
        Err(_) => Ok(SignatureCheck {
            result: 'N',
            trust: "undefined".to_owned(),
            ..SignatureCheck::default()
        }),
    }
}

/// Check the signature of tag object `id`; `N` when it has none.
pub fn check_tag(repo: &Repository, id: Oid) -> Result<SignatureCheck, GitError> {
    let odb = repo.odb()?;
    let obj = odb.read(id)?;
    let (payload, sig) = split_tag(obj.data());
    if sig.is_empty() {
        return Ok(SignatureCheck {
            result: 'N',
            trust: "undefined".to_owned(),
            payload: payload.to_vec(),
            ..SignatureCheck::default()
        });
    }
    check(repo, payload, &String::from_utf8_lossy(sig))
}

/// `Name <email> <secs> <+hhmm>`, as object headers write an identity.
pub(crate) fn ident_line(sig: &git2::Signature) -> String {
    let t = sig.when();
    let off = t.offset_minutes();
    format!(
        "{} <{}> {} {}{:02}{:02}",
        sig.name().unwrap_or(""),
        sig.email().unwrap_or(""),
        t.seconds(),
        if off < 0 { '-' } else { '+' },
        off.abs() / 60,
        off.abs() % 60
    )
}

/// Create a commit, signed with `key` (`Some("")` for the default key) when
/// set, and move `update_ref` to it as `Repository::commit` does.
#[allow(clippy::too_many_arguments)]
pub(crate) fn commit(
    repo: &Repository,
    update_ref: Option<&str>,
    author: &git2::Signature,
    committer: &git2::Signature,
    message: &str,
    tree: &git2::Tree,
    parents: &[&git2::Commit],
    key: Option<&str>,
) -> Result<Oid, GitError> {
    let Some(key) = key else {
        return Ok(repo.commit(update_ref, author, committer, message, tree, parents)?);
    };
    let buf = repo.commit_create_buffer(author, committer, message, tree, parents)?;
    let content = buf.as_str().unwrap_or_default().to_owned();
    let sig = sign_buffer(repo, content.as_bytes(), Some(key))?;
    let id = repo.commit_signed(&content, &sig, None)?;
    if let Some(name) = update_ref {
        let summary = message.lines().next().unwrap_or("");
        let what = if parents.is_empty() {
            "commit (initial)"
        } else {
            "commit"
        };
        let mut target = name.to_owned();
        while let Ok(r) = repo.find_reference(&target) {
            match r.symbolic_target()? {
                Some(t) => target = t.to_owned(),
                None => break,
            }
        }
        repo.reference(&target, id, true, &format!("{what}: {summary}"))?;
    }
    Ok(id)
}

/// [`commit`], signed when commit.gpgSign is set: for the history rgit
/// rewrites itself (absorb, split, squash, reword).
pub(crate) fn commit_configured(
    repo: &Repository,
    update_ref: Option<&str>,
    author: &git2::Signature,
    committer: &git2::Signature,
    message: &str,
    tree: &git2::Tree,
    parents: &[&git2::Commit],
) -> Result<Oid, GitError> {
    let key = commit_key(repo, None, false);
    commit(
        repo,
        update_ref,
        author,
        committer,
        message,
        tree,
        parents,
        key.as_deref(),
    )
}

/// The key to sign commits with: `-S[<key>]` when given, none with
/// `--no-gpg-sign`, else the default key when commit.gpgSign is set.
pub(crate) fn commit_key(repo: &Repository, sign: Option<&str>, no_sign: bool) -> Option<String> {
    match sign {
        _ if no_sign => None,
        Some(k) => Some(k.to_owned()),
        None => config_bool(repo, "commit.gpgsign").then(String::new),
    }
}

/// Write an annotated tag object, signed when `key` is set.
pub(crate) fn tag_object(
    repo: &Repository,
    name: &str,
    target: &git2::Object,
    tagger: &git2::Signature,
    message: &str,
    key: Option<&str>,
) -> Result<Oid, GitError> {
    let kind = target.kind().map_or("commit", |k| k.str());
    let mut buf = format!(
        "object {}\ntype {kind}\ntag {name}\ntagger {}\n\n{message}",
        target.id(),
        ident_line(tagger)
    );
    if let Some(key) = key {
        let sig = sign_buffer(repo, buf.as_bytes(), Some(key))?;
        buf.push_str(&sig);
    }
    Ok(repo.odb()?.write(git2::ObjectType::Tag, buf.as_bytes())?)
}
