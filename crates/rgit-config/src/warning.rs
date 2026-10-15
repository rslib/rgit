use std::fmt;

use crate::settings::Source;

/// A problem with one setting or file. Loading goes on without the bad part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub source: Source,
    pub kind: WarningKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarningKind {
    Read(String),
    /// `line` is 1-based.
    Parse {
        line: usize,
        message: String,
    },
    UnknownKey(String),
    BadValue {
        key: String,
        expected: String,
    },
    /// The key may not be set in the shared repo file.
    NotShared(String),
    /// A top-level table did not fit the type the caller asked for.
    Section {
        name: String,
        message: String,
    },
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.source)?;
        match &self.kind {
            WarningKind::Read(message) => write!(f, "{message}"),
            WarningKind::Parse { line, message } => write!(f, "line {line}: {message}"),
            WarningKind::UnknownKey(key) => write!(f, "unknown key {key}"),
            WarningKind::BadValue { key, expected } => write!(f, "{key}: expected {expected}"),
            WarningKind::NotShared(key) => {
                write!(f, "{key} is not allowed in the shared repo file")
            }
            WarningKind::Section { name, message } => write!(f, "[{name}]: {message}"),
        }
    }
}
