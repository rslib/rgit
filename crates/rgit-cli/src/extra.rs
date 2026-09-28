//! git commands for object replacement, rerere, packs and diagnostics, done
//! natively with git's output and exit codes.

use std::sync::Arc;

use clap::Subcommand;
use rgit_git::GitBackend;

use crate::cli::CliError;
use crate::output::Output;

#[derive(Subcommand)]
pub enum Extra {
    /// Stand one object in for another with refs under refs/replace/, like
    /// `git replace`: `<object> <replacement>`, `--graft`, `--edit`, `-d`
    /// or a listing.
    Replace {
        /// List replace refs matching a pattern (the default with no
        /// arguments).
        #[arg(short = 'l', long)]
        list: bool,
        /// Delete the replace refs of these objects.
        #[arg(short = 'd', long)]
        delete: bool,
        /// Edit an object's content in the editor and use the result.
        #[arg(short = 'e', long)]
        edit: bool,
        /// Replace a commit with one that has these parents.
        #[arg(short = 'g', long)]
        graft: bool,
        /// Turn each line of info/grafts into a --graft, then remove the file.
        #[arg(long)]
        convert_graft_file: bool,
        /// Replace the ref if it exists, and allow objects of another type.
        #[arg(short = 'f', long)]
        force: bool,
        /// With --edit, edit a tree's raw bytes rather than ls-tree lines.
        #[arg(long)]
        raw: bool,
        /// Listing format: short, medium or long.
        #[arg(long, value_name = "FORMAT")]
        format: Option<String>,
        /// Objects (and replacement, parents or pattern, by mode).
        args: Vec<String>,
    },
    /// Show each contact (`Name <email>` or `<email>`) as the mailmap maps
    /// it, like `git check-mailmap`.
    CheckMailmap {
        /// Also read contacts from stdin, one per line, after the arguments.
        #[arg(long)]
        stdin: bool,
        /// Read this mailmap file too.
        #[arg(long, value_name = "FILE")]
        mailmap_file: Vec<String>,
        /// Read the mailmap in this blob too (`HEAD:.mailmap`).
        #[arg(long, value_name = "BLOB")]
        mailmap_blob: Vec<String>,
        /// The contacts to map.
        contacts: Vec<String>,
        #[arg(skip)]
        input: Option<String>,
    },
    /// Check packs against their indexes, like `git verify-pack`; `-v`
    /// lists every object and the delta chain histogram.
    VerifyPack {
        /// List each object: id, type, size, size in pack, offset and delta
        /// base.
        #[arg(short = 'v', long)]
        verbose: bool,
        /// Show only the delta chain histogram.
        #[arg(short = 's', long)]
        stat_only: bool,
        /// The hash algorithm (only sha1).
        #[arg(long, value_name = "HASH")]
        object_format: Option<String>,
        /// The packs: `<pack>.idx`, `<pack>.pack` or `<pack>`.
        packs: Vec<String>,
    },
    /// List a pack index read from stdin, like `git show-index`:
    /// `<offset> <id> (<crc32>)` per object.
    ShowIndex {
        /// The hash algorithm (only sha1).
        #[arg(long, value_name = "HASH")]
        object_format: Option<String>,
    },
    /// Build the index for a pack file, like `git index-pack`; `--stdin`
    /// stores a pack read from stdin in the repository.
    IndexPack {
        /// Write the index here instead of beside the pack.
        #[arg(short = 'o', value_name = "INDEX")]
        output: Option<String>,
        /// Read the pack from stdin.
        #[arg(long)]
        stdin: bool,
        /// Accepted for git compatibility (thin packs are completed from
        /// the repository's objects).
        #[arg(long)]
        fix_thin: bool,
        /// Write a .keep file, with this message.
        #[arg(long, value_name = "MSG", num_args = 0..=1, require_equals = true, default_missing_value = "")]
        keep: Option<String>,
        /// Check the existing index instead of writing one.
        #[arg(long)]
        verify: bool,
        /// With --verify, list the objects and the delta histogram.
        #[arg(long)]
        verify_stat: bool,
        /// With --verify, only the delta histogram.
        #[arg(long)]
        verify_stat_only: bool,
        /// Write a .rev reverse index (default pack.writeReverseIndex).
        #[arg(long)]
        rev_index: bool,
        /// Do not write a .rev reverse index.
        #[arg(long, overrides_with = "rev_index")]
        no_rev_index: bool,
        /// Accepted for git compatibility.
        #[arg(short = 'v', long = "verbose", hide = true)]
        verbose: bool,
        /// Accepted for git compatibility.
        #[arg(long, value_name = "N", hide = true)]
        threads: Option<String>,
        /// Accepted for git compatibility.
        #[arg(long, value_name = "MSG-ID", num_args = 0..=1, require_equals = true, hide = true)]
        strict: Option<String>,
        /// The pack file.
        pack: Option<String>,
    },
    /// Write each object of a pack read from stdin as a loose object, like
    /// `git unpack-objects`; objects the repository has are skipped.
    UnpackObjects {
        /// Only check the pack.
        #[arg(short = 'n')]
        dry_run: bool,
        /// Show no progress.
        #[arg(short = 'q')]
        quiet: bool,
        /// Accepted for git compatibility.
        #[arg(short = 'r', hide = true)]
        recover: bool,
        /// Accepted for git compatibility.
        #[arg(long, hide = true)]
        strict: bool,
    },
    /// Pack the objects listed on stdin, or reachable from the revisions
    /// on stdin with `--revs`, like `git pack-objects`; prints the pack's
    /// name.
    PackObjects {
        /// Write the pack to stdout instead of `<base-name>-<hash>.pack`.
        #[arg(long)]
        stdout: bool,
        /// Read revisions (`A`, `^B`, `--not`) from stdin and pack what
        /// they reach.
        #[arg(long)]
        revs: bool,
        /// With --revs, also every ref.
        #[arg(long)]
        all: bool,
        /// Show no progress.
        #[arg(short = 'q', long)]
        quiet: bool,
        /// Accepted for git compatibility (libgit2 picks deltas itself).
        #[arg(long, hide = true)]
        progress: bool,
        #[arg(long, value_name = "N", hide = true)]
        window: Option<String>,
        #[arg(long, value_name = "N", hide = true)]
        depth: Option<String>,
        #[arg(long, hide = true)]
        delta_base_offset: bool,
        #[arg(long, hide = true)]
        thin: bool,
        #[arg(long, hide = true)]
        non_empty: bool,
        #[arg(long, hide = true)]
        include_tag: bool,
        #[arg(long, hide = true)]
        local: bool,
        #[arg(long, hide = true)]
        incremental: bool,
        /// Where the pack goes: `<base-name>-<hash>.pack` and `.idx`.
        base_name: Option<String>,
        #[arg(skip)]
        input: Option<String>,
    },
    /// Remove loose objects that packs also hold, like `git prune-packed`.
    PrunePacked {
        /// Print the `rm -f` commands instead.
        #[arg(short = 'n', long)]
        dry_run: bool,
        /// Show no progress.
        #[arg(short = 'q', long)]
        quiet: bool,
    },
    /// Write info/refs and objects/info/packs for dumb transports, like
    /// `git update-server-info`.
    UpdateServerInfo {
        /// Rewrite the files even when they look current.
        #[arg(short = 'f', long)]
        force: bool,
    },
    /// Write a blob to a temporary `.merge_file_XXXXXX` file and print its
    /// name, like `git unpack-file`.
    UnpackFile {
        /// The blob.
        blob: String,
    },
    /// Resolve one unmerged path from its three stages, like the
    /// `git merge-one-file` script: `<orig> <ours> <theirs> <path>
    /// <orig mode> <our mode> <their mode>`, empty for a missing side.
    MergeOneFile {
        #[arg(allow_hyphen_values = true, num_args = 0..)]
        args: Vec<String>,
    },
    /// Run a merge program on each unmerged path, like `git merge-index
    /// [-o] [-q] <program> (-a | [--] <path>...)`; `git-merge-one-file`
    /// runs rgit's own.
    MergeIndex {
        #[arg(allow_hyphen_values = true, num_args = 0.., trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Write a bug report template with system details and open it in the
    /// editor, like `git bugreport`.
    Bugreport {
        /// Write the report (and the diagnostics zip) here.
        #[arg(short = 'o', long = "output-directory", value_name = "PATH")]
        output_directory: Option<String>,
        /// strftime format for the file name suffix.
        #[arg(short = 's', long, value_name = "FORMAT")]
        suffix: Option<String>,
        /// No suffix.
        #[arg(long, conflicts_with = "suffix")]
        no_suffix: bool,
        /// Also write a diagnostics zip: `stats` (default) or `all`.
        #[arg(long, value_name = "MODE", num_args = 0..=1, require_equals = true, default_missing_value = "stats")]
        diagnose: Option<String>,
    },
    /// Zip up repository statistics for a bug report, like `git diagnose`.
    Diagnose {
        /// Write the zip here.
        #[arg(short = 'o', long = "output-directory", value_name = "PATH")]
        output_directory: Option<String>,
        /// strftime format for the file name suffix.
        #[arg(short = 's', long, value_name = "FORMAT")]
        suffix: Option<String>,
        /// `stats` (default) or `all` (also the repository's own files).
        #[arg(long, value_name = "MODE")]
        mode: Option<String>,
    },
    /// Fetch the missing objects of a partial clone, like `git backfill`.
    Backfill {
        /// Accepted for git compatibility.
        #[arg(long, value_name = "N", hide = true)]
        min_batch_size: Option<String>,
        /// Accepted for git compatibility.
        #[arg(long, hide = true)]
        sparse: bool,
    },
}

impl Extra {
    /// Whether the command works outside a repository, as git's does.
    pub fn runs_without_repo(&self) -> bool {
        match self {
            Extra::VerifyPack { .. } | Extra::ShowIndex { .. } => true,
            Extra::Bugreport { diagnose, .. } => diagnose.is_none(),
            Extra::IndexPack { stdin, .. } => !stdin,
            _ => false,
        }
    }
}

/// Report git's `error:` on stderr and its exit code when printing git's
/// text; agents get the error itself.
fn failed(raw: bool, e: impl std::fmt::Display, code: i32) -> anyhow::Result<Output> {
    if !raw {
        return Err(CliError {
            message: e.to_string(),
            help: None,
            code,
        }
        .into());
    }
    eprintln!("error: {e}");
    crate::cli::set_exit_code(code);
    Ok(Output::new(String::new()))
}

/// git's usage error: the reason, then the command's usage, exit 129.
fn usage(message: &str, text: &str) -> anyhow::Error {
    CliError {
        message: format!("fatal: {message}\n\n{text}"),
        help: None,
        code: 129,
    }
    .into()
}

/// stdout, and stderr as git prints it; a failure exits `code`.
fn report(raw: bool, r: rgit_git::Report, code: i32) -> Output {
    if raw {
        eprint!("{}", r.err);
    }
    if r.failed {
        crate::cli::set_exit_code(code);
    }
    let mut out = Output::message(r.out.trim_end().to_owned());
    out.text = r.out;
    if !raw && !r.err.is_empty() {
        out = out.with("errors", r.err.trim_end().to_owned());
    }
    out
}

const REPLACE_USAGE: &str = "usage: git replace [-f] <object> <replacement>
   or: git replace [-f] --edit <object>
   or: git replace [-f] --graft <commit> [<parent>...]
   or: git replace [-f] --convert-graft-file
   or: git replace -d <object>...
   or: git replace [--format=<format>] [-l [<pattern>]]

