//! Built-in branching workflows. The user picks a preset (gitflow, github,
//! gitlab, trunk, release-flow) once; a single verb set (start / finish /
//! release) then behaves per that preset's policy. The active preset is stored
//! in git config under `rgit.workflow`, so nothing is forced: ignore these and
//! rgit is still a plain git tool. Lives in the backend so the CLI and TUI share
//! it.

use crate::backend::GitBackend;
use crate::error::GitError;

fn err(msg: impl Into<String>) -> GitError {
    GitError::Other(msg.into())
}

/// The policy a preset expands into. Verbs read this table; only the data
/// differs between workflows.
struct Policy {
    preset: &'static str,
    main: String,
    integration: Option<String>,
    feature_prefix: &'static str,
    release_prefix: &'static str,
    feature_base: String,
    release_base: String,
    release_merges_to: Vec<String>,
    tag_on_release: bool,
    delete_on_finish: bool,
    pull_request: bool,
    upstream_first: bool,
}

pub(crate) fn detect_main(backend: &dyn GitBackend) -> String {
    if backend.branch_exists("main") {
        "main".to_owned()
    } else if backend.branch_exists("master") {
        "master".to_owned()
    } else {
        // No conventional trunk: use the current branch rather than inventing one.
        backend
            .status()
            .ok()
            .and_then(|s| s.head.branch)
            .unwrap_or_else(|| "main".to_owned())
    }
}

fn policy(preset: &str, backend: &dyn GitBackend) -> Result<Policy, GitError> {
    let main = detect_main(backend);
    let p = match preset {
        "gitflow" => Policy {
            preset: "gitflow",
            main: main.clone(),
            integration: Some("develop".to_owned()),
            feature_prefix: "feature/",
            release_prefix: "release/",
            feature_base: "develop".to_owned(),
            release_base: "develop".to_owned(),
            release_merges_to: vec![main, "develop".to_owned()],
            tag_on_release: true,
            delete_on_finish: true,
            pull_request: false,
            upstream_first: false,
        },
        "github" => Policy {
            preset: "github",
            main: main.clone(),
            integration: None,
            feature_prefix: "",
            release_prefix: "",
            feature_base: main.clone(),
            release_base: main,
            release_merges_to: vec![],
            tag_on_release: false,
            delete_on_finish: true,
            pull_request: true,
            upstream_first: false,
        },
        "gitlab" => Policy {
            preset: "gitlab",
            main: main.clone(),
            integration: None,
            feature_prefix: "",
            release_prefix: "stable/",
            feature_base: main.clone(),
            release_base: main,
            release_merges_to: vec![],
            tag_on_release: true,
            delete_on_finish: true,
            pull_request: true,
            upstream_first: true,
        },
        "trunk" => Policy {
            preset: "trunk",
            main: main.clone(),
            integration: None,
            feature_prefix: "",
            release_prefix: "release-",
            feature_base: main.clone(),
            release_base: main,
            release_merges_to: vec![],
            tag_on_release: true,
            delete_on_finish: true,
            pull_request: true,
            upstream_first: true,
        },
        "release-flow" | "releaseflow" => Policy {
            preset: "release-flow",
            main: main.clone(),
            integration: None,
            feature_prefix: "users/",
            release_prefix: "releases/",
            feature_base: main.clone(),
            release_base: main,
            release_merges_to: vec![],
            tag_on_release: true,
            delete_on_finish: false,
            pull_request: true,
            upstream_first: true,
        },
        other => {
            return Err(err(format!(
                "unknown workflow {other:?}; choose gitflow, github, gitlab, trunk, or release-flow"
            )));
        }
    };
    Ok(p)
}

fn active(backend: &dyn GitBackend) -> Result<Policy, GitError> {
    match backend.config_get("rgit.workflow")? {
        Some(preset) => policy(&preset, backend),
        None => Err(err("no workflow set; run `rgit flow init <preset>` first")),
    }
}

fn current_branch(backend: &dyn GitBackend) -> Result<String, GitError> {
    backend
        .status()?
        .head
        .branch
        .ok_or_else(|| err("HEAD is detached; not on a branch"))
}

/// Set the active workflow and prepare any long-lived branches it needs.
pub fn init(backend: &dyn GitBackend, preset: &str) -> Result<String, GitError> {
    let p = policy(preset, backend)?;
    backend.config_set("rgit.workflow", p.preset)?;

    let mut notes = vec![format!("workflow set to {}", p.preset)];
    if let Some(dev) = &p.integration {
        if !backend.branch_exists(dev) {
            backend.checkout_branch(&p.main)?;
            backend.create_branch(dev)?;
            notes.push(format!("created integration branch {dev} from {}", p.main));
        }
    }
    Ok(notes.join("\n"))
}

/// Show the active workflow and its policy.
pub fn status(backend: &dyn GitBackend) -> Result<String, GitError> {
    let p = active(backend)?;
    let mut out = vec![
        format!("workflow: {}", p.preset),
        format!("main: {}", p.main),
    ];
    if let Some(dev) = &p.integration {
        out.push(format!("integration: {dev}"));
    }
    out.push(format!(
        "feature: {}<name> from {}",
        p.feature_prefix, p.feature_base
    ));
    out.push(format!(
        "release: {}<version> from {}",
        p.release_prefix, p.release_base
    ));
    out.push(format!(
        "finish: {}",
        if p.pull_request {
            "push + pull request"
        } else {
            "local merge"
        }
    ));
    Ok(out.join("\n"))
}

