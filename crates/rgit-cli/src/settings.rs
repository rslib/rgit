//! `rgit settings`: every known key with its effective value and where it
//! came from.

use rgit_config::{Entry, Layer, Source};

use crate::cli::CliError;
use crate::output::Output;
use crate::toon::{Node, Obj};

/// The layer column: which kind of source set the value.
fn layer_name(source: &Source) -> &'static str {
    match source {
        Source::Default => "default",
        Source::File {
            layer: Layer::Global,
            ..
        } => "global",
        Source::File {
            layer: Layer::Shared,
            ..
        } => "shared",
        Source::File {
            layer: Layer::Private,
            ..
        } => "private",
        Source::Env(_) => "env",
    }
}

fn entries(key: Option<&str>) -> anyhow::Result<Vec<Entry>> {
    let all = crate::settings().entries();
    let Some(key) = key else {
        return Ok(all);
    };
    let found: Vec<Entry> = all.into_iter().filter(|e| e.key == key).collect();
    if found.is_empty() {
        return Err(anyhow::Error::new(CliError {
            message: format!("unknown setting {key}"),
            help: Some("Run `rgit settings` to list the settings".to_owned()),
            code: 1,
        }));
    }
    Ok(found)
}

fn line(entry: &Entry) -> String {
    let value = entry.value.as_deref().unwrap_or("(unset)");
    format!("{} = {value}  ({})", entry.key, entry.source)
}

fn row(entry: &Entry) -> Obj {
    vec![
        ("key".to_owned(), Node::from(entry.key.as_str())),
        (
            "value".to_owned(),
            Node::from(entry.value.as_deref().unwrap_or("")),
        ),
        ("source".to_owned(), Node::from(entry.source.to_string())),
        ("layer".to_owned(), Node::from(layer_name(&entry.source))),
    ]
}

/// The settings as human lines, one per key.
pub fn text(key: Option<&str>) -> anyhow::Result<String> {
    Ok(entries(key)?
        .iter()
        .map(line)
        .collect::<Vec<_>>()
        .join("\n"))
}

/// The settings as an output: human lines, or a table of key, value, source
/// and layer.
pub fn run(key: Option<&str>) -> anyhow::Result<Output> {
    let entries = entries(key)?;
    let text = entries.iter().map(line).collect::<Vec<_>>().join("\n");
    let rows = entries.iter().map(row).collect();
    Ok(Output::new(text).list(
        "settings",
        rows,
        &["key", "value", "source", "layer"],
        "no settings",
    ))
}
