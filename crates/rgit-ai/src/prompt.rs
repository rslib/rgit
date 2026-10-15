use std::path::PathBuf;

use rgit_config::Settings;

use crate::{Error, Inputs, Task};

const RULES: &str = include_str!("prompts/rules.md");
const DRAFT: &str = include_str!("prompts/draft.md");
const REVIEW: &str = include_str!("prompts/review.md");
const DEFAULT_MAX_DIFF_BYTES: usize = 100_000;

pub fn builtin_template(task: Task) -> &'static str {
    match task {
        Task::Draft => DRAFT,
        Task::Review => REVIEW,
    }
}

pub fn builtin_rules() -> &'static str {
    RULES
}

fn get<'a>(settings: &'a Settings, key: &str) -> Option<&'a toml::Value> {
    let mut segments = key.split('.');
    let mut value = settings.table().get(segments.next()?)?;
    for segment in segments {
        value = value.as_table()?.get(segment)?;
    }
    Some(value)
}

fn get_str<'a>(settings: &'a Settings, key: &str) -> Option<&'a str> {
    get(settings, key)?.as_str()
}

/// The names that have an `ai.<name>.command`, sorted.
pub fn tools(settings: &Settings) -> Vec<String> {
    let Some(table) = settings.table().get("ai").and_then(toml::Value::as_table) else {
        return Vec::new();
    };
    let mut names: Vec<String> = table
        .iter()
        .filter(|(_, tool)| tool.get("command").is_some_and(toml::Value::is_str))
        .map(|(name, _)| name.clone())
        .collect();
    names.sort();
    names
}

pub fn tool_for(settings: &Settings, task: Task, explicit: Option<&str>) -> Result<String, Error> {
    let name = match explicit {
        Some(name) => name,
        None => {
            let key = format!("commit.ai.{}", task.as_str());
            get_str(settings, &key).ok_or(Error::NoTool { key })?
        }
    };
    let defined = tools(settings);
    if defined.iter().any(|tool| tool == name) {
        Ok(name.to_owned())
    } else {
        Err(Error::UnknownTool {
            name: name.to_owned(),
            defined,
        })
    }
}

fn read_file(path: PathBuf) -> Result<String, Error> {
    std::fs::read_to_string(&path).map_err(|error| Error::PromptFile {
        path,
        error: error.to_string(),
    })
}

/// The text of an inline-or-file setting under `base`, if either is set.
fn text(settings: &Settings, base: &str) -> Result<Option<String>, Error> {
    if let Some(inline) = get_str(settings, base) {
        return Ok(Some(inline.to_owned()));
    }
    let file = format!("{base}_file");
    match settings.path(&file) {
        Some(path) => read_file(path).map(Some),
        None => Ok(None),
    }
}

fn template(settings: &Settings, task: Task, tool: &str) -> Result<String, Error> {
    let name = task.as_str();
    if let Some(text) = text(settings, &format!("ai.{tool}.{name}_prompt"))? {
        return Ok(text);
    }
    if let Some(text) = text(settings, &format!("commit.ai.{name}_prompt"))? {
        return Ok(text);
    }
    Ok(builtin_template(task).to_owned())
}

fn rules(settings: &Settings) -> Result<String, Error> {
    Ok(text(settings, "commit.ai.rules")?.unwrap_or_else(|| RULES.trim_end().to_owned()))
}

fn cut_diff(diff: &str, max: usize) -> String {
    if diff.len() <= max {
        return diff.to_owned();
    }
    let mut end = max;
    while !diff.is_char_boundary(end) {
        end -= 1;
    }
    if let Some(newline) = diff[..end].rfind('\n') {
        end = newline + 1;
    }
    let mut out = diff[..end].to_owned();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!(
        "[diff cut at {max} bytes; the stat above shows the full scope]"
    ));
    out
}

/// Whether `template` has a `{{name}}` placeholder.
fn mentions(template: &str, name: &str) -> bool {
    template
        .split("{{")
        .skip(1)
        .any(|rest| rest.split_once("}}").is_some_and(|(n, _)| n.trim() == name))
}

/// One pass over `template`; substituted values are never scanned.
fn render(template: &str, value: impl Fn(&str) -> Option<String>) -> Result<String, Error> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            out.push_str(&rest[open..]);
            return Ok(out);
        };
        let name = after[..close].trim();
        out.push_str(&value(name).ok_or_else(|| Error::Placeholder(name.to_owned()))?);
        rest = &after[close + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The resolved template with every placeholder filled.
pub fn prompt(
    settings: &Settings,
    task: Task,
    tool: &str,
    inputs: &Inputs,
) -> Result<String, Error> {
    if inputs.diff.trim().is_empty() {
        return Err(Error::NothingStaged);
    }
    let template = template(settings, task, tool)?;
    let max = match get(settings, "commit.ai.max_diff_bytes").and_then(toml::Value::as_integer) {
        Some(n) => usize::try_from(n).unwrap_or(DEFAULT_MAX_DIFF_BYTES),
        None => DEFAULT_MAX_DIFF_BYTES,
    };
    let rules = if mentions(&template, "rules") {
        rules(settings)?
    } else {
        String::new()
    };
    render(&template, |name| {
        Some(match name {
            "diff" => cut_diff(&inputs.diff, max),
            "stat" => inputs.stat.clone(),
            "files" => inputs.files.clone(),
            "log" => inputs.log.clone(),
            "branch" => inputs.branch.clone(),
            "rules" => rules.clone(),
            _ => return None,
        })
    })
}
