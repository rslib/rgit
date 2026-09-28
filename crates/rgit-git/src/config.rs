//! `git config` over libgit2, inside a repository or outside one.

use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};

use git2::{Binding, Config, Repository};

use crate::GitError;

/// Which config file(s) a `config` command reads or writes. `Any` reads every
/// file and writes the repository's own.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConfigScope {
    #[default]
    Any,
    System,
    Global,
    Local,
    Worktree,
    File(PathBuf),
}

/// One config value and where it came from.
#[derive(Debug, Clone)]
pub struct ConfigEntry {
    /// `section.key` or `section.subsection.key`, as git prints it.
    pub name: String,
    /// `None` for a bare `key` line (an implicit true).
    pub value: Option<String>,
    /// system, global, local, worktree or command, as `--show-scope` prints.
    pub scope: &'static str,
    /// The file, relative to the working tree when inside it.
    pub origin: String,
}

/// How `config_set` treats existing values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetMode {
    /// Replace the single value (matching the pattern, if given).
    Replace,
    /// Append another value.
    Add,
    /// Replace every value (matching the pattern, if given) with one.
    ReplaceAll,
}

fn open_repo(git_dir: Option<&Path>) -> Result<Option<Repository>, GitError> {
    Ok(match git_dir {
        Some(dir) => Some(Repository::open(dir)?),
        None => None,
    })
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn system_path() -> Option<PathBuf> {
    let off = std::env::var("GIT_CONFIG_NOSYSTEM")
        .ok()
        .is_some_and(|v| Config::parse_bool(v).unwrap_or(false));
    if off {
        return None;
    }
    env_path("GIT_CONFIG_SYSTEM")
        .or_else(|| Config::find_system().ok())
        .or_else(|| Some(PathBuf::from("/etc/gitconfig")))
}

fn home_gitconfig() -> Result<PathBuf, GitError> {
    env_path("HOME")
        .map(|h| h.join(".gitconfig"))
        .or_else(|| Config::find_global().ok())
        .ok_or_else(|| GitError::Other("no global config file: HOME is not set".to_owned()))
}

/// The global files git reads, lowest priority first.
fn global_paths() -> Vec<(PathBuf, bool)> {
    if let Some(path) = env_path("GIT_CONFIG_GLOBAL") {
        return vec![(path, false)];
    }
    let xdg = env_path("XDG_CONFIG_HOME")
        .or_else(|| env_path("HOME").map(|h| h.join(".config")))
        .map(|d| d.join("git/config"));
    xdg.map(|p| (p, true))
        .into_iter()
        .chain(home_gitconfig().ok().map(|p| (p, false)))
        .collect()
}

fn worktree_config(repo: &Repository) -> Option<PathBuf> {
    let on = Config::open(&repo.commondir().join("config"))
        .and_then(|c| c.get_bool("extensions.worktreeconfig"))
        .unwrap_or(false);
    on.then(|| repo.path().join("config.worktree"))
}

fn no_repo() -> GitError {
    GitError::Other("not in a git directory".to_owned())
}

/// The file `scope` writes to.
pub fn config_file(git_dir: Option<&Path>, scope: &ConfigScope) -> Result<PathBuf, GitError> {
    let repo = open_repo(git_dir)?;
    write_path(repo.as_ref(), scope)
}

fn write_path(repo: Option<&Repository>, scope: &ConfigScope) -> Result<PathBuf, GitError> {
    Ok(match scope {
        ConfigScope::System => system_path().ok_or_else(|| {
            GitError::Other("the system config is disabled (GIT_CONFIG_NOSYSTEM)".to_owned())
        })?,
        ConfigScope::Global => {
            if let Some(path) = env_path("GIT_CONFIG_GLOBAL") {
                return Ok(path);
            }
            // git writes the XDG file only when it exists and ~/.gitconfig does not.
            let home = home_gitconfig()?;
            match global_paths()
                .into_iter()
                .find(|(p, xdg)| *xdg && p.exists())
            {
                Some((xdg, _)) if !home.exists() => xdg,
                _ => home,
            }
        }
        ConfigScope::File(path) => path.clone(),
        ConfigScope::Worktree => {
            let repo = repo.ok_or_else(no_repo)?;
            worktree_config(repo).unwrap_or_else(|| repo.commondir().join("config"))
        }
        ConfigScope::Local | ConfigScope::Any => {
            repo.ok_or_else(no_repo)?.commondir().join("config")
        }
    })
}

/// The files `scope` reads, lowest priority first, with their libgit2 level.
fn read_paths(
    repo: Option<&Repository>,
    scope: &ConfigScope,
) -> Result<Vec<(PathBuf, libgit2_sys::git_config_level_t)>, GitError> {
    use libgit2_sys as raw;
    let local = || repo.map(|r| r.commondir().join("config"));
    Ok(match scope {
        ConfigScope::Any => {
            let mut out: Vec<_> = system_path()
                .map(|p| (p, raw::GIT_CONFIG_LEVEL_SYSTEM))
                .into_iter()
                .collect();
            for (p, xdg) in global_paths() {
                let level = if xdg {
                    raw::GIT_CONFIG_LEVEL_XDG
                } else {
                    raw::GIT_CONFIG_LEVEL_GLOBAL
                };
                out.push((p, level));
            }
            out.extend(local().map(|p| (p, raw::GIT_CONFIG_LEVEL_LOCAL)));
            out.extend(
                repo.and_then(worktree_config)
                    .map(|p| (p, raw::GIT_CONFIG_LEVEL_WORKTREE)),
            );
            out
        }
        ConfigScope::Global => global_paths()
            .into_iter()
            .map(|(p, _)| (p, raw::GIT_CONFIG_LEVEL_GLOBAL))
            .collect(),
        ConfigScope::Local => vec![(local().ok_or_else(no_repo)?, raw::GIT_CONFIG_LEVEL_LOCAL)],
        ConfigScope::System => vec![(write_path(repo, scope)?, raw::GIT_CONFIG_LEVEL_SYSTEM)],
        ConfigScope::Worktree => vec![(write_path(repo, scope)?, raw::GIT_CONFIG_LEVEL_WORKTREE)],
        ConfigScope::File(p) => vec![(p.clone(), raw::GIT_CONFIG_LEVEL_APP)],
    })
}

/// Add one file to `config` at `level`. With the repository, libgit2 can
/// evaluate `includeIf.gitdir:`/`onbranch:` conditions.
fn add_file(
    config: &Config,
    path: &Path,
    level: libgit2_sys::git_config_level_t,
    repo: Option<&Repository>,
) -> Result<(), GitError> {
    let cpath = CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| GitError::Other(format!("bad path {}", path.display())))?;
    let repo_ptr = repo.map_or(std::ptr::null(), |r| r.raw() as *const _);
    // SAFETY: both handles are live; libgit2 copies the path.
    let rc = unsafe {
        libgit2_sys::git_config_add_file_ondisk(config.raw(), cpath.as_ptr(), level, repo_ptr, 0)
    };
    if rc < 0 {
        return Err(git2::Error::last_error(rc).into());
    }
    Ok(())
}

