//! A Model Context Protocol server over stdio, exposing rgit's git operations
//! as tools so an agent can drive the repository. Started with `rgit mcp`.
//!
//! The transport, protocol handshake, and framing come from the official `rmcp`
//! SDK; `ServerHandler` is implemented by hand so the whole tool catalog and the
//! `dispatch` map (tool name + JSON args -> backend call + rendered result) stay
//! in one place. `dispatch` is synchronous, so each call runs on a blocking task
//! rather than stalling the async runtime. The tool surface mirrors the CLI
//! (minus interactive prompts and the TUI).

use std::sync::Arc;

use rgit_git::{GitBackend, LogOptions, ResetMode};
use serde_json::{Map, Value, json};

use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::{RequestContext, RoleServer},
    transport::stdio,
};

/// The MCP server: a git backend shared across tool calls.
struct RgitMcp {
    backend: Arc<dyn GitBackend>,
}

/// Serve MCP over stdio until the client disconnects. Returns a process exit
/// code.
pub fn serve(backend: Arc<dyn GitBackend>) -> i32 {
    // The MCP surface is always plain text - never ANSI color.
    crate::render::set_color(false);
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("rgit: {e}");
            return 1;
        }
    };
    match runtime.block_on(run(backend)) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("rgit: mcp: {e}");
            1
        }
    }
}

