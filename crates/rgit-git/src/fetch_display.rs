//! `git fetch`'s report: the `From` line and the aligned ref lines, laid out
//! as git's fetch.c does.

/// One reported ref: git's flag column, the summary, the remote and local
/// names (already shortened) and a trailing note.
pub(crate) struct Row {
    pub code: char,
    pub summary: String,
    pub remote: String,
    pub local: String,
    pub error: Option<String>,
    /// Whether the row widens the name column (not for `FETCH_HEAD` or
    /// pruned refs, which git leaves out of the measure).
    pub counted: bool,
}

/// `url` as git shows it after `From`: without credentials, a trailing `/`
/// or `.git`.
pub(crate) fn display_url(url: &str) -> String {
    let url = anonymize(url);
    let trimmed = url.trim_end_matches('/');
    let trimmed = match trimmed.strip_suffix(".git") {
        Some(t) if trimmed.len() > 5 => t,
        _ => trimmed,
    };
    trimmed.to_owned()
}

/// git's transport_anonymize_url: drop `user[:pass]@` from a URL.
fn anonymize(url: &str) -> String {
    let local = url.find(':').is_none_or(|c| url[..c].contains('/'));
    let Some(at) = url.find('@').filter(|_| !local) else {
        return url.to_owned();
    };
    let rest = &url[at + 1..];
    match url.find("://") {
        None if !rest.contains(':') => url.to_owned(),
        None => rest.to_owned(),
        Some(scheme) => {
            let ok = url[..scheme]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c));
            let slash = url[scheme + 3..].find('/').map(|i| i + scheme + 3);
            if !ok || slash.is_some_and(|s| s < at) {
                url.to_owned()
            } else {
                format!("{}{rest}", &url[..scheme + 3])
            }
        }
    }
}

/// The report for `rows` fetched from `url`: nothing when there are none.
pub(crate) fn render(url: &str, rows: &[Row], compact: bool) -> Vec<String> {
    if rows.is_empty() {
        return Vec::new();
    }
    // Like git, lines too long for the terminal do not widen the column.
    let mut max = std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .filter(|&c| c > 0)
        .unwrap_or(80);
    if compact {
        max = max * 2 / 3;
    }
    let mut width = 10;
    for r in rows.iter().filter(|r| r.counted) {
        let (rlen, llen) = (r.remote.chars().count(), r.local.chars().count());
        let llen = if compact { 0 } else { llen };
        if 21 + rlen + 4 + llen < max {
            width = width.max(rlen);
        }
    }
    let mut out = vec![format!("From {}", display_url(url))];
    for r in rows {
        let (remote, local) = if !compact {
            (r.remote.clone(), r.local.clone())
        } else if r.remote == r.local {
            (r.remote.clone(), "*".to_owned())
        } else {
            match star(&r.remote, &r.local) {
                Some(remote) => (remote, r.local.clone()),
                None => (
                    r.remote.clone(),
                    star(&r.local, &r.remote).unwrap_or_else(|| r.local.clone()),
                ),
            }
        };
        let mut line = format!(" {} {:<17} {remote:<width$} -> {local}", r.code, r.summary);
        if let Some(e) = &r.error {
            line.push_str(&format!("  ({e})"));
        }
        out.push(line);
    }
    out
}

/// git's find_and_replace: `needle` in `hay` as whole path components,
/// replaced by `*`.
fn star(hay: &str, needle: &str) -> Option<String> {
    let at = if hay.ends_with(needle) {
        hay.len() - needle.len()
    } else {
        hay.find(needle)?
    };
    if at > 0 && !hay[..at].ends_with('/') {
        return None;
    }
    let end = at + needle.len();
    if end < hay.len() && !hay[end..].starts_with('/') {
        return None;
    }
    Some(format!("{}*{}", &hay[..at], &hay[end..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_compact_names_follow_git() {
        assert_eq!(display_url("/srv/r.git/"), "/srv/r");
        assert_eq!(display_url("https://u:p@host/x.git"), "https://host/x");
        assert_eq!(display_url("git@host:x/y.git"), "host:x/y");
        assert_eq!(star("origin/main", "main").as_deref(), Some("origin/*"));
        assert_eq!(star("origin/xmain", "main"), None);
    }
}