/// The config `scope` means as one libgit2 config: the file it writes, or
/// every file it reads (git's stacking, honouring GIT_CONFIG_GLOBAL,
/// GIT_CONFIG_SYSTEM and GIT_CONFIG_NOSYSTEM, which libgit2 ignores).
pub(crate) fn open_config(
    repo: &Repository,
    scope: ConfigScope,
    write: bool,
) -> Result<Config, GitError> {
    if write {
        return open_for_write(Some(repo), &scope);
    }
    let config = Config::new()?;
    for (path, level) in read_paths(Some(repo), &scope)? {
        if path.exists() {
            add_file(&config, &path, level, Some(repo))?;
        }
    }
    Ok(config)
}

/// Every entry `scope` sees, in git's order (lowest priority first). Entries
/// from included files are kept only with `includes`.
pub fn config_list(
    git_dir: Option<&Path>,
    scope: &ConfigScope,
    includes: bool,
) -> Result<Vec<ConfigEntry>, GitError> {
    use libgit2_sys as raw;
    let repo = open_repo(git_dir)?;
    let files = read_paths(repo.as_ref(), scope)?;
    let single = files.len() == 1 && *scope != ConfigScope::Any;
    if let [(path, _)] = files.as_slice()
        && single
        && !path.exists()
    {
        return Err(GitError::Other(format!(
            "unable to read config file '{}': No such file or directory",
            path.display()
        )));
    }
    let workdir = repo
        .as_ref()
        .and_then(|r| r.workdir().map(Path::to_path_buf));
    let mut out = Vec::new();
    // One file at a time keeps git's order: each file, then what it includes.
    for (path, level) in files.iter().filter(|(p, _)| p.exists()) {
        let config = Config::new()?;
        add_file(&config, path, *level, repo.as_ref())?;
        let mut entries = config.entries(None)?;
        let mut file = Vec::new();
        while let Some(entry) = entries.next() {
            let entry = entry?;
            if entry.include_depth() > 0 && !includes {
                continue;
            }
            // SAFETY: the entry is live for this iteration step.
            let origin = unsafe {
                let p = (*entry.raw()).origin_path;
                if p.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(p).to_string_lossy().into_owned()
                }
            };
            // git names the repository's files from `.git`, the rest in full.
            let local = matches!(
                *level,
                raw::GIT_CONFIG_LEVEL_LOCAL | raw::GIT_CONFIG_LEVEL_WORKTREE
            );
            let origin = match &workdir {
                Some(w) if local => Path::new(&origin)
                    .strip_prefix(w)
                    .map_or(origin.clone(), |p| p.display().to_string()),
                _ => origin,
            };
            file.push(ConfigEntry {
                name: String::from_utf8_lossy(entry.name_bytes()).into_owned(),
                value: entry
                    .has_value()
                    .then(|| String::from_utf8_lossy(entry.value_bytes()).into_owned()),
                scope: match *level {
                    raw::GIT_CONFIG_LEVEL_SYSTEM => "system",
                    raw::GIT_CONFIG_LEVEL_XDG | raw::GIT_CONFIG_LEVEL_GLOBAL => "global",
                    raw::GIT_CONFIG_LEVEL_LOCAL => "local",
                    raw::GIT_CONFIG_LEVEL_WORKTREE => "worktree",
                    _ => "command",
                },
                origin,
            });
        }
        out.extend(file);
    }
    Ok(out)
}

