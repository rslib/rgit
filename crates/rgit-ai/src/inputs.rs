use rgit_git::GitBackend;

use crate::{Error, Inputs};

const LOG_SUBJECTS: usize = 10;

/// What a commit will contain beyond the index: `-a` takes tracked changes,
/// paths take those paths (only them, unless `include`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Scope<'a> {
    pub all: bool,
    pub include: bool,
    pub amend: bool,
    pub paths: &'a [String],
}

fn git(error: rgit_git::GitError) -> Error {
    Error::Git(error.to_string())
}

/// Gather what the prompt templates need from the repo for the changes in `scope`.
pub fn inputs(backend: &dyn GitBackend, scope: &Scope) -> Result<Inputs, Error> {
    let diff = if scope.all || scope.amend || !scope.paths.is_empty() {
        backend.commit_patch(scope.all, scope.include, scope.amend, scope.paths)
    } else {
        backend.staged_patch()
    }
    .map_err(git)?;
    let (stat, files) =
        match rgit_git::parse_patch(diff.as_bytes(), &rgit_git::ApplyOpts::default()) {
            Ok(parsed) => (
                rgit_git::patch_stat(&parsed),
                parsed
                    .iter()
                    .map(|f| f.name().to_owned())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Err(_) => (String::new(), String::new()),
        };
    let log = backend
        .log(&rgit_git::LogOptions {
            limit: LOG_SUBJECTS,
            ..Default::default()
        })
        .unwrap_or_default()
        .into_iter()
        .map(|entry| entry.summary)
        .collect::<Vec<_>>()
        .join("\n");
    let branch = backend
        .status()
        .map_err(git)?
        .head
        .branch
        .unwrap_or_else(|| "HEAD".to_owned());
    Ok(Inputs {
        diff,
        stat: stat.trim_end().to_owned(),
        files,
        log,
        branch,
        repo: backend.workdir().to_owned(),
    })
}