async fn run(backend: Arc<dyn GitBackend>) -> anyhow::Result<()> {
    let service = RgitMcp { backend }.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

impl ServerHandler for RgitMcp {
    fn get_info(&self) -> ServerInfo {
        // from_build_env() reports rmcp's own name/version, so set ours.
        let mut info = Implementation::from_build_env();
        info.name = "rgit".to_owned();
        info.version = env!("CARGO_PKG_VERSION").to_owned();
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(info)
            .with_instructions(
                "Drive a git repository. Tools mirror git subcommands; results are compact plain text, one item per line.",
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let backend = self.backend.clone();
        let name = request.name.to_string();
        let args = Value::Object(request.arguments.unwrap_or_default());
        let result = tokio::task::spawn_blocking(move || dispatch(&backend, &name, &args))
            .await
            .map_err(|e| McpError::internal_error(format!("task join: {e}"), None))?;
        let call = match result {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Err(text) => CallToolResult::error(vec![ContentBlock::text(text)]),
        };
        Ok(call.into())
    }
}

/// Build a tool's JSON Schema from `(field, type, required)` triples. `type` is
/// a JSON Schema type, or `string[]`/`integer[]` for arrays.
fn schema(props: &[(&str, &str, bool)]) -> Map<String, Value> {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for (field, ty, req) in props {
        let field_schema = match *ty {
            "string[]" => json!({ "type": "array", "items": { "type": "string" } }),
            "integer[]" => json!({ "type": "array", "items": { "type": "integer" } }),
            t => json!({ "type": t }),
        };
        properties.insert((*field).to_owned(), field_schema);
        if *req {
            required.push(json!(field));
        }
    }
    let mut m = Map::new();
    m.insert("type".to_owned(), json!("object"));
    m.insert("properties".to_owned(), Value::Object(properties));
    m.insert("required".to_owned(), Value::Array(required));
    m
}

fn tool(name: &'static str, description: &'static str, props: &[(&str, &str, bool)]) -> Tool {
    Tool::new(name, description, schema(props))
}

/// The full tool catalog, mirroring the CLI surface.
fn tools() -> Vec<Tool> {
    let none: &[(&str, &str, bool)] = &[];
    vec![
        tool(
            "git_status",
            "Working-tree status: branch, ahead/behind, and changed files grouped by state.",
            none,
        ),
        tool(
            "git_log",
            "Recent commits, newest first.",
            &[
                ("limit", "integer", false),
                ("all", "boolean", false),
                ("author", "string", false),
            ],
        ),
        tool(
            "git_diff",
            "Diff. With no revs: staged changes (patch=true for a unified patch, else a diffstat). With `from`/`to`: that ref range.",
            &[
                ("from", "string", false),
                ("to", "string", false),
                ("patch", "boolean", false),
            ],
        ),
        tool(
            "git_show",
            "A commit's metadata and diffstat against its first parent.",
            &[("rev", "string", true)],
        ),
        tool(
            "git_blame",
            "Blame a working-tree file: each line with the commit that last touched it.",
            &[("path", "string", true)],
        ),
        tool(
            "git_refs",
            "All references: local branches, remote branches, and tags.",
            none,
        ),
        tool(
            "git_branches",
            "Local branch names, marking the current one.",
            none,
        ),
        tool("git_stashes", "The stash list.", none),
        tool(
            "git_remotes",
            "Configured remotes and their fetch URLs.",
            none,
        ),
        tool("git_worktrees", "Linked worktrees.", none),
        tool(
            "git_describe",
            "Describe a revision relative to the nearest tag (default HEAD).",
            &[("rev", "string", false)],
        ),
        tool(
            "git_stage",
            "Stage a path, or one hunk (`hunk` = new-side start line), or specific `lines` within that hunk.",
            &[
                ("path", "string", true),
                ("hunk", "integer", false),
                ("lines", "integer[]", false),
            ],
        ),
        tool(
            "git_unstage",
            "Unstage a path, or one hunk, or specific lines within that hunk.",
            &[
                ("path", "string", true),
                ("hunk", "integer", false),
                ("lines", "integer[]", false),
            ],
        ),
        tool(
            "git_stage_all",
            "Stage every change in the working tree.",
            none,
        ),
        tool("git_unstage_all", "Unstage everything back to HEAD.", none),
        tool(
            "git_discard",
            "Discard a path's unstaged changes (or a hunk/lines). Destructive.",
            &[
                ("path", "string", true),
                ("hunk", "integer", false),
                ("lines", "integer[]", false),
            ],
        ),
        tool(
            "git_resolve",
            "Resolve a conflicted path by taking `ours` (else theirs) and staging it.",
            &[("path", "string", true), ("ours", "boolean", false)],
        ),
        tool(
            "git_commit",
            "Commit the index with a message (runs hooks). `amend` replaces HEAD.",
            &[("message", "string", true), ("amend", "boolean", false)],
        ),
        tool(
            "git_extend",
            "Amend HEAD with the current index, keeping its message (no editor).",
            none,
        ),
        tool("git_fetch", "Fetch the current branch's remote.", none),
        tool(
            "git_pull",
            "Fetch and fast-forward the current branch.",
            none,
        ),
        tool(
            "git_push",
            "Push the current branch to its upstream.",
            &[
                ("force", "boolean", false),
                ("force_with_lease", "boolean", false),
                ("set_upstream", "boolean", false),
            ],
        ),
        tool(
            "git_checkout",
            "Check out a branch (or a revision/tag as detached HEAD).",
            &[("rev", "string", true)],
        ),
        tool(
            "git_merge",
            "Merge a revision into the current branch.",
            &[("rev", "string", true)],
        ),
        tool(
            "git_rebase",
            "Rebase the current branch onto a revision (aborts on conflict).",
            &[("onto", "string", true)],
        ),
        tool(
            "git_rebase_continue",
            "Continue an in-progress rebase after resolving conflicts.",
            none,
        ),
        tool(
            "git_rebase_skip",
            "Skip the current commit of an in-progress rebase.",
            none,
        ),
        tool("git_rebase_abort", "Abort an in-progress rebase.", none),
        tool(
            "git_cherry_pick",
            "Cherry-pick a commit onto HEAD.",
            &[("rev", "string", true)],
        ),
        tool(
            "git_revert",
            "Revert a commit on HEAD.",
            &[("rev", "string", true)],
        ),
        tool(
            "git_reset",
            "Reset HEAD to a revision. `mode` is soft, mixed (default), or hard.",
            &[("rev", "string", true), ("mode", "string", false)],
        ),
        tool(
            "git_undo",
            "Undo the last HEAD move, keeping uncommitted work.",
            none,
        ),
        tool(
            "git_bisect",
            "Run a `git bisect` subcommand (e.g. [\"start\",\"<bad>\",\"<good>\"], [\"good\"], [\"bad\"], [\"reset\"]).",
            &[("args", "string[]", true)],
        ),
        tool(
            "git_stash_push",
            "Stash the working tree and index (with an optional message).",
            &[("message", "string", false)],
        ),
        tool(
            "git_stash_pop",
            "Apply and drop the stash at `index` (default 0, the most recent).",
            &[("index", "integer", false)],
        ),
        tool(
            "git_stash_apply",
            "Apply the stash at `index` without dropping it (default 0).",
            &[("index", "integer", false)],
        ),
        tool(
            "git_stash_drop",
            "Drop the stash at `index` without applying it (default 0).",
            &[("index", "integer", false)],
        ),
        tool(
            "git_branch_create",
            "Create a branch at HEAD and check it out.",
            &[("name", "string", true)],
        ),
        tool(
            "git_branch_delete",
            "Delete a local branch.",
            &[("name", "string", true)],
        ),
        tool(
            "git_branch_rename",
            "Rename a local branch.",
            &[("old", "string", true), ("new", "string", true)],
        ),
        tool(
            "git_tag_create",
            "Create a tag at HEAD (annotated when `message` is given).",
            &[("name", "string", true), ("message", "string", false)],
        ),
        tool(
            "git_tag_delete",
            "Delete a tag.",
            &[("name", "string", true)],
        ),
        tool(
            "git_remote_add",
            "Add a remote.",
            &[("name", "string", true), ("url", "string", true)],
        ),
        tool(
            "git_remote_remove",
            "Remove a remote.",
            &[("name", "string", true)],
        ),
        tool(
            "git_worktree_add",
            "Create a linked worktree at `path` on a new branch `name`.",
            &[("name", "string", true), ("path", "string", true)],
        ),
        tool(
            "git_worktree_remove",
            "Remove a linked worktree.",
            &[("name", "string", true)],
        ),
        tool(
            "git_clean",
            "Remove every untracked file and directory. Destructive.",
            none,
        ),
        tool(
            "git_rm",
            "Remove a tracked path from the index and the working tree.",
            &[("path", "string", true)],
        ),
        tool(
            "git_mv",
            "Rename/move a tracked path.",
            &[("from", "string", true), ("to", "string", true)],
        ),
        tool(
            "git_run",
            "Run any git subcommand and return its stdout (the escape hatch).",
            &[("args", "string[]", true)],
        ),
    ]
}

/// Map a tool name and its arguments to a backend call and a rendered result.
fn dispatch(backend: &Arc<dyn GitBackend>, name: &str, args: &Value) -> Result<String, String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str);
    let req = |k: &str| s(k).ok_or_else(|| format!("{k} required"));
    let flag = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);
    let hunk = args.get("hunk").and_then(Value::as_u64).map(|n| n as u32);
    let lines: Vec<usize> = args
        .get("lines")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_u64().map(|n| n as usize))
                .collect()
        })
        .unwrap_or_default();
    let str_vec = |k: &str| -> Vec<String> {
        args.get(k)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let index = || args.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;

    match name {
        "git_status" => backend
            .status()
            .map(|s| crate::render::status(&s))
            .map_err(emap),
        "git_log" => {
            let opts = LogOptions {
                limit: args.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize,
                all: flag("all"),
                author: s("author").map(str::to_owned),
            };
            backend
                .log(&opts)
                .map(|e| crate::render::log(&e))
                .map_err(emap)
        }
        "git_diff" => {
            let patch = flag("patch");
            match (s("from"), s("to")) {
                (Some(from), Some(to)) => diff_out(backend.diff_refs(from, to), patch),
                (Some(rev), None) => diff_out(backend.diff_refs(rev, "HEAD"), patch),
                (None, _) if patch => backend.staged_patch().map_err(emap),
                (None, _) => backend
                    .status()
                    .map(|s| crate::render::diffstat(&s.staged))
                    .map_err(emap),
            }
        }
        "git_show" => backend
            .commit_details(req("rev")?)
            .map(|c| crate::render::commit_details(&c))
            .map_err(emap),
        "git_blame" => backend
            .blame(req("path")?)
            .map(|b| crate::render::blame(&b))
            .map_err(emap),
        "git_refs" => backend
            .refs()
            .map(|r| crate::render::refs(&r))
            .map_err(emap),
        "git_branches" => {
            let current = backend.status().ok().and_then(|s| s.head.branch);
            backend
                .local_branches()
                .map(|b| crate::render::branches(&b, current.as_deref()))
                .map_err(emap)
        }
        "git_stashes" => backend
            .status()
            .map(|s| crate::render::stashes(&s.stashes))
            .map_err(emap),
        "git_remotes" => backend
            .remotes()
            .map(|r| crate::render::remotes(&r))
            .map_err(emap),
        "git_worktrees" => backend
            .worktrees()
            .map(|w| crate::render::worktrees(&w))
            .map_err(emap),
        "git_describe" => backend.describe(s("rev").unwrap_or("HEAD")).map_err(emap),

        "git_stage" => done(match (hunk, lines.as_slice()) {
            (Some(h), l) if !l.is_empty() => backend.stage_lines(req("path")?, h, l),
            (Some(h), _) => backend.stage_hunk(req("path")?, h),
            (None, _) => backend.stage_file(req("path")?),
        }),
        "git_unstage" => done(match (hunk, lines.as_slice()) {
            (Some(h), l) if !l.is_empty() => backend.unstage_lines(req("path")?, h, l),
            (Some(h), _) => backend.unstage_hunk(req("path")?, h),
            (None, _) => backend.unstage_file(req("path")?),
        }),
        "git_stage_all" => done(backend.stage_all()),
        "git_unstage_all" => done(backend.unstage_all()),
        "git_discard" => done(match (hunk, lines.as_slice()) {
            (Some(h), l) if !l.is_empty() => backend.discard_lines(req("path")?, h, l),
            (Some(h), _) => backend.discard_hunk(req("path")?, h),
            (None, _) => backend.discard_file(req("path")?),
        }),
        "git_resolve" => done(backend.resolve_conflict(req("path")?, flag("ours"))),

        "git_commit" => {
            let message = req("message")?;
            let r = if flag("amend") {
                backend.amend(message)
            } else {
                backend.commit(message)
            };
            r.map_err(emap)?;
            Ok(backend.commit_report().join("\n"))
        }
        "git_extend" => {
            backend.commit_extend().map_err(emap)?;
            Ok(backend.commit_report().join("\n"))
        }

        // The MCP surface has no console, so progress is discarded.
        "git_fetch" => done(backend.fetch(&|_| {})),
        "git_pull" => done(backend.pull(&|_| {})),
        "git_push" => done(backend.push(
            flag("force"),
            flag("force_with_lease"),
            flag("set_upstream"),
            &|_| {},
        )),

        "git_checkout" => {
            let rev = req("rev")?;
            let is_branch = backend
                .local_branches()
                .map(|bs| bs.iter().any(|b| b == rev))
                .unwrap_or(false);
            done(if is_branch {
                backend.checkout_branch(rev)
            } else {
                backend.checkout_detached(rev)
            })
        }
        "git_merge" => done(backend.merge(req("rev")?, &|_| {})),
        "git_rebase" => done(backend.rebase_onto(req("onto")?, &|_| {})),
        "git_rebase_continue" => done(backend.rebase_continue()),
        "git_rebase_skip" => done(backend.rebase_skip()),
        "git_rebase_abort" => done(backend.rebase_abort()),
        "git_cherry_pick" => done(backend.cherry_pick(req("rev")?)),
        "git_revert" => done(backend.revert(req("rev")?)),
        "git_reset" => {
            let mode = match s("mode").unwrap_or("mixed") {
                "soft" => ResetMode::Soft,
                "hard" => ResetMode::Hard,
                _ => ResetMode::Mixed,
            };
            done(backend.reset(req("rev")?, mode))
        }
        "git_undo" => done(backend.undo()),
        "git_bisect" => match backend.bisect(&str_vec("args")) {
            Ok(out) if out.is_empty() => Ok("ok".to_owned()),
            Ok(out) => Ok(out),
            Err(e) => Err(e.to_string()),
        },

        "git_stash_push" => done(match s("message") {
            Some(m) => backend.stash_push_message(m),
            None => backend.stash_push(),
        }),
        "git_stash_pop" => done(backend.stash_pop(index())),
        "git_stash_apply" => done(backend.stash_apply(index())),
        "git_stash_drop" => done(backend.stash_drop(index())),

        "git_branch_create" => done(backend.create_branch(req("name")?)),
        "git_branch_delete" => done(backend.delete_branch(req("name")?)),
        "git_branch_rename" => done(backend.rename_branch(req("old")?, req("new")?)),

        "git_tag_create" => done(backend.create_tag(req("name")?, s("message").unwrap_or(""))),
        "git_tag_delete" => done(backend.delete_tag(req("name")?)),

        "git_remote_add" => done(backend.add_remote(req("name")?, req("url")?)),
        "git_remote_remove" => done(backend.remove_remote(req("name")?)),

        "git_worktree_add" => done(backend.add_worktree(req("name")?, req("path")?)),
        "git_worktree_remove" => done(backend.remove_worktree(req("name")?)),

        "git_clean" => done(backend.clean()),
        "git_rm" => done(backend.remove_path(req("path")?)),
        "git_mv" => done(backend.move_path(req("from")?, req("to")?)),
        "git_run" => backend.git(&str_vec("args")).map_err(emap),

        other => Err(format!("unknown tool: {other}")),
    }
}

fn emap(e: rgit_git::GitError) -> String {
    e.to_string()
}

fn done(r: Result<(), rgit_git::GitError>) -> Result<String, String> {
    r.map(|()| "ok".to_owned()).map_err(emap)
}

fn diff_out(
    files: Result<Vec<rgit_git::FileDiff>, rgit_git::GitError>,
    patch: bool,
) -> Result<String, String> {
    files
        .map(|f| {
            if patch {
                crate::render::patch(&f)
            } else {
                crate::render::diffstat(&f)
            }
        })
        .map_err(emap)
}
