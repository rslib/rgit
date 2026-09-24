//! Stable change identity, jj/Gerrit style. Opt-in with `git config
//! rgit.changeId true`: commits rgit creates then carry a `Change-Id: I<40 hex>`
//! trailer that is generated once and kept across amend and rebase, so a
//! logical change keeps one identity even as its commit oid changes. Off by
//! default, rgit adds no trailer, but an id a commit already has is still kept
//! when rgit rewrites it.

use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

const KEY: &str = "Change-Id:";

/// A fresh change id: `I` followed by 40 hex chars. Derived by hashing local
/// entropy (time, pid, a process-local counter, and the OS-seeded RandomState)
/// so two ids differ even within the same nanosecond; the id namespace is
/// per-developer and a collision is both astronomically unlikely and harmless.
pub fn generate() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(n);
    hasher.write_u128(nanos);
    let rand = hasher.finish();

    let mut seed = Vec::with_capacity(40);
    seed.extend_from_slice(&nanos.to_le_bytes());
    seed.extend_from_slice(&n.to_le_bytes());
    seed.extend_from_slice(&rand.to_le_bytes());
    seed.extend_from_slice(&u64::from(std::process::id()).to_le_bytes());
    let oid = git2::Oid::hash_object(git2::ObjectType::Blob, &seed)
        .expect("hashing fixed-size bytes cannot fail");
    format!("I{oid}")
}

