use std::fmt;
use std::path::PathBuf;

#[derive(Debug)]
pub enum Error {
    NothingStaged,
    Git(String),
    NoTool {
        key: String,
    },
    UnknownTool {
        name: String,
        defined: Vec<String>,
    },
    Placeholder(String),
    PromptFile {
        path: PathBuf,
        error: String,
    },
    Spawn {
        tool: String,
        error: String,
    },
    Failed {
        tool: String,
        code: Option<i32>,
        stderr: String,
    },
    Timeout {
        tool: String,
        secs: u64,
    },
    TempFile {
        path: PathBuf,
        error: String,
    },
    Interrupted,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NothingStaged => f.write_str("nothing staged"),
            Error::Git(message) => f.write_str(message),
            Error::NoTool { key } => write!(f, "no AI tool chosen; set {key}"),
            Error::UnknownTool { name, defined } => {
                if defined.is_empty() {
                    write!(f, "unknown AI tool {name}; no ai.<name>.command is set")
                } else {
                    write!(
                        f,
                        "unknown AI tool {name}; defined tools: {}",
                        defined.join(", ")
                    )
                }
            }
            Error::Placeholder(name) => write!(f, "unknown placeholder {{{{{name}}}}} in prompt"),
            Error::PromptFile { path, error } => {
                write!(f, "cannot read {}: {error}", path.display())
            }
            Error::Spawn { tool, error } => write!(f, "cannot run AI tool {tool}: {error}"),
            Error::Failed { tool, code, stderr } => {
                match code {
                    Some(code) => write!(f, "AI tool {tool} failed with exit code {code}")?,
                    None => write!(f, "AI tool {tool} was stopped by a signal")?,
                }
                if !stderr.is_empty() {
                    write!(f, ":\n{stderr}")?;
                }
                Ok(())
            }
            Error::TempFile { path, error } => {
                write!(f, "cannot write {}: {error}", path.display())
            }
            Error::Interrupted => f.write_str("interrupted"),
            Error::Timeout { tool, secs } => {
                write!(f, "AI tool {tool} timed out after {secs} seconds")
            }
        }
    }
}

impl std::error::Error for Error {}
