//! Stable change identity, jj/Gerrit style. Every commit rgit creates carries a
//! `Change-Id: I<40 hex>` trailer that is generated once and then preserved
//! across amend and rebase, so a logical change keeps one identity even as its
//! commit oid changes. Plain git ignores the trailer, so this stays fully
//! git-safe: it is just extra lines in the commit message.

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
pub fn ensure(message: &str) -> String {
    match extract(message) {
        Some(_) => message.to_owned(),
        None => append(message, &generate()),
    }
}

/// The new message with a change id, carrying the old commit's id forward when
/// the new message does not already carry one (so amend keeps the identity).
pub fn preserve(old_message: &str, new_message: &str) -> String {
    if extract(new_message).is_some() {
        return new_message.to_owned();
    }
    let id = extract(old_message).unwrap_or_else(generate);
    append(new_message, &id)
}

/// Append a `Change-Id` trailer. Joins an existing trailer block (a final
/// paragraph whose lines are all `Key: value`) rather than opening a new one.
fn append(message: &str, id: &str) -> String {
    let trailer = format!("{KEY} {id}");
    let body = message.trim_end();
    if body.is_empty() {
        return trailer;
    }
    let last_para = body.rsplit("\n\n").next().unwrap_or("");
    if is_trailer_block(last_para) {
        format!("{body}\n{trailer}\n")
    } else {
        format!("{body}\n\n{trailer}\n")
    }
}

/// Whether every non-empty line of `para` looks like a git trailer (`Token: …`),
/// so a new trailer joins it instead of starting a fresh block.
fn is_trailer_block(para: &str) -> bool {
    let mut any = false;
    for line in para.lines() {
        if line.trim().is_empty() {
            continue;
        }
        any = true;
        let Some((key, _)) = line.split_once(':') else {
            return false;
        };
        if key.is_empty()
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return false;
        }
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

    #[test]
    fn ensure_adds_a_trailer_then_is_idempotent() {
        let once = ensure("fix the thing");
        assert!(once.starts_with("fix the thing\n\nChange-Id: I"));
        assert_eq!(ensure(&once), once);
        assert_eq!(extract(&once), extract(&ensure(&once)));
    }

    #[test]
    fn ensure_joins_an_existing_trailer_block() {
        let msg = "subject\n\nbody line\n\nSigned-off-by: Dev <d@e.f>";
        let out = ensure(msg);
        // The Change-Id joins the trailer paragraph, no blank line before it.
        assert!(out.contains("Signed-off-by: Dev <d@e.f>\nChange-Id: I"));
    }

    #[test]
    fn preserve_carries_the_old_id_when_the_new_message_lacks_one() {
        let old = ensure("original");
        let id = extract(&old).unwrap();
        let out = preserve(&old, "reworded");
        assert_eq!(extract(&out).as_deref(), Some(id.as_str()));
    }

    #[test]
    fn preserve_keeps_an_explicit_new_id() {
        let old = ensure("original");
        let new = ensure("different subject");
        let out = preserve(&old, &new);
        assert_eq!(extract(&out), extract(&new));
        assert_ne!(extract(&out), extract(&old));
    }
}
