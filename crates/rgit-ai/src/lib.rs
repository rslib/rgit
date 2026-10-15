mod clean;
mod error;
mod inputs;
mod interrupt;
mod prompt;
mod run;

pub use error::Error;
pub use inputs::{Scope, inputs};
pub use prompt::{builtin_rules, builtin_template, prompt, tool_for, tools};
pub use run::{run, run_interruptible, run_with_cancel};

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    Draft,
    Review,
}

impl Task {
    pub fn as_str(self) -> &'static str {
        match self {
            Task::Draft => "draft",
            Task::Review => "review",
        }
    }
}

/// What the caller gathered from git. `diff` is the full staged diff.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    pub diff: String,
    pub stat: String,
    pub files: String,
    pub log: String,
    pub branch: String,
    pub repo: PathBuf,
}
