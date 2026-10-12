//! A command's help as a value, for the TOON and JSON modes: the about text,
//! the usage, its subcommands, its flags and its examples, so an agent can
//! read a command's surface without parsing clap's prose.

use clap::{Arg, Command};

use crate::examples;
use crate::obj;
use crate::output::Output;
use crate::toon::{Node, Obj};

/// `cmd`, reached by `path` (`rgit forge pr`), as a result an agent can read.
pub fn structured(cmd: &Command, path: &str) -> Output {
    let about = cmd
        .get_long_about()
        .or_else(|| cmd.get_about())
        .map(|a| a.to_string())
        .unwrap_or_default();
    let mut rendered = cmd.clone().bin_name(path);
    let usage = rendered
        .render_usage()
        .to_string()
        .trim_start_matches("Usage: ")
        .trim()
        .to_owned();
    let subcommands: Vec<Obj> = cmd
        .get_subcommands()
        .filter(|c| !c.is_hide_set() && c.get_name() != "help")
        .map(|c| {
            obj! {
                "name" => c.get_name(),
                "about" => about_of(c.get_about().map(|a| a.to_string()).unwrap_or_default()),
            }
        })
        .collect();
    let flags: Vec<Obj> = cmd
        .get_arguments()
        .filter(|a| !a.is_hide_set() && !matches!(a.get_id().as_str(), "help" | "version"))
        .map(|a| {
            obj! {
                "flag" => flag_name(a),
                "about" => about_of(a.get_help().map(|h| h.to_string()).unwrap_or_default()),
            }
        })
        .collect();
    let mut out = Output::new(String::new())
        .with("about", about)
        .with("usage", usage)
        .list(
            "subcommands",
            subcommands,
            &["name", "about"],
            "0 subcommands",
        )
        .list("flags", flags, &["flag", "about"], "0 flags");
    if let Some(lines) = path.strip_prefix("rgit ").and_then(examples::lookup) {
        let examples: Vec<Node> = lines.iter().map(|l| Node::Str((*l).to_owned())).collect();
        out = out.with("examples", Node::List(examples));
    }
    out
}

/// A flag as a person types it: `-s, --short`, `--porcelain` or `-q`. A
/// positional that takes values is named by its value (`ARGS`).
fn flag_name(arg: &Arg) -> String {
    match (arg.get_short(), arg.get_long()) {
        (Some(short), Some(long)) => format!("-{short}, --{long}"),
        (Some(short), None) => format!("-{short}"),
        (None, Some(long)) => format!("--{long}"),
        (None, None) => arg
            .get_value_names()
            .and_then(|v| v.first())
            .map(|v| v.to_string())
            .unwrap_or_else(|| arg.get_id().to_string()),
    }
}

/// An about text as one line: its first sentence, without the full stop.
fn about_of(about: String) -> String {
    let line = about.lines().next().unwrap_or("").trim();
    line.split(". ")
        .next()
        .unwrap_or(line)
        .trim_end_matches('.')
        .to_owned()
}