/// `key` as libgit2 names it: section and variable lowercased, subsection kept.
pub fn config_key(key: &str) -> Result<String, GitError> {
    let (Some((section, rest)), Some((_, var))) = (key.split_once('.'), key.rsplit_once('.'))
    else {
        return Err(GitError::Other(format!(
            "key does not contain a section: {key}"
        )));
    };
    if var.is_empty() || section.is_empty() {
        return Err(GitError::Other(format!("invalid key: {key}")));
    }
    let sub = &rest[..rest.len() - var.len()];
    Ok(format!(
        "{}.{sub}{}",
        section.to_lowercase(),
        var.to_lowercase()
    ))
}

fn open_for_write(repo: Option<&Repository>, scope: &ConfigScope) -> Result<Config, GitError> {
    let path = write_path(repo, scope)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    Ok(Config::open(&path)?)
}

/// Set `key` to `value`. `pattern` (a regex; `!` negates) limits which existing
/// values are replaced.
pub fn config_set(
    git_dir: Option<&Path>,
    scope: &ConfigScope,
    key: &str,
    value: &str,
    pattern: Option<&str>,
    mode: SetMode,
) -> Result<(), GitError> {
    let repo = open_repo(git_dir)?;
    let mut config = open_for_write(repo.as_ref(), scope)?;
    let key = config_key(key)?;
    match (mode, pattern) {
        (SetMode::Add, _) => config.set_multivar(&key, "$^", value)?,
        (SetMode::Replace, None) => config.set_str(&key, value)?,
        (SetMode::Replace, Some(p)) => {
            let hits = matching(&config, &key, Some(p))?;
            match hits.len() {
                0 => config.set_multivar(&key, "$^", value)?,
                1 => config.set_multivar(&key, &config_fixed_value(&hits[0]), value)?,
                _ => {
                    return Err(GitError::Other(format!(
                        "{key} has multiple values matching; use --replace-all"
                    )));
                }
            }
        }
        (SetMode::ReplaceAll, p) => {
            // The first match is replaced in place, the rest removed, as git does.
            let hits = matching(&config, &key, p)?;
            let first = hits
                .first()
                .map_or("$^".to_owned(), |v| config_fixed_value(v));
            for v in hits.iter().skip(1).filter(|v| **v != hits[0]) {
                config.remove_multivar(&key, &config_fixed_value(v))?;
            }
            config.set_multivar(&key, &first, value)?;
        }
    }
    Ok(())
}

