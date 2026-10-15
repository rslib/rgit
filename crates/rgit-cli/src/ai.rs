//! `rgit ai`: draft or review the staged changes with an AI tool from the
//! settings, and show the prompt a task would use.

use std::sync::Arc;

use rgit_ai::{Inputs, Task};
use rgit_git::GitBackend;

use crate::cli::{AiCmd, AiTask, CliError};
use crate::output::Output;

fn fail(error: impl std::fmt::Display) -> anyhow::Error {
    anyhow::Error::new(CliError {
        message: error.to_string(),
        help: None,
        code: 1,
    })
}

/// An AI error; an interrupt exits 130, as a shell does for Ctrl-C.
fn fail_ai(error: rgit_ai::Error) -> anyhow::Error {
    let code = if matches!(error, rgit_ai::Error::Interrupted) {
        130
    } else {
        1
    };
    anyhow::Error::new(CliError {
        message: error.to_string(),
        help: None,
        code,
    })
}

pub use rgit_ai::Scope;

/// Run the tool for `task` on the staged changes: the tool name and its cleaned output.
fn execute(
    backend: &Arc<dyn GitBackend>,
    task: Task,
    explicit: Option<&str>,
    scope: &Scope,
) -> anyhow::Result<(String, String)> {
    let settings = crate::settings();
    let tool = rgit_ai::tool_for(settings, task, explicit).map_err(fail)?;
    let inputs = rgit_ai::inputs(backend.as_ref(), scope).map_err(fail)?;
    let text = rgit_ai::run_interruptible(settings, task, &tool, &inputs).map_err(fail_ai)?;
    Ok((tool, text))
}

/// The drafted commit message for `rgit commit --draft`.
pub fn draft_message(
    backend: &Arc<dyn GitBackend>,
    tool: Option<&str>,
    scope: &Scope,
) -> anyhow::Result<String> {
    execute(backend, Task::Draft, tool, scope).map(|(_, text)| text)
}

fn prompt_text(task: Task, explicit: Option<&str>) -> anyhow::Result<String> {
    let settings = crate::settings();
    let tool = match rgit_ai::tool_for(settings, task, explicit) {
        Ok(tool) => tool,
        Err(rgit_ai::Error::NoTool { .. }) => String::new(),
        Err(error) => return Err(fail(error)),
    };
    let keep = |name: &str| format!("{{{{{name}}}}}");
    let inputs = Inputs {
        diff: keep("diff"),
        stat: keep("stat"),
        files: keep("files"),
        log: keep("log"),
        branch: keep("branch"),
        repo: std::path::PathBuf::new(),
    };
    rgit_ai::prompt(settings, task, &tool, &inputs).map_err(fail)
}

pub fn run(backend: &Arc<dyn GitBackend>, cmd: AiCmd) -> anyhow::Result<Output> {
    match cmd {
        AiCmd::Draft { tool } => {
            let (tool, message) =
                execute(backend, Task::Draft, tool.as_deref(), &Scope::default())?;
            Ok(Output::new(message.clone())
                .with("message", message)
                .with("tool", tool))
        }
        AiCmd::Review { tool } => {
            let (tool, review) =
                execute(backend, Task::Review, tool.as_deref(), &Scope::default())?;
            Ok(Output::new(review.clone())
                .with("review", review)
                .with("tool", tool))
        }
        AiCmd::Prompt { task, tool } => {
            let task = match task {
                AiTask::Draft => Task::Draft,
                AiTask::Review => Task::Review,
            };
            let text = prompt_text(task, tool.as_deref())?;
            Ok(Output::new(text.clone())
                .with("prompt", text)
                .with("task", task.as_str()))
        }
    }
}
