use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::de::DeserializeOwned;

use crate::keys::{KEYS, lookup};
use crate::warning::{Warning, WarningKind};

const ENV_CONFIG: &str = "RGIT_CONFIG";
const ENV_OUTPUT: &str = "RGIT_OUTPUT";
const KEY_OUTPUT_FORMAT: &str = "output.format";

/// The repo folders the caller found; this crate cannot discover them.
#[derive(Debug, Clone)]
pub struct RepoDirs {
    pub worktree_root: Option<PathBuf>,
    pub common_dir: PathBuf,
}

impl RepoDirs {
    /// The committed file `<worktree root>/.rgit/config.toml`, if there is a worktree.
    pub fn shared_file(&self) -> Option<PathBuf> {
        self.worktree_root
            .as_ref()
            .map(|root| root.join(".rgit").join("config.toml"))
    }

    pub fn private_file(&self) -> PathBuf {
        self.common_dir.join("rgit").join("config.toml")
    }

    pub fn stack_file(&self) -> PathBuf {
        self.common_dir.join("rgit").join("stacks.toml")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Global,
    Shared,
    Private,
}

/// Where a setting got its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Default,
    File { layer: Layer, path: PathBuf },
    Env(&'static str),
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Default => f.write_str("default"),
            Source::File { path, .. } => write!(f, "{}", path.display()),
            Source::Env(name) => f.write_str(name),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Human,
    Json,
    Toon,
}

impl OutputFormat {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "human" => Some(Self::Human),
            "json" => Some(Self::Json),
            "toon" => Some(Self::Toon),
            _ => None,
        }
    }
}

/// The typed core keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Core {
    pub output_format: Option<OutputFormat>,
    pub compact: bool,
    pub change_id: bool,
    pub workflow_preset: Option<String>,
}

/// The config files to read, lowest layer first. A `None` layer is skipped.
#[derive(Debug, Clone, Default)]
pub struct Files {
    pub global: Option<PathBuf>,
    pub shared: Option<PathBuf>,
    pub private: Option<PathBuf>,
}

/// One row for listing: a key, its value and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub value: Option<String>,
    pub source: Source,
    pub help: &'static str,
}

/// The merged settings of all layers, with where each key came from.
#[derive(Debug, Clone)]
pub struct Settings {
    table: toml::Table,
    sources: BTreeMap<String, Source>,
    warnings: Vec<Warning>,
}

/// The global file path from the real environment. The file may not exist.
pub fn global_file() -> Option<PathBuf> {
    global_file_with(&|name| std::env::var(name).ok())
}

/// The global file path: `$RGIT_CONFIG`, else `$XDG_CONFIG_HOME/rgit/config.toml`,
/// else `$HOME/.config/rgit/config.toml`.
pub fn global_file_with(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(explicit) = env(ENV_CONFIG) {
        return Some(PathBuf::from(explicit));
    }
    let base = env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("rgit").join("config.toml"))
}

impl Settings {
    /// Load every layer using the real environment. Never fails; see [`Settings::warnings`].
    pub fn load(repo: Option<&RepoDirs>) -> Settings {
        let env = |name: &str| std::env::var(name).ok();
        let files = Files {
            global: global_file_with(&env),
            shared: repo.and_then(RepoDirs::shared_file),
            private: repo.map(RepoDirs::private_file),
        };
        Self::load_with(files, &env)
    }

    pub fn load_with(files: Files, env: &dyn Fn(&str) -> Option<String>) -> Settings {
        let mut settings = Settings {
            table: toml::Table::new(),
            sources: BTreeMap::new(),
            warnings: Vec::new(),
        };
        let layers = [
            (Layer::Global, files.global),
            (Layer::Shared, files.shared),
            (Layer::Private, files.private),
        ];
        for (layer, path) in layers {
            if let Some(path) = path {
                settings.apply_file(layer, path);
            }
        }
        if let Some(value) = env(ENV_OUTPUT).filter(|v| !v.trim().is_empty()) {
            let source = Source::Env(ENV_OUTPUT);
            settings.apply_leaf(KEY_OUTPUT_FORMAT, toml::Value::String(value), source);
        }
        settings
    }