/// Remove `key`; `all` removes every value (matching `pattern`, if given).
pub fn config_unset(
    git_dir: Option<&Path>,
    scope: &ConfigScope,
    key: &str,
    pattern: Option<&str>,
    all: bool,
) -> Result<(), GitError> {
    let repo = open_repo(git_dir)?;
    let mut config = open_for_write(repo.as_ref(), scope)?;
    let key = config_key(key)?;
    let hits = matching(&config, &key, pattern)?;
    if hits.is_empty() {
        return Err(GitError::Other(format!("{key} is not set")));
    }
    if hits.len() > 1 && !all {
        return Err(GitError::Other(format!(
            "{key} has multiple values; use --unset-all"
        )));
    }
    for v in hits {
        config.remove_multivar(&key, &config_fixed_value(&v))?;
    }
    Ok(())
}

/// The values of `key` in `config` that match `pattern`.
fn matching(config: &Config, key: &str, pattern: Option<&str>) -> Result<Vec<String>, GitError> {
    let test = value_matcher(pattern)?;
    let mut out = Vec::new();
    match config.multivar(key, None) {
        Ok(entries) => entries.for_each(|e| {
            let v = String::from_utf8_lossy(e.value_bytes()).into_owned();
            if test(&v) {
                out.push(v);
            }
        })?,
        Err(e) if e.code() == git2::ErrorCode::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(out)
}

/// A test for git's value-pattern: a regex, negated by a leading `!`.
pub fn value_matcher(pattern: Option<&str>) -> Result<impl Fn(&str) -> bool + use<>, GitError> {
    let (negate, re) = match pattern {
        Some(p) => {
            let (negate, p) = p.strip_prefix('!').map_or((false, p), |p| (true, p));
            let re = regex::Regex::new(p)
                .map_err(|e| GitError::Other(format!("invalid pattern {p:?}: {e}")))?;
            (negate, Some(re))
        }
        None => (false, None),
    };
    Ok(move |v: &str| re.as_ref().is_none_or(|re| re.is_match(v) != negate))
}

/// A value pattern matching exactly `value` (git's `--fixed-value`).
pub fn config_fixed_value(value: &str) -> String {
    format!("^{}$", regex::escape(value))
}

/// A test for `--get-regexp`'s name regex. Like git, the section and variable
/// parts of the pattern are lowercased.
pub fn config_name_matcher(pattern: &str) -> Result<impl Fn(&str) -> bool + use<>, GitError> {
    let pat = match (pattern.find('.'), pattern.rfind('.')) {
        (Some(i), Some(j)) => format!(
            "{}{}{}",
            pattern[..i].to_lowercase(),
            &pattern[i..j],
            pattern[j..].to_lowercase()
        ),
        _ => pattern.to_lowercase(),
    };
    let re = regex::Regex::new(&pat)
        .map_err(|e| GitError::Other(format!("invalid key pattern: {e}")))?;
    Ok(move |name: &str| re.is_match(name))
}

/// Rename section `old` to `new`, or remove it when `new` is `None`. Section
/// names are `section` or `section.subsection`.
pub fn config_section(
    git_dir: Option<&Path>,
    scope: &ConfigScope,
    old: &str,
    new: Option<&str>,
) -> Result<(), GitError> {
    let repo = open_repo(git_dir)?;
    let path = write_path(repo.as_ref(), scope)?;
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let header = |name: &str| match name.split_once('.') {
        Some((s, sub)) => format!(
            "[{s} \"{}\"]",
            sub.replace('\\', "\\\\").replace('"', "\\\"")
        ),
        None => format!("[{name}]"),
    };
    let mut out = String::new();
    let mut inside = false;
    let mut found = false;
    for line in text.split_inclusive('\n') {
        let t = line.trim_start();
        if t.starts_with('[') {
            inside = section_name(t).is_some_and(|n| same_section(&n, old));
            found |= inside;
            if inside {
                match new {
                    Some(new) => {
                        // Keep anything after the header (a key on the same line).
                        let rest = t.find(']').map_or("", |i| &t[i + 1..]);
                        out.push_str(&header(new));
                        out.push_str(if rest.trim().is_empty() { "\n" } else { rest });
                        inside = false;
                    }
                    None => continue,
                }
                continue;
            }
        }
        if !inside {
            out.push_str(line);
        }
    }
    if !found {
        return Err(GitError::Other(format!("no such section: {old}")));
    }
    std::fs::write(&path, out)?;
    Ok(())
}

/// `section` or `section.subsection` of a `[...]` header line.
fn section_name(line: &str) -> Option<String> {
    let inner = &line[1..line.find(']')?];
    Some(match inner.split_once([' ', '\t']) {
        Some((s, sub)) => {
            let sub = sub.trim().trim_matches('"');
            format!("{s}.{}", sub.replace("\\\"", "\"").replace("\\\\", "\\"))
        }
        // The old `[section.subsection]` form.
        None => inner.to_owned(),
    })
}

fn same_section(a: &str, b: &str) -> bool {
    let (sa, suba) = a.split_once('.').unwrap_or((a, ""));
    let (sb, subb) = b.split_once('.').unwrap_or((b, ""));
    sa.eq_ignore_ascii_case(sb) && suba == subb
}

/// `value` converted as git's `--type=<kind>` prints it.
pub fn config_typed(value: Option<&str>, kind: &str) -> Result<String, GitError> {
    let bad = |what: &str| {
        GitError::Other(format!(
            "bad {what} config value '{}'",
            value.unwrap_or_default()
        ))
    };
    // A bare `key` line is an implicit true.
    let Some(value) = value else {
        return Ok(match kind {
            "bool" | "bool-or-int" => "true".to_owned(),
            _ => String::new(),
        });
    };
    Ok(match kind {
        "bool" => Config::parse_bool(value)
            .map_err(|_| bad("boolean"))?
            .to_string(),
        "int" => Config::parse_i64(value)
            .map_err(|_| bad("numeric"))?
            .to_string(),
        "bool-or-int" => match Config::parse_i64(value) {
            Ok(n) => n.to_string(),
            Err(_) => Config::parse_bool(value)
                .map_err(|_| bad("boolean"))?
                .to_string(),
        },
        "path" => match value.strip_prefix("~/") {
            Some(rest) => env_path("HOME")
                .ok_or_else(|| bad("path"))?
                .join(rest)
                .display()
                .to_string(),
            None => value.to_owned(),
        },
        "expiry-date" => expiry_date(value)
            .ok_or_else(|| bad("expiry-date"))?
            .to_string(),
        "color" => ansi_color(value).ok_or_else(|| bad("color"))?,
        _ => value.to_owned(),
    })
}

/// A git expiry date as a unix timestamp: `now`, `never`, `<n>.<unit>.ago`,
/// `<n> <unit> ago`, a timestamp, or `YYYY-MM-DD[ HH:MM:SS]`.
pub fn expiry_date(value: &str) -> Option<i64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let v = value.trim().to_lowercase();
    match v.as_str() {
        "now" | "all" => return Some(now),
        "never" | "false" => return Some(0),
        _ => {}
    }
    if let Ok(n) = v.parse::<i64>() {
        return Some(n);
    }
    let words: Vec<&str> = v.split(['.', ' ']).filter(|w| !w.is_empty()).collect();
    if let [n, unit, "ago"] = words.as_slice() {
        let n: i64 = n.parse().ok()?;
        let unit = unit.trim_end_matches('s');
        let secs = match unit {
            "second" | "sec" => 1,
            "minute" | "min" => 60,
            "hour" => 3600,
            "day" => 86400,
            "week" => 7 * 86400,
            "month" => 30 * 86400,
            "year" => 365 * 86400,
            _ => return None,
        };
        return Some(now - n * secs);
    }
    civil_seconds(&v)
}