/// The change id recorded in a commit message, if any.
pub fn extract(message: &str) -> Option<String> {
    message.lines().find_map(|line| {
        let value = line.strip_prefix(KEY)?.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

/// The message with a change-id trailer guaranteed present: unchanged if it
/// already has one, else a freshly generated id appended.
/// Whether new commits get a Change-Id: the `rgit.changeId` git config, off
/// by default.
pub fn enabled(repo: &git2::Repository) -> bool {
    repo.config()
        .and_then(|c| c.get_bool("rgit.changeId"))
        .unwrap_or(false)
}

/// `message` with a fresh Change-Id appended when enabled and none is present.
pub fn ensure(repo: &git2::Repository, message: &str) -> String {
    match extract(message) {
        None if enabled(repo) => append(message, &generate()),
        _ => message.to_owned(),
    }
}

/// The new message with a change id, carrying the old commit's id forward when
/// the new message does not already carry one (so amend keeps the identity).
/// `new_message` carrying `old_message`'s Change-Id, or a fresh one when
/// enabled and the old message had none.
pub fn preserve(repo: &git2::Repository, old_message: &str, new_message: &str) -> String {
    if extract(new_message).is_some() {
        return new_message.to_owned();
    }
    match extract(old_message) {
        Some(id) => append(new_message, &id),
        None if enabled(repo) => append(new_message, &generate()),
        None => new_message.to_owned(),
    }
}

/// Append a `Change-Id` trailer, git interpret-trailers style: it joins an
/// existing trailer block (a final paragraph, never the subject, whose lines
/// are all `Key: value`) and is otherwise set off by one blank line.
fn append(message: &str, id: &str) -> String {
    let trailer = format!("{KEY} {id}");
    let body = message.trim_end();
    if body.is_empty() {
        return trailer;
    }
    let lines: Vec<&str> = body.trim_start().lines().collect();
    let joins = lines
        .iter()
        .rposition(|l| l.trim().is_empty())
        .is_some_and(|blank| is_trailer_block(&lines[blank + 1..]));
    let sep = if joins { "\n" } else { "\n\n" };
    format!("{body}{sep}{trailer}\n")
}

/// Whether every line of a paragraph is a git trailer (`Token: value`, with
/// indented continuation lines), so a new trailer joins it.
fn is_trailer_block(para: &[&str]) -> bool {
    let mut any = false;
    for line in para {
        if line.starts_with([' ', '\t']) && any {
            continue;
        }
        let Some((key, _)) = line.split_once(':') else {
            return false;
        };
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return false;
        }
        any = true;
    }
    any
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_well_formed_and_unique() {
        let a = generate();
        let b = generate();
        assert_ne!(a, b);
        assert!(a.starts_with('I'));
        assert_eq!(a.len(), 41);
        assert!(a[1..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// A throwaway repo with `rgit.changeId` set to `on`.
    fn repo(name: &str, on: bool) -> git2::Repository {
        let dir =
            std::env::temp_dir().join(format!("rgit-changeid-unit-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = git2::Repository::init(&dir).unwrap();
        repo.config()
            .unwrap()
            .set_bool("rgit.changeId", on)
            .unwrap();
        repo
    }

    #[test]
    fn off_by_default_adds_nothing() {
        let dir =
            std::env::temp_dir().join(format!("rgit-changeid-unit-{}-default", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = git2::Repository::init(&dir).unwrap();
        assert!(!enabled(&repo));
        assert_eq!(ensure(&repo, "fix: x"), "fix: x");
        assert_eq!(preserve(&repo, "old", "new"), "new");
    }

    #[test]
    fn off_still_carries_an_existing_id() {
        let off = repo("off-carry", false);
        let old = append("original", "I1");
        assert_eq!(
            extract(&preserve(&off, &old, "reworded")).as_deref(),
            Some("I1")
        );
    }

    #[test]
    fn ensure_adds_a_trailer_then_is_idempotent() {
        let on = repo("ensure", true);
        let once = ensure(&on, "fix the thing");
        assert!(once.starts_with("fix the thing\n\nChange-Id: I"));
        assert_eq!(ensure(&on, &once), once);
    }

    #[test]
    fn ensure_joins_an_existing_trailer_block() {
        let on = repo("join", true);
        let msg = "subject\n\nbody line\n\nSigned-off-by: Dev <d@e.f>";
        let out = ensure(&on, msg);
        assert!(out.contains("Signed-off-by: Dev <d@e.f>\nChange-Id: I"));
    }

    #[test]
    fn append_separates_or_joins_like_interpret_trailers() {
        let id = "I0";
        let cases = [
            ("subject", "subject\n\nChange-Id: I0\n"),
            ("subject\n\n\n", "subject\n\nChange-Id: I0\n"),
            ("fix: a bug", "fix: a bug\n\nChange-Id: I0\n"),
            ("feat: x\n", "feat: x\n\nChange-Id: I0\n"),
            ("\n\nfeat: x", "\n\nfeat: x\n\nChange-Id: I0\n"),
            ("subject\n\nbody\n", "subject\n\nbody\n\nChange-Id: I0\n"),
            (
                "fix: x\n\nNote: prose\nmore",
                "fix: x\n\nNote: prose\nmore\n\nChange-Id: I0\n",
            ),
            (
                "subject\n\nSigned-off-by: A <a@b>\n\n",
                "subject\n\nSigned-off-by: A <a@b>\nChange-Id: I0\n",
            ),
            (
                "fix: x\n\nbody\n\nCo-authored-by: A\n  <a@b>\nFixes: #1",
                "fix: x\n\nbody\n\nCo-authored-by: A\n  <a@b>\nFixes: #1\nChange-Id: I0\n",
            ),
            (
                "subject\n \nSigned-off-by: A",
                "subject\n \nSigned-off-by: A\nChange-Id: I0\n",
            ),
        ];
        for (msg, want) in cases {
            assert_eq!(append(msg, id), want, "message {msg:?}");
        }
    }

    #[test]
    fn preserve_carries_the_old_id_when_the_new_message_lacks_one() {
        let on = repo("carry", true);
        let old = ensure(&on, "original");
        let id = extract(&old).unwrap();
        let out = preserve(&on, &old, "reworded");
        assert_eq!(extract(&out).as_deref(), Some(id.as_str()));
    }

    #[test]
    fn preserve_keeps_an_explicit_new_id() {
        let on = repo("explicit", true);
        let old = ensure(&on, "original");
        let new = ensure(&on, "different subject");
        let out = preserve(&on, &old, &new);
        assert_eq!(extract(&out), extract(&new));
        assert_ne!(extract(&out), extract(&old));
    }
}