    -l, --list            list replace refs
    -d, --delete          delete replace refs
    -e, --edit            edit existing object
    -g, --graft           change a commit's parents
    --convert-graft-file  convert existing graft file
    -f, --[no-]force      replace the ref if it exists
    --[no-]raw            do not pretty-print contents for --edit
    --[no-]format <format>
                          use this format
";

fn stdin_bytes() -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)?;
    Ok(buf)
}

fn fatal(message: impl Into<String>) -> anyhow::Error {
    CliError {
        message: message.into(),
        help: None,
        code: 128,
    }
    .into()
}

fn text(t: String) -> Output {
    let mut out = Output::message(t.trim_end().to_owned());
    out.text = t;
    out
}

/// The pack and object-file commands, in the repository at `git_dir` or,
/// for those that allow it, outside one.
pub fn packs(
    git_dir: Option<std::path::PathBuf>,
    command: Extra,
    raw: bool,
) -> anyhow::Result<Output> {
    let repo = || git_dir.clone().ok_or_else(CliError::not_a_repo);
    let sha1_only = |f: &Option<String>| match f.as_deref() {
        None | Some("sha1") => Ok(()),
        Some(f) => Err(fatal(format!("unknown hash algorithm '{f}'"))),
    };
    Ok(match command {
        Extra::VerifyPack {
            verbose,
            stat_only,
            object_format,
            packs,
        } => {
            sha1_only(&object_format)?;
            if packs.is_empty() {
                return usage_text(raw, VERIFY_PACK_USAGE);
            }
            let mut out = String::new();
            let mut bad = false;
            for p in &packs {
                let (text, err, ok) = rgit_git::verify_pack(p, verbose, stat_only);
                out.push_str(&text);
                if raw {
                    eprint!("{err}");
                }
                bad |= !ok;
            }
            crate::cli::set_exit(bad);
            text(out)
        }
        Extra::ShowIndex { object_format } => {
            sha1_only(&object_format)?;
            text(rgit_git::show_index(&stdin_bytes()?).map_err(|e| fatal(e.to_string()))?)
        }
        Extra::IndexPack {
            output,
            stdin,
            keep,
            verify,
            verify_stat,
            verify_stat_only,
            rev_index,
            no_rev_index,
            pack,
            ..
        } => {
            if pack.is_none() && !stdin {
                return usage_text(raw, INDEX_PACK_USAGE);
            }
            let o = rgit_git::IndexPackOpts {
                pack: pack.map(Into::into),
                index: output.map(Into::into),
                stdin: if stdin { Some(stdin_bytes()?) } else { None },
                keep,
                verify: verify || verify_stat || verify_stat_only,
                stat: verify_stat || verify_stat_only,
                stat_only: verify_stat_only,
                rev_index: if rev_index {
                    Some(true)
                } else {
                    no_rev_index.then_some(false)
                },
            };
            let dir = if stdin {
                Some(repo()?)
            } else {
                git_dir.clone()
            };
            text(rgit_git::index_pack(dir.as_deref(), &o).map_err(|e| fatal(e.to_string()))?)
        }
        Extra::UnpackObjects { dry_run, quiet, .. } => {
            let data = stdin_bytes()?;
            let n = rgit_git::unpack_objects(&repo()?, &data, dry_run)
                .map_err(|e| fatal(e.to_string()))?;
            if raw && !quiet && std::io::IsTerminal::is_terminal(&std::io::stderr()) {
                eprintln!("Unpacking objects: 100% ({n}/{n}), done.");
            }
            Output::new(String::new()).with("objects", n)
        }
        Extra::PackObjects {
            stdout,
            revs,
            all,
            base_name,
            input,
            ..
        } => {
            if stdout == base_name.is_some() {
                return usage_text(raw, PACK_OBJECTS_USAGE);
            }
            let input = match input {
                Some(i) => i,
                None => String::from_utf8_lossy(&stdin_bytes()?).into_owned(),
            };
            let lines: Vec<String> = input.lines().map(str::to_owned).collect();
            // --all implies --revs, as in git.
            let revs = revs || all;
            let o = rgit_git::PackObjectsOpts {
                revs: revs.then(|| lines.clone()),
                objects: if revs { Vec::new() } else { lines },
                all,
            };
            let (data, name) =
                rgit_git::pack_objects(&repo()?, &o).map_err(|e| fatal(e.to_string()))?;
            match base_name {
                None => {
                    use std::io::Write;
                    let mut out = std::io::stdout().lock();
                    out.write_all(&data)?;
                    out.flush()?;
                    Output::new(String::new())
                }
                Some(base) => {
                    rgit_git::write_pack_files(&base, &data, &name)?;
                    text(format!("{name}\n"))
                }
            }
        }
        Extra::PrunePacked { dry_run, .. } => {
            text(rgit_git::prune_packed_objects(&repo()?, dry_run)?)
        }
        Extra::UpdateServerInfo { force } => {
            rgit_git::update_server_info(&repo()?, force)?;
            text(String::new())
        }
        Extra::UnpackFile { blob } => {
            let name = rgit_git::unpack_file(&repo()?, &blob).map_err(|e| fatal(e.to_string()))?;
            text(format!("{name}\n"))
        }
        Extra::Bugreport {
            output_directory,
            suffix,
            no_suffix,
            diagnose,
        } => {
            let dir = std::path::PathBuf::from(output_directory.unwrap_or_default());
            let suffix = (!no_suffix).then(|| suffix.unwrap_or_else(|| "%Y-%m-%d-%H%M".into()));
            if let Some(mode) = diagnose.as_deref().filter(|m| *m != "none") {
                let dir_ = if dir.as_os_str().is_empty() {
                    std::path::Path::new(".")
                } else {
                    dir.as_path()
                };
                let zip = dir_.join(format!(
                    "git-diagnostics-{}.zip",
                    rgit_git::strftime_now(suffix.as_deref().unwrap_or("%Y-%m-%d-%H%M"))
                ));
                let log = rgit_git::diagnose(&repo()?, &zip, mode == "all")?;
                if raw {
                    print!("{log}");
                    eprintln!(
                        "\nDiagnostics complete.\nAll of the gathered info is captured in '{}'",
                        zip.display()
                    );
                }
            }
            let path = rgit_git::bugreport(git_dir.as_deref(), &dir, suffix.as_deref())
                .map_err(|e| fatal(e.to_string()))?;
            if raw {
                eprintln!("Created new report at '{}'.", path.display());
                let editor = std::env::var("GIT_EDITOR")
                    .ok()
                    .filter(|e| !e.is_empty())
                    .or_else(|| std::env::var("VISUAL").ok())
                    .or_else(|| std::env::var("EDITOR").ok())
                    .unwrap_or_else(|| "vi".into());
                if editor != ":" {
                    let ok = std::process::Command::new("sh")
                        .arg("-c")
                        .arg(format!("{editor} \"$@\""))
                        .arg(&editor)
                        .arg(&path)
                        .status()
                        .is_ok_and(|s| s.success());
                    crate::cli::set_exit(!ok);
                }
            }
            Output::new(String::new()).with("report", path.display().to_string())
        }
        Extra::Diagnose {
            output_directory,
            suffix,
            mode,
        } => {
            let mode = mode.unwrap_or_else(|| "stats".into());
            if !matches!(mode.as_str(), "stats" | "all") {
                return failed(raw, format!("invalid --mode value '{mode}'"), 129);
            }
            let dir = std::path::PathBuf::from(output_directory.unwrap_or_else(|| ".".into()));
            let zip = dir.join(format!(
                "git-diagnostics-{}.zip",
                rgit_git::strftime_now(suffix.as_deref().unwrap_or("%Y-%m-%d-%H%M"))
            ));
            let log = rgit_git::diagnose(&repo()?, &zip, mode == "all")?;
            if raw {
                eprintln!(
                    "\nDiagnostics complete.\nAll of the gathered info is captured in '{}'",
                    zip.display()
                );
            }
            let mut out = text(log);
            out.set("zip", zip.display().to_string());
            out
        }
        Extra::Backfill { .. } => {
            if rgit_git::is_partial_clone(&repo()?) {
                // ponytail: libgit2 cannot fetch objects on demand; a partial
                // clone's missing blobs stay missing.
                return Err(fatal(
                    "backfill of a partial clone is not supported: libgit2 cannot fetch missing objects",
                ));
            }
            text(String::new())
        }
        Extra::MergeOneFile { args } => {
            let r = rgit_git::merge_one_file(&repo()?, &args)?;
            report(raw, r, 1)
        }
        Extra::MergeIndex { args } => merge_index(&repo()?, &args, raw)?,
        _ => unreachable!("not a pack command"),
    })
}