/// `YYYY-MM-DD[ HH:MM[:SS]]` (UTC) as a unix timestamp.
fn civil_seconds(s: &str) -> Option<i64> {
    let (date, time) = s.split_once(['t', ' ']).unwrap_or((s, ""));
    let mut dp = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (dp.next()??, dp.next()??, dp.next()??);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // days_from_civil (Howard Hinnant, public domain).
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let mut secs = (era * 146097 + doe - 719468) * 86400;
    let mut tp = time.split(':').filter(|p| !p.is_empty());
    for mult in [3600, 60, 1] {
        if let Some(p) = tp.next() {
            secs += p.parse::<i64>().ok()? * mult;
        }
    }
    Some(secs)
}

/// A git color spec (`bold red blue`, `#ff0000`, `208`, `reset`) as its ANSI
/// escape.
pub fn ansi_color(spec: &str) -> Option<String> {
    let mut attrs = Vec::new();
    let mut colors: Vec<String> = Vec::new();
    for word in spec.split_whitespace() {
        let w = word.to_lowercase();
        if w == "reset" {
            attrs.push("".to_owned());
            continue;
        }
        let attr = |name: &str| -> Option<u8> {
            Some(match name {
                "bold" => 1,
                "dim" => 2,
                "italic" => 3,
                "ul" => 4,
                "blink" => 5,
                "reverse" => 7,
                "strike" => 9,
                _ => return None,
            })
        };
        if let Some(a) = attr(&w) {
            attrs.push(a.to_string());
            continue;
        }
        if let Some(a) = w
            .strip_prefix("no-")
            .or_else(|| w.strip_prefix("no"))
            .and_then(attr)
        {
            attrs.push((if a == 1 { 22 } else { 20 + a }).to_string());
            continue;
        }
        if colors.len() == 2 {
            return None;
        }
        let bg = !colors.is_empty();
        let base = |n: u8| (n + if bg { 10 } else { 0 }).to_string();
        const NAMES: [&str; 8] = [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ];
        let code = if w == "normal" {
            String::new()
        } else if w == "default" {
            base(39)
        } else if let Some(i) = NAMES.iter().position(|n| *n == w) {
            base(30 + i as u8)
        } else if let Some(i) = w
            .strip_prefix("bright")
            .and_then(|n| NAMES.iter().position(|c| *c == n))
        {
            base(90 + i as u8)
        } else if let Some(hex) = w.strip_prefix('#').filter(|h| h.len() == 6) {
            let c = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
            format!(
                "{};2;{};{};{}",
                if bg { 48 } else { 38 },
                c(0)?,
                c(2)?,
                c(4)?
            )
        } else if let Ok(n) = w.parse::<i32>() {
            match n {
                -1 => String::new(),
                0..=7 => base(30 + n as u8),
                8..=15 => base(90 + n as u8 - 8),
                16..=255 => format!("{};5;{n}", if bg { 48 } else { 38 }),
                _ => return None,
            }
        } else {
            return None;
        };
        colors.push(code);
    }
    let parts: Vec<String> = attrs
        .into_iter()
        .chain(colors)
        .filter(|p| !p.is_empty())
        .collect();
    Some(if spec.trim().is_empty() {
        String::new()
    } else {
        format!("\x1b[{}m", parts.join(";"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_match_git() {
        assert_eq!(ansi_color("bold red blue").unwrap(), "\x1b[1;31;44m");
        assert_eq!(ansi_color("reset").unwrap(), "\x1b[m");
        assert_eq!(ansi_color("208").unwrap(), "\x1b[38;5;208m");
        assert_eq!(ansi_color("#ff0000 ul").unwrap(), "\x1b[4;38;2;255;0;0m");
        assert!(ansi_color("nocolor").is_none());
    }

    #[test]
    fn keys_normalize_like_git() {
        assert_eq!(config_key("Core.AutoCRLF").unwrap(), "core.autocrlf");
        assert_eq!(config_key("Remote.Up.URL").unwrap(), "remote.Up.url");
        assert!(config_key("nodot").is_err());
    }
}
