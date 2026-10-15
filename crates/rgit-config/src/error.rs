use std::fmt;
use std::path::PathBuf;

/// A failure while writing a config file.
#[derive(Debug)]
pub enum Error {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        message: String,
    },
    /// The `.lock` file stayed in place for the whole wait.
    Lock(PathBuf),
    /// The key is empty or crosses a value that is not a table.
    BadKey(String),
    /// The edit closure failed.
    Edit(Box<dyn std::error::Error + Send + Sync>),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Error::Parse { path, message } => write!(f, "{}: {message}", path.display()),
            Error::Lock(path) => write!(f, "{}: timed out waiting for lock", path.display()),
            Error::BadKey(key) => write!(f, "cannot set key {key:?}"),
            Error::Edit(source) => write!(f, "{source}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            Error::Edit(source) => Some(source.as_ref()),
            _ => None,
        }
    }
}