const MERGE_INDEX_USAGE: &str =
    "usage: git merge-index [-o] [-q] <merge-program> (-a | [--] [<filename>...])\n";

/// `git merge-index`: run the program on each unmerged path named (or all
/// with -a) with merge-one-file's seven arguments.
fn merge_index(git_dir: &std::path::Path, args: &[String], raw: bool) -> anyhow::Result<Output> {
    if args.len() < 2 {
        return usage_text(raw, MERGE_INDEX_USAGE);
    }
    let mut i = 0;
    let one_shot = args[i] == "-o";
    if one_shot {
        i += 1;
    }
    let quiet = args.get(i).is_some_and(|a| a == "-q");
    if quiet {
        i += 1;
    }
    let Some(program) = args.get(i) else {
        return usage_text(raw, MERGE_INDEX_USAGE);
    };
    let stages = rgit_git::unmerged_stages(git_dir)?;
    let mut out = String::new();
    let mut errors = 0;
    let mut run_one = |a: &[String; 7]| -> anyhow::Result<()> {
        let ok = if program == "git-merge-one-file" {
            let r = rgit_git::merge_one_file(git_dir, a)?;
            out.push_str(&r.out);
            if raw {
                print!("{}", r.out);
                eprint!("{}", r.err);
            }
            !r.failed
        } else {
            std::process::Command::new(program)
                .args(a)
                .status()
                .map_err(|e| fatal(format!("cannot run {program}: {e}")))?
                .success()
        };
        if !ok {
            if one_shot {
                errors += 1;
            } else if quiet {
                return Err(CliError {
                    message: String::new(),
                    help: None,
                    code: 1,
                }
                .into());
            } else {
                return Err(fatal("merge program failed"));
            }
        }
        Ok(())
    };
    let mut force_file = false;
    for arg in &args[i + 1..] {
        if !force_file && arg.starts_with('-') {
            match arg.as_str() {
                "--" => force_file = true,
                "-a" => {
                    for s in &stages {
                        run_one(s)?;
                    }
                }
                _ => return Err(fatal(format!("git merge-index: unknown option {arg}"))),
            }
            continue;
        }
        match stages.iter().find(|s| s[3] == *arg) {
            Some(s) => run_one(s)?,
            None if rgit_git::index_has_path(git_dir, arg)? => {}
            None => return Err(fatal(format!("git merge-index: {arg} not in the cache"))),
        }
    }
    if errors > 0 && !quiet {
        return Err(fatal("merge program failed"));
    }
    crate::cli::set_exit_code(errors);
    let mut o = Output::message(out.trim_end().to_owned());
    if raw {
        o.text = String::new();
    }
    Ok(o)
}

