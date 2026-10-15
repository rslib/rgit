//! Built-in branching workflows. The user picks a preset (gitflow, github,
//! gitlab, trunk, release-flow) once; a single verb set (start / finish /
//! release) then behaves per that preset's policy. The active preset is stored
//! in the private rgit config file as `workflow.preset`, so nothing is forced:
//! ignore these and rgit is still a plain git tool. Lives in the backend so the
//! CLI and TUI share it.

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
    let settings = rgit_config::Settings::load(Some(&crate::stack_file::dirs_of(backend)));
    match settings.core().workflow_preset {
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
    rgit_config::set_private(
        &crate::stack_file::dirs_of(backend),
        "workflow.preset",
        p.preset,
    )
    .map_err(|e| err(e.to_string()))?;

    let mut notes = vec![format!("workflow set to {}", p.preset)];
    if let Some(dev) = &p.integration
        && !backend.branch_exists(dev)
    {
        backend.checkout_branch(&p.main)?;
        backend.create_branch(dev)?;
        notes.push(format!("created integration branch {dev} from {}", p.main));
    }
    Ok(notes.join("\n"))
}

/// Show the active workflow and its policy.
/// The active workflow as ordered `(field, value)` pairs.
pub fn describe(backend: &dyn GitBackend) -> Result<Vec<(&'static str, String)>, GitError> {
    let p = active(backend)?;
    let mut out = vec![("workflow", p.preset.to_owned()), ("main", p.main.clone())];
    if let Some(dev) = &p.integration {
        out.push(("integration", dev.clone()));
    }
    out.push((
        "feature",
        format!("{}<name> from {}", p.feature_prefix, p.feature_base),
    ));
    out.push((
        "release",
        format!("{}<version> from {}", p.release_prefix, p.release_base),
    ));
    out.push((
        "finish",
        if p.pull_request {
            "push + pull request"
        } else {
            "local merge"
        }
        .to_owned(),
    ));
    Ok(out)
}

pub fn status(backend: &dyn GitBackend) -> Result<String, GitError> {
    Ok(describe(backend)?
        .into_iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join("\n"))
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
pub fn finish(backend: &dyn GitBackend) -> Result<FinishOutcome, GitError> {
    let p = active(backend)?;
    let current = current_branch(backend)?;
    if current == p.main || Some(&current) == p.integration.as_ref() {
        return Err(err(format!(
            "on {current}; switch to a feature branch to finish it"
        )));
    }

    if p.pull_request {
        backend.push(None, false, false, true, &|_| {})?;
        // The forge half (find or create the pull request) lives above rgit-git;
        // hand the caller the pair it needs for it.
        return Ok(FinishOutcome::Pushed {
            branch: current,
            base: p.main,
        });
    }

    let target = p.integration.clone().unwrap_or_else(|| p.main.clone());
    backend.checkout_branch(&target)?;
    backend.merge(&current, true, false, &|_| {})?;
    let mut note = format!("merged {current} into {target}");
    if p.delete_on_finish {
        backend.delete_branch(&current, true)?;
        note.push_str(" and deleted the branch");
    }
    Ok(FinishOutcome::Merged(note))
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
        backend.merge(&branch, true, false, &|_| {})?;
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

/// What `finish` did with the feature branch.
pub enum FinishOutcome {
    /// The branch was merged locally; the note describes it.
    Merged(String),
    /// The branch was pushed; the caller opens or updates its pull request
    /// against `base` through the forge.
    Pushed { branch: String, base: String },
}