    fn apply_file(&mut self, layer: Layer, path: PathBuf) {
        let source = Source::File {
            layer,
            path: path.clone(),
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                self.warn(source, WarningKind::Read(error.to_string()));
                return;
            }
        };
        let table = match toml::from_str::<toml::Table>(&text) {
            Ok(table) => table,
            Err(error) => {
                let offset = error.span().map_or(0, |span| span.start.min(text.len()));
                let line = 1 + text.as_bytes()[..offset]
                    .iter()
                    .filter(|b| **b == b'\n')
                    .count();
                let message = error.message().to_owned();
                self.warn(source, WarningKind::Parse { line, message });
                return;
            }
        };
        let mut leaves = Vec::new();
        flatten(&table, "", &mut leaves);
        for (key, value) in leaves {
            self.apply_leaf(&key, value, source.clone());
        }
    }

    fn apply_leaf(&mut self, key: &str, value: toml::Value, source: Source) {
        let Some(def) = lookup(key) else {
            self.warn(source, WarningKind::UnknownKey(key.to_owned()));
            return;
        };
        if !def.shared
            && matches!(
                source,
                Source::File {
                    layer: Layer::Shared,
                    ..
                }
            )
        {
            self.warn(source, WarningKind::NotShared(key.to_owned()));
            return;
        }
        if !def.kind.accepts(&value) {
            let expected = def.kind.expected();
            let key = key.to_owned();
            self.warn(source, WarningKind::BadValue { key, expected });
            return;
        }
        let mut segments = key.split('.').peekable();
        let mut table = &mut self.table;
        while let Some(segment) = segments.next() {
            if segments.peek().is_none() {
                table.insert(segment.to_owned(), value);
                break;
            }
            let entry = table
                .entry(segment.to_owned())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            match entry {
                toml::Value::Table(inner) => table = inner,
                _ => unreachable!("known keys have a fixed table shape"),
            }
        }
        self.sources.insert(key.to_owned(), source);
    }

    fn warn(&mut self, source: Source, kind: WarningKind) {
        self.warnings.push(Warning { source, kind });
    }

    pub fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    /// The whole merged table, for callers that deserialize the root.
    pub fn table(&self) -> &toml::Table {
        &self.table
    }

    /// Deserialize one top-level table. A missing table gives `T::default()`.
    pub fn section<T: DeserializeOwned + Default>(&self, name: &str) -> Result<T, Warning> {
        let Some(value) = self.table.get(name) else {
            return Ok(T::default());
        };
        value.clone().try_into().map_err(|error: toml::de::Error| {
            let prefix = format!("{name}.");
            let source = self
                .sources
                .iter()
                .find(|(key, _)| key.starts_with(&prefix) || key.as_str() == name)
                .map_or(Source::Default, |(_, source)| source.clone());
            Warning {
                source,
                kind: WarningKind::Section {
                    name: name.to_owned(),
                    message: error.message().to_owned(),
                },
            }
        })
    }

    /// Where a key's value came from; `None` for a key rgit does not know.
    pub fn source(&self, key: &str) -> Option<Source> {
        if let Some(source) = self.sources.get(key) {
            return Some(source.clone());
        }
        lookup(key).map(|_| Source::Default)
    }

    pub fn core(&self) -> Core {
        let get = |key: &str| {
            let mut value = None;
            let mut table = Some(&self.table);
            for segment in key.split('.') {
                value = table.and_then(|t| t.get(segment));
                table = value.and_then(toml::Value::as_table);
            }
            value
        };
        let flag = |key| get(key).and_then(toml::Value::as_bool).unwrap_or(false);
        Core {
            output_format: get(KEY_OUTPUT_FORMAT)
                .and_then(toml::Value::as_str)
                .and_then(OutputFormat::parse),
            compact: flag("output.compact"),
            change_id: flag("commit.change_id"),
            workflow_preset: get("workflow.preset")
                .and_then(toml::Value::as_str)
                .map(str::to_owned),
        }
    }

    /// Every key a layer set plus every fixed known key, sorted by key.
    pub fn entries(&self) -> Vec<Entry> {
        let mut leaves = Vec::new();
        flatten(&self.table, "", &mut leaves);
        let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
        for (key, value) in leaves {
            let def = lookup(&key).expect("merged keys are known");
            let source = self.sources.get(&key).cloned().unwrap_or(Source::Default);
            let value = match value {
                toml::Value::String(s) => s,
                other => other.to_string(),
            };
            let help = def.help;
            let entry = Entry {
                key: key.clone(),
                value: Some(value),
                source,
                help,
            };
            entries.insert(key, entry);
        }
        for def in KEYS.iter().filter(|def| def.is_concrete()) {
            entries
                .entry(def.pattern.to_owned())
                .or_insert_with(|| Entry {
                    key: def.pattern.to_owned(),
                    value: def.default.map(str::to_owned),
                    source: Source::Default,
                    help: def.help,
                });
        }
        entries.into_values().collect()
    }
}

fn flatten(table: &toml::Table, prefix: &str, out: &mut Vec<(String, toml::Value)>) {
    for (name, value) in table {
        let key = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        match value {
            toml::Value::Table(inner) => flatten(inner, &key, out),
            leaf => out.push((key, leaf.clone())),
        }
    }
}
