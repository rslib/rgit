//! Layered rgit settings: global file, shared and private repo files, then
//! environment, with the last layer that sets a key winning.

mod discover;
mod edit;
mod error;
mod keys;
mod settings;
mod warning;

pub use edit::{edit_toml, read_toml, set_private};
pub use error::Error;
pub use keys::{KEYS, KeyDef, Kind};
pub use settings::{
    Core, Entry, Files, Layer, OutputFormat, RepoDirs, Settings, Source, global_file,
    global_file_with,
};
pub use toml_edit;
pub use warning::{Warning, WarningKind};