/// Start a feature branch per the active workflow.
pub fn start(backend: &dyn GitBackend, name: &str) -> Result<String, GitError> {
    let p = active(backend)?;
    let branch = format!("{}{name}", p.feature_prefix);
    backend.checkout_branch(&p.feature_base)?;
    backend.create_branch(&branch)?;
    Ok(format!("started {branch} from {}", p.feature_base))
}

/// Finish the current feature branch per the active workflow.
pub fn finish(backend: &dyn GitBackend) -> Result<String, GitError> {
    let p = active(backend)?;
    let current = current_branch(backend)?;
    if current == p.main || Some(&current) == p.integration.as_ref() {
        return Err(err(format!(
            "on {current}; switch to a feature branch to finish it"
        )));
    }

    if p.pull_request {
        backend.push(None, false, false, true, &|_| {})?;
        return Ok(open_pull_request(&current, &p.main));
    }

    let target = p.integration.clone().unwrap_or_else(|| p.main.clone());
    backend.checkout_branch(&target)?;
    backend.merge(&current, true, &|_| {})?;
    let mut note = format!("merged {current} into {target}");
    if p.delete_on_finish {
        backend.delete_branch(&current, true)?;
        note.push_str(" and deleted the branch");
    }
    Ok(note)
}

/// Start or finish a release per the active workflow.
pub fn release(backend: &dyn GitBackend, version: &str, finish: bool) -> Result<String, GitError> {
    let p = active(backend)?;
    let branch = format!("{}{version}", p.release_prefix);

    if !finish {
        backend.checkout_branch(&p.release_base)?;
        backend.create_branch(&branch)?;
        return Ok(format!("started release {branch} from {}", p.release_base));
    }

    if p.release_merges_to.is_empty() {
        let mut note = format!("release {branch} is ready");
        if p.tag_on_release {
            backend.checkout_branch(&branch)?;
            backend.create_tag(version, "")?;
            note = format!("tagged {version} on {branch}");
        }
        if p.upstream_first {
            note.push_str("; fix bugs on main first, then cherry-pick forward");
        }
        return Ok(note);
    }

    let mut notes = Vec::new();
    for (i, target) in p.release_merges_to.iter().enumerate() {
        backend.checkout_branch(target)?;
        backend.merge(&branch, true, &|_| {})?;
        notes.push(format!("merged {branch} into {target}"));
        if i == 0 && p.tag_on_release {
            backend.create_tag(version, "")?;
            notes.push(format!("tagged {version}"));
        }
    }
    if p.delete_on_finish {
        backend.delete_branch(&branch, true)?;
        notes.push(format!("deleted {branch}"));
    }
    Ok(notes.join("\n"))
}

/// After pushing, open a pull/merge request with the forge CLI if available (gh,
/// then glab); otherwise return guidance. These are the forge tools, not git.
pub(crate) fn open_pull_request(branch: &str, base: &str) -> String {
    // GitHub via gh. A stack re-submit re-pushes a rewritten branch, and an open
    // PR already tracks the branch head, so the push updated it: check for an
    // existing PR first and report that, rather than failing on a duplicate
    // `pr create` and misreporting it as "gh not installed".
    if let Some(url) = run_forge(
        "gh",
        &["pr", "view", branch, "--json", "url", "-q", ".url"],
    ) {
        return format!("pushed {branch} and updated its pull request:\n{url}");
    }
    if let Some(url) = run_forge(
        "gh",
        &["pr", "create", "--fill", "--base", base, "--head", branch],
    ) {
        return format!("pushed {branch} and opened a pull request:\n{url}");
    }
    // GitLab via glab. `mr view <branch>` prints the URL on its own line when an
    // MR exists, so the same update-vs-create split applies.
    if let Some(url) = run_forge("glab", &["mr", "view", branch, "-F", "json"])
        .and_then(|json| forge_json_field(&json, "web_url"))
    {
        return format!("pushed {branch} and updated its merge request:\n{url}");
    }
    if let Some(url) = run_forge(
        "glab",
        &[
            "mr",
            "create",
            "--fill",
            "--yes",
            "--source-branch",
            branch,
            "--target-branch",
            base,
        ],
    ) {
        return format!("pushed {branch} and opened a merge request:\n{url}");
    }
    format!("pushed {branch}. Open a pull request into {base} (install gh or glab to automate).")
}

/// Pull a top-level string field out of a forge CLI's JSON output without a JSON
/// dependency: find `"field"`, then the next quoted value after the colon.
fn forge_json_field(json: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\"");
    let after = &json[json.find(&key)? + key.len()..];
    let colon = after.find(':')?;
    let rest = &after[colon + 1..];
    let start = rest.find('"')? + 1;
    let end = rest[start..].find('"')? + start;
    let value = rest[start..end].replace("\\/", "/");
    (!value.is_empty()).then_some(value)
}

fn run_forge(bin: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(bin).args(args).output().ok()?;
    if out.status.success() {
        let url = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        (!url.is_empty()).then_some(url)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::forge_json_field;

    #[test]
    fn extracts_web_url_from_forge_json() {
        let json = r#"{"iid":7,"web_url":"https:\/\/gitlab.com\/acme\/app\/-\/merge_requests\/7","title":"x"}"#;
        assert_eq!(
            forge_json_field(json, "web_url").as_deref(),
            Some("https://gitlab.com/acme/app/-/merge_requests/7")
        );
        assert_eq!(forge_json_field(json, "missing"), None);
    }
}