/// git's usage text on stderr, exit 129.
fn usage_text(raw: bool, text: &str) -> anyhow::Result<Output> {
    if !raw {
        return Err(CliError {
            message: text.trim_end().to_owned(),
            help: None,
            code: 129,
        }
        .into());
    }
    eprint!("{text}");
    crate::cli::set_exit_code(129);
    Ok(Output::new(String::new()))
}

const VERIFY_PACK_USAGE: &str =
    "usage: git verify-pack [-v | --verbose] [-s | --stat-only] [--] <pack>.idx...

    -v, --[no-]verbose    verbose
    -s, --[no-]stat-only  show statistics only
    --[no-]object-format <hash>
                          specify the hash algorithm to use

";

const INDEX_PACK_USAGE: &str = "usage: git index-pack [-v] [-o <index-file>] [--keep | --keep=<msg>] [--[no-]rev-index] [--verify] [--strict[=<msg-id>=<severity>...]] [--fsck-objects[=<msg-id>=<severity>...]] (<pack-file> | --stdin [--fix-thin] [<pack-file>])\n";

const PACK_OBJECTS_USAGE: &str = "usage: git pack-objects [<options>] <base-name>
   or: git pack-objects [<options>] --stdout [<rev-list-opts>]";

pub fn run(backend: &Arc<dyn GitBackend>, command: Extra, raw: bool) -> anyhow::Result<Output> {
    let git_dir = backend.git_dir();
    match command {
        c @ (Extra::VerifyPack { .. }
        | Extra::ShowIndex { .. }
        | Extra::IndexPack { .. }
        | Extra::UnpackObjects { .. }
        | Extra::PackObjects { .. }
        | Extra::PrunePacked { .. }
        | Extra::UpdateServerInfo { .. }
        | Extra::UnpackFile { .. }
        | Extra::MergeOneFile { .. }
        | Extra::MergeIndex { .. }
        | Extra::Bugreport { .. }
        | Extra::Diagnose { .. }
        | Extra::Backfill { .. }) => packs(Some(git_dir), c, raw),
        Extra::CheckMailmap {
            stdin,
            mailmap_file,
            mailmap_blob,
            mut contacts,
            input,
        } => {
            if stdin {
                let text = match input {
                    Some(t) => t,
                    None => std::io::read_to_string(std::io::stdin())?,
                };
                contacts.extend(text.lines().map(str::to_owned));
            } else if contacts.is_empty() {
                return Err(CliError {
                    message: "no contacts specified".into(),
                    help: None,
                    code: 128,
                }
                .into());
            }
            let text = rgit_git::check_mailmap(&git_dir, &contacts, &mailmap_file, &mailmap_blob)?;
            let rows = text
                .lines()
                .map(|l| crate::obj! { "contact" => l })
                .collect();
            let mut out =
                Output::new(String::new()).list("contacts", rows, &["contact"], "0 contacts");
            out.text = text;
            Ok(out)
        }
        Extra::Replace {
            list,
            delete,
            edit,
            graft,
            convert_graft_file,
            force,
            raw: raw_edit,
            format,
            args,
        } => {
            let modes = [list, delete, edit, graft, convert_graft_file];
            if modes.iter().filter(|m| **m).count() > 1 {
                return Err(usage(
                    "options '--list', '--delete', '--edit', '--graft' and \
                     '--convert-graft-file' cannot be used together",
                    REPLACE_USAGE,
                ));
            }
            let list = list || !(delete || edit || graft || convert_graft_file) && args.is_empty();
            let write = !(list || delete);
            if force && !write {
                return Err(usage(
                    "-f only makes sense when writing a replacement",
                    REPLACE_USAGE,
                ));
            }
            if raw_edit && !edit {
                return Err(usage("--raw only makes sense with --edit", REPLACE_USAGE));
            }
            if format.is_some() && !list {
                return Err(usage(
                    "--format cannot be used when not listing",
                    REPLACE_USAGE,
                ));
            }
            let one = |r: Result<rgit_git::Report, rgit_git::GitError>| match r {
                Ok(r) => Ok(report(raw, r, 1)),
                Err(e) => failed(raw, e, 255),
            };
            if list {
                if args.len() > 1 {
                    return Err(usage(
                        "only one pattern can be given with -l",
                        REPLACE_USAGE,
                    ));
                }
                return match rgit_git::replace_list(
                    &git_dir,
                    args.first().map(String::as_str),
                    format.as_deref(),
                ) {
                    Ok(text) => {
                        let rows: Vec<crate::toon::Obj> = text
                            .lines()
                            .map(|l| crate::obj! { "replace" => l })
                            .collect();
                        let mut out = Output::new(String::new()).list(
                            "refs",
                            rows,
                            &["replace"],
                            "0 replace refs",
                        );
                        out.text = text;
                        Ok(out)
                    }
                    Err(e) => failed(raw, e, 255),
                };
            }
            if delete {
                if args.is_empty() {
                    return Err(usage("-d needs at least one argument", REPLACE_USAGE));
                }
                return one(rgit_git::replace_delete(&git_dir, &args));
            }
            if graft {
                if args.is_empty() {
                    return Err(usage("-g needs at least one argument", REPLACE_USAGE));
                }
                let r = rgit_git::replace_graft(&git_dir, &args, force, false);
                return match r {
                    Ok(r) => Ok(report(raw, r, 255)),
                    Err(e) => failed(raw, e, 255),
                };
            }
            if convert_graft_file {
                if !args.is_empty() {
                    return Err(usage(
                        "--convert-graft-file takes no argument",
                        REPLACE_USAGE,
                    ));
                }
                return one(rgit_git::replace_convert_grafts(&git_dir, force));
            }
            if edit {
                let [name] = &args[..] else {
                    return Err(usage("-e needs exactly one argument", REPLACE_USAGE));
                };
                let (old, kind, data) =
                    match rgit_git::replace_edit_export(&git_dir, name, force, raw_edit) {
                        Ok(x) => x,
                        Err(e) => return failed(raw, e, 255),
                    };
                let path = git_dir.join("EDIT_OBJECT");
                std::fs::write(&path, &data)?;
                crate::interactive::launch_editor(backend, &path)?;
                let data = std::fs::read(&path)?;
                let _ = std::fs::remove_file(&path);
                return match rgit_git::replace_edit_import(
                    &git_dir, old, kind, &data, raw_edit, force,
                ) {
                    Ok(()) => Ok(Output::new(String::new())),
                    Err(e) => failed(raw, e, 255),
                };
            }
            let [object, repl] = &args[..] else {
                return Err(usage("bad number of arguments", REPLACE_USAGE));
            };
            match rgit_git::replace_object(&git_dir, object, repl, force) {
                Ok(()) => Ok(Output::new(String::new())),
                Err(e) => failed(raw, e, 255),
            }
        }
    }
}
