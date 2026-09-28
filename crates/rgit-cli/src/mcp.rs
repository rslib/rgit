//! A Model Context Protocol server over stdio, exposing rgit's git operations
//! as tools so an agent can drive the repository. Started with `rgit mcp`.
//!
//! The transport, protocol handshake, and framing come from the official `rmcp`
//! SDK; `ServerHandler` is implemented by hand so the whole tool catalog and the
//! `dispatch` map stay in one place. A tool with a CLI equivalent builds that
//! `cli::Command` and runs it through `axi::run`, so its result (TOON text plus
//! the same data as structured content) matches `rgit --toon <cmd>`. `dispatch`
//! is synchronous, so each call runs on a blocking task rather than stalling the
//! async runtime. Tools never prompt.

use std::sync::Arc;

use rgit_git::{Git2Backend, GitBackend, GrepQuery};
use serde_json::{Map, Value, json};

use crate::cli::{
    BisectCmd, BranchCmd, BranchOpts, CliError, Command, DiffFormat, FlowCmd, IndexCmd, LanesCmd,
    NotesCmd, Plumbing, RebaseFlags, RemoteCmd, StackCmd, StashCmd, StashPush, WorkspaceCmd,
    WorktreeCmd,
};
use crate::output::Output;
use crate::toon::{Node, Obj};

use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
        GetPromptRequestParams, GetPromptResponse, GetPromptResult, Implementation,
        ListPromptsResult, ListToolsResult, PaginatedRequestParams, Prompt, PromptMessage, Role,
        ServerCapabilities, ServerInfo, Tool,
    },
    service::{RequestContext, RoleServer},
    transport::stdio,
};

/// The MCP server: resolves each tool call to a git backend by the optional
/// `repo` argument, defaulting to the repo the server was started in.
struct RgitMcp {
    registry: Registry,
    /// When true (the HTTP transport, mirroring the read-only web viewer), only
    /// inspection tools are listed and any mutating tool is refused. Stdio
    /// (`rgit mcp`, run locally by the user) is never read-only.
    read_only: bool,
}

/// Tools that only inspect the repository - safe to expose over the read-only
/// HTTP endpoint. Everything not listed mutates state (or the network) and is
/// hidden and refused in read-only mode. New tools default to write (hidden)
/// until explicitly added here.
const READ_ONLY_TOOLS: &[&str] = &[
    "git_status",
    "git_log",
    "git_diff",
    "git_show",
    "git_blame",
    "git_refs",
    "git_tree",
    "git_blob",
    "git_files",
    "git_grep",
    "git_remotes",
    "git_remote_get_url",
    "git_worktrees",
    "git_describe",
    "git_smartlog",
    "git_oplog",
    "git_stashes",
    "git_stash_show",
    "git_branches",
    "git_lanes_list",
    "git_stack_list",
    "git_workspace_list",
    "git_flow_status",
    "git_config",
    "git_notes",
    "git_fsck",
    "git_rev_parse",
    "git_ls_files",
    "git_ls_tree",
    "git_cat_file",
    "git_show_ref",
    "git_for_each_ref",
    "git_rev_list",
    "git_merge_base",
    "git_reflog",
    "git_shortlog",
    "git_grep_tracked",
    "git_check_ignore",
    "git_var",
    "git_symbolic_ref",
    "git_count_objects",
];

/// Whether a tool only reads (may be served over the read-only HTTP endpoint).
fn is_read_only(name: &str) -> bool {
    READ_ONLY_TOOLS.contains(&name)
}

/// How a tool call's `repo` argument is resolved to a backend. The mode is
/// chosen by transport: stdio is local and trusted, HTTP never is.
enum Mode {
    /// Local stdio (`rgit mcp`): `repo` is any filesystem path the process can
    /// read. Safe because the user runs it in their own shell; never used over
    /// the network.
    LocalPath { default: Arc<dyn GitBackend> },
    /// HTTP single-repo serve: only the one served repo, addressed by an empty
    /// `repo` or its exact name. No path is ever accepted from the client.
    Single {
        name: String,
        backend: Arc<dyn GitBackend>,
    },
    /// HTTP multi-repo serve (`--root`): `repo` is the name of a directory under
    /// the root and may not escape it; no default repo.
    Rooted { root: std::path::PathBuf },
}

/// Maps a tool call's `repo` argument to a backend, per the active [`Mode`].
struct Registry {
    mode: Mode,
    cache: std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, Arc<dyn GitBackend>>>,
}

impl Registry {
    /// Local stdio resolver: trusts arbitrary filesystem paths. NEVER mount this
    /// over HTTP - use [`single`](Self::single) or [`rooted`](Self::rooted).
    fn new(default: Arc<dyn GitBackend>) -> Self {
        Self::with_mode(Mode::LocalPath { default })
    }

    /// HTTP single-repo resolver: the client may address only the served repo by
    /// name (or an empty `repo`); no filesystem paths are honored.
    fn single(name: String, backend: Arc<dyn GitBackend>) -> Self {
        Self::with_mode(Mode::Single { name, backend })
    }

    /// HTTP multi-repo resolver: `repo` names a directory under `root`, confined
    /// to it. There is no default, so every call must name a repo.
    fn rooted(root: std::path::PathBuf) -> Self {
        Self::with_mode(Mode::Rooted { root })
    }

    fn with_mode(mode: Mode) -> Self {
        Self {
            mode,
            cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn resolve(&self, repo: Option<&str>) -> Result<Arc<dyn GitBackend>, String> {
        let asked = repo.map(str::trim).filter(|s| !s.is_empty());
        match &self.mode {
            Mode::LocalPath { default } => match asked {
                None => Ok(default.clone()),
                Some(path) => self.open(std::path::PathBuf::from(path), path, None),
            },
            Mode::Single { name, backend } => match asked {
                None => Ok(backend.clone()),
                Some(n) if n == name => Ok(backend.clone()),
                Some(n) => Err(format!(
                    "unknown repo {n:?}: this server serves only {name:?}"
                )),
            },
            Mode::Rooted { root } => match asked {
                None => Err(
                    "no default repo: this server was started with --root, so every call must set `repo`"
                        .to_owned(),
                ),
                // A single path segment only: no separators, no parent refs.
                Some(n) if n.contains('/') || n.contains('\\') || n.contains("..") => {
                    Err(format!("invalid repo name {n:?}: expected a name under the root"))
                }
                Some(n) => self.open(root.join(n), n, Some(root)),
            },
        }
    }

    /// Canonicalize, optionally confine under `root`, then discover and cache.
    fn open(
        &self,
        target: std::path::PathBuf,
        name: &str,
        confine: Option<&std::path::PathBuf>,
    ) -> Result<Arc<dyn GitBackend>, String> {
        let canon =
            std::fs::canonicalize(&target).map_err(|e| format!("no such repo {name:?}: {e}"))?;
        if let Some(root) = confine {
            let root_canon = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
            if !canon.starts_with(&root_canon) {
                return Err(format!("repo {name:?} is outside the served root"));
            }
        }
        if let Some(b) = self.cache.lock().expect("registry mutex").get(&canon) {
            return Ok(b.clone());
        }
        let backend: Arc<dyn GitBackend> =
            Arc::new(Git2Backend::discover(&canon).map_err(|e| e.to_string())?);
        self.cache
            .lock()
            .expect("registry mutex")
            .insert(canon, backend.clone());
        Ok(backend)
    }
}

/// Serve MCP over stdio until the client disconnects. Returns a process exit
/// code.
pub fn serve(backend: Arc<dyn GitBackend>) -> i32 {
    // The MCP surface never carries ANSI color.
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
    let service = RgitMcp {
        registry: Registry::new(backend),
        // Local stdio, run by the user in their own shell: full read/write.
        read_only: false,
    }
    .serve(stdio())
    .await?;
    service.waiting().await?;
    Ok(())
}

/// An axum router exposing the same MCP surface over HTTP (POST /mcp, Streamable
/// HTTP) so a running `rgit serve` feeds both the browser and agents. Confined
/// to the one served repo, addressed by `name` or an empty `repo` - the client
/// can never open an arbitrary filesystem path over the network.
pub fn http_router(name: String, backend: Arc<dyn GitBackend>) -> axum::Router {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpService, session::local::LocalSessionManager,
    };
    // No ANSI color over the wire.
    crate::render::set_color(false);
    let service = StreamableHttpService::new(
        move || {
            Ok(RgitMcp {
                registry: Registry::single(name.clone(), backend.clone()),
                // Network endpoint mirroring the read-only web viewer.
                read_only: true,
            })
        },
        Arc::new(LocalSessionManager::default()),
        Default::default(),
    );
    axum::Router::new().nest_service("/mcp", service)
}

/// Like [`http_router`] but for a managed root serving many repos: there is no
/// default repo, so each tool call names one via `repo` (resolved under `root`).
pub fn http_router_rooted(root: std::path::PathBuf) -> axum::Router {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpService, session::local::LocalSessionManager,
    };
    crate::render::set_color(false);
    let service = StreamableHttpService::new(
        move || {
            Ok(RgitMcp {
                registry: Registry::rooted(root.clone()),
                read_only: true,
            })
        },
        Arc::new(LocalSessionManager::default()),
        Default::default(),
    );
    axum::Router::new().nest_service("/mcp", service)
}

impl ServerHandler for RgitMcp {
    fn get_info(&self) -> ServerInfo {
        // from_build_env() reports rmcp's own name/version, so set ours.
        let mut info = Implementation::from_build_env();
        info.name = "rgit".to_owned();
        info.version = env!("CARGO_PKG_VERSION").to_owned();
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .build(),
        )
        .with_server_info(info)
        .with_instructions(
            "Drive a git repository. Results are TOON, the same output as `rgit --toon <command>`, \
             with the same data attached as structured content. Lists come back as tables with \
             counts and an explicit empty state; `fields` adds columns and `full` turns off \
             truncation where a tool takes them. A `help` list gives next steps, and errors come \
             back as `error` plus `help`. Repeating a create or delete that is already done is a \
             no-op, not an error. Key flows: inspect with git_status / git_smartlog / git_log \
             before acting; stage (git_stage / git_stage_all) then git_commit with a clear \
             message; sync with git_fetch then git_pull or git_rebase. Every destructive tool is \
             snapshotted, so git_undo reverses the last operation (recovering uncommitted work), \
             git_redo replays it, and git_oplog lists the history. For step-by-step playbooks, \
             read the prompts: commit_changes, sync_with_remote, resolve_conflicts, \
             safe_experiment, review_local_work, start_feature.",
        )
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        let prompts = SKILLS
            .iter()
            .map(|(name, desc, _)| Prompt::new(*name, Some(*desc), None))
            .collect();
        Ok(ListPromptsResult::with_all_items(prompts))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, McpError> {
        let skill = SKILLS
            .iter()
            .find(|(name, _, _)| *name == request.name)
            .ok_or_else(|| {
                McpError::invalid_params(format!("unknown prompt: {}", request.name), None)
            })?;
        let mut result = GetPromptResult::default();
        result.description = Some(skill.1.to_owned());
        result.messages = vec![PromptMessage::new_text(Role::User, skill.2)];
        Ok(result.into())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let mut items = tools();
        if self.read_only {
            items.retain(|t| is_read_only(&t.name));
        }
        Ok(ListToolsResult::with_all_items(items))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let name = request.name.to_string();
        if self.read_only && !is_read_only(&name) {
            let refused = anyhow::Error::new(CliError {
                message: format!(
                    "{name} is a write operation and is disabled on this read-only endpoint"
                ),
                help: Some("Run `rgit mcp` (stdio) for full access".to_owned()),
                code: 1,
            });
            return Ok(reply(failure(&refused), true).into());
        }
        let args = Value::Object(request.arguments.unwrap_or_default());
        let backend = match self
            .registry
            .resolve(args.get("repo").and_then(Value::as_str))
        {
            Ok(b) => b,
            Err(e) => return Ok(reply(failure(&anyhow::anyhow!(e)), true).into()),
        };
        let result = tokio::task::spawn_blocking(move || dispatch(&backend, &name, &args))
            .await
            .map_err(|e| McpError::internal_error(format!("task join: {e}"), None))?;
        let call = match result {
            Ok(value) => reply(value, false),
            Err(error) => reply(failure(&error), true),
        };
        Ok(call.into())
    }
}

/// A tool result: the TOON text as content and the same object as structured
/// content.
fn reply(value: Obj, is_error: bool) -> CallToolResult {
    let content = vec![ContentBlock::text(crate::toon::encode(&value))];
    let mut result = if is_error {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    };
    result.structured_content = serde_json::to_value(Node::Obj(value)).ok();
    result
}

/// An error as `error` plus `help`, the shape `rgit --toon` prints on failure.
fn failure(error: &anyhow::Error) -> Obj {
    let (message, help, _) = crate::output::translate(error);
    let help: Vec<String> = help.iter().map(|h| as_tool_call(h)).collect();
    let mut value = crate::obj! { "error" => message };
    if !help.is_empty() {
        value.push(("help".to_owned(), help.into()));
    }
    value
}

/// Embedded skills: guided, multi-step playbooks exposed as MCP prompts, so an
/// agent can fetch the flow rather than infer it from the raw tool list. Each is
/// `(name, one-line description, playbook body)`.
const SKILLS: &[(&str, &str, &str)] = &[
    (
        "commit_changes",
        "Review the working tree and make a clean commit.",
        "Goal: commit the current work as one clear change.\n\
         1. Call git_status to see what changed and whether anything is already staged.\n\
         2. Review the actual diff with git_diff (add patch=true for the full unified patch).\n\
         3. Stage what belongs in this commit: git_stage for one path (or a hunk/lines), or \
         git_stage_all for everything. Keep unrelated changes out.\n\
         4. Commit with git_commit and a concise message (imperative mood, e.g. \"fix parser off-\
         by-one\"). This runs the repo's hooks; if a hook fails, fix and retry.\n\
         5. Confirm with git_status (clean) and git_log (your commit on top).\n\
         If you commit the wrong thing, git_undo restores the pre-commit state, staged changes and \
         all.",
    ),
    (
        "sync_with_remote",
        "Bring your branch up to date with its remote.",
        "Goal: integrate upstream changes into your branch.\n\
         1. git_fetch to update remote-tracking refs without touching your work.\n\
         2. git_status to see ahead/behind counts.\n\
         3. If you are only behind (no local commits to keep linear), git_pull to fast-forward.\n\
         4. If you have local commits, prefer git_rebase with onto set to the upstream (e.g. \
         \"origin/main\") to keep history linear.\n\
         5. On conflict, follow the resolve_conflicts skill, then git_rebase_continue.\n\
         6. git_push when done (add set_upstream=true the first time).",
    ),
    (
        "resolve_conflicts",
        "Resolve a merge or rebase conflict and continue.",
        "Goal: finish a merge/rebase that stopped on conflicts.\n\
         1. git_status lists the conflicted paths.\n\
         2. For each path, either edit the file to the desired result and git_stage it, or take one \
         side wholesale with git_resolve (ours=true for your side, false for theirs).\n\
         3. When git_status shows no remaining conflicts, continue: git_rebase_continue for a \
         rebase, or git_commit for a merge.\n\
         4. If it is going badly, git_rebase_abort returns to the pre-rebase state, or git_undo \
         reverses the operation entirely.",
    ),
    (
        "safe_experiment",
        "Try a risky change with a guaranteed way back.",
        "Goal: make a risky change knowing you can fully revert.\n\
         Every destructive rgit operation is snapshotted first, so you do not need a manual backup.\n\
         1. Do the change (reset, rebase, discard, checkout, etc.).\n\
         2. If the result is wrong, git_undo restores HEAD, the branch, AND the working tree \
         (including uncommitted edits) to the state before that operation.\n\
         3. git_oplog shows the recent operations; git_redo replays one you undid.\n\
         Note: this covers rgit operations only; changes made with a separate plain `git` command \
         are not snapshotted.",
    ),
    (
        "review_local_work",
        "Understand the current state before acting.",
        "Goal: get oriented in the repository.\n\
         1. git_smartlog shows your local/draft commits and the trunk they branch from (marks HEAD \
         and the trunk) - the fastest way to see your work.\n\
         2. git_status shows uncommitted changes and the current branch.\n\
         3. git_diff shows unstaged changes (cached=true for staged, patch=true for the patch); \
         git_log shows recent history; git_show a revision for one commit's detail.\n\
         Start here before any change so you act on facts, not assumptions.",
    ),
    (
        "start_feature",
        "Start a new feature branch and push it.",
        "Goal: begin isolated work on a new branch.\n\
         1. git_smartlog / git_status to confirm a clean starting point on the trunk.\n\
         2. git_branch_create <name> to create and switch to the feature branch.\n\
         3. Make changes, then use the commit_changes skill to stage and commit.\n\
         4. git_push with set_upstream=true to publish the branch and set tracking.\n\
         5. Keep it current with the sync_with_remote skill as the trunk moves.",
    ),
];

/// Optional table columns beyond a tool's defaults.
const FIELDS: (&str, &str, bool) = ("fields", "string[]", false);
/// Optional switch that turns off truncation of long text and lists.
const FULL: (&str, &str, bool) = ("full", "boolean", false);

/// Build a tool's JSON Schema from `(field, type, required)` triples. `type` is
/// a JSON Schema type, or `string[]`/`integer[]` for arrays.
fn schema(props: &[(&str, &str, bool)]) -> Map<String, Value> {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for (field, ty, req) in props {
        let mut field_schema = match *ty {
            "string[]" => json!({ "type": "array", "items": { "type": "string" } }),
            "integer[]" => json!({ "type": "array", "items": { "type": "integer" } }),
            "paths" => json!({ "anyOf": [
                { "type": "string" },
                { "type": "array", "items": { "type": "string" } },
            ] }),
            t => json!({ "type": t }),
        };
        let about = match *field {
            "fields" => Some("Extra table columns to return beyond the defaults."),
            "full" => Some("Return long text and long lists without truncation."),
            _ => None,
        };
        if let (Some(about), Some(obj)) = (about, field_schema.as_object_mut()) {
            obj.insert("description".to_owned(), json!(about));
        }
        properties.insert((*field).to_owned(), field_schema);
        if *req {
            required.push(json!(field));
        }
    }
    // Every tool accepts an optional `repo` to target another repository;
    // empty or omitted uses the repo the server was started in.
    properties.insert(
        "repo".to_owned(),
        json!({
            "type": "string",
            "description": "Repository to act on; empty or omitted uses the server's current repo.",
        }),
    );
    let mut m = Map::new();
    m.insert("type".to_owned(), json!("object"));
    m.insert("properties".to_owned(), Value::Object(properties));
    m.insert("required".to_owned(), Value::Array(required));
    m
}

fn tool(name: &'static str, description: &'static str, props: &[(&str, &str, bool)]) -> Tool {
    Tool::new(name, description, schema(props))
}

/// The tool catalog as plain data for the web `/agent` reference page, derived
/// from the same [`tools`] declarations the MCP server serves. The implicit
/// `repo` argument is dropped here (the page documents it once, globally).
pub fn tool_catalog() -> Vec<rgit_web::McpTool> {
    tools()
        .into_iter()
        .map(|t| {
            let required: std::collections::HashSet<String> = t
                .input_schema
                .get("required")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let mut args: Vec<rgit_web::McpArg> = t
                .input_schema
                .get("properties")
                .and_then(Value::as_object)
                .map(|props| {
                    props
                        .iter()
                        .filter(|(k, _)| k.as_str() != "repo")
                        .map(|(k, v)| rgit_web::McpArg {
                            name: k.clone(),
                            ty: v
                                .get("type")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                            required: required.contains(k),
                        })
                        .collect()
                })
                .unwrap_or_default();
            // Required args first, then alphabetical, for a stable readable order.
            args.sort_by(|a, b| {
                b.required
                    .cmp(&a.required)
                    .then_with(|| a.name.cmp(&b.name))
            });
            let name = t.name.to_string();
            let writes = !is_read_only(&name);
            rgit_web::McpTool {
                name,
                description: t.description.map(|d| d.to_string()).unwrap_or_default(),
                writes,
                args,
            }
        })
        .collect()
}

/// The full tool catalog, mirroring the CLI surface.
fn tools() -> Vec<Tool> {
    let none: &[(&str, &str, bool)] = &[];
    vec![
        tool(
            "git_status",
            "Working-tree status: branch, upstream ahead/behind, change counts, and a files table \
             (path, staged, unstaged) that says so when the tree is clean. Ends with next-step help. \
             `paths` limits it to files, folders or globs; `untracked` is no/normal/all; \
             `ignored` also lists ignored files.",
            &[
                FIELDS,
                ("paths", "paths", false),
                ("untracked", "string", false),
                ("ignored", "boolean", false),
            ],
        ),
        tool(
            "git_log",
            "Commits newest first (limit, default 20; skip pages): a table of id, summary, \
             author, when, with a count when more exist. Filter with author, since, until, path \
             (one or many), grep \
             (message regex; ignore_case), merges/no_merges; rev (one or many: `main`, `^main`, \
             `A..B`, `A...B`) or all picks the start; first_parent, reverse, follow (one path \
             across renames). patch, stat, name_only, name_status or numstat add each commit's \
             changes as text. Extra fields: oid, parents, refs, unpushed.",
            &[
                ("limit", "integer", false),
                ("skip", "integer", false),
                ("all", "boolean", false),
                ("author", "string", false),
                ("since", "string", false),
                ("until", "string", false),
                ("rev", "paths", false),
                ("path", "paths", false),
                ("grep", "paths", false),
                ("ignore_case", "boolean", false),
                ("first_parent", "boolean", false),
                ("merges", "boolean", false),
                ("no_merges", "boolean", false),
                ("reverse", "boolean", false),
                ("follow", "boolean", false),
                ("patch", "boolean", false),
                ("stat", "boolean", false),
                ("name_only", "boolean", false),
                ("name_status", "boolean", false),
                ("numstat", "boolean", false),
                FIELDS,
            ],
        ),
        tool(
            "git_diff",
            "Changes as a diffstat table (path, added, removed) with totals. Default is unstaged \
             changes; cached=true for staged; from alone diffs that revision against the working \
             tree (the index with cached), or takes a range `A..B` / `A...B` (from the merge \
             base); from+to diffs two revisions. paths limits to files, folders or globs. \
             patch=true returns the unified patch (truncated unless full), with `unified` context \
             lines and ignore_all_space; name_only, name_status and numstat list files.",
            &[
                ("from", "string", false),
                ("to", "string", false),
                ("paths", "paths", false),
                ("cached", "boolean", false),
                ("patch", "boolean", false),
                ("name_only", "boolean", false),
                ("name_status", "boolean", false),
                ("numstat", "boolean", false),
                ("unified", "integer", false),
                ("ignore_all_space", "boolean", false),
                FIELDS,
                FULL,
            ],
        ),
        tool(
            "git_show",
            "One commit (default HEAD): id, author, when, subject, body, totals, and a \
             changed-files table (paths limits it; no_patch drops it). patch=true returns its \
             patch instead (truncated unless full); name_only/name_status/numstat list files. \
             rev `<rev>:<path>` prints a file (or folder) at a revision, `:<path>` the staged \
             file; several revs print each.",
            &[
                ("rev", "paths", false),
                ("paths", "paths", false),
                ("patch", "boolean", false),
                ("name_only", "boolean", false),
                ("name_status", "boolean", false),
                ("numstat", "boolean", false),
                ("no_patch", "boolean", false),
                FIELDS,
                FULL,
            ],
        ),
        tool(
            "git_blame",
            "Blame a file (working tree, or as of `rev`): a table of line, id, author, text. Shows \
             the first 200 lines; `lines` takes START,END or START,+COUNT, and help names the \
             next range.",
            &[
                ("path", "string", true),
                ("rev", "string", false),
                ("lines", "string", false),
                FIELDS,
            ],
        ),
        tool(
            "git_refs",
            "All references as a table of name, kind (local/remote/tag), head.",
            &[FIELDS],
        ),
        tool(
            "git_tree",
            "One directory of a revision's tree (rev defaults to HEAD, path to the root): a table \
             of name, kind (dir/file), size.",
            &[("rev", "string", false), ("path", "string", false), FIELDS],
        ),
        tool(
            "git_blob",
            "A file's contents at a revision (rev defaults to HEAD) with its path and size. Long \
             content is truncated unless full.",
            &[("path", "string", true), ("rev", "string", false), FULL],
        ),
        tool(
            "git_files",
            "Every file path in a revision's tree (rev defaults to HEAD). The first 200 unless full.",
            &[("rev", "string", false), FULL],
        ),
        tool(
            "git_grep",
            "Search the working tree for a literal string (case-insensitive, gitignore-aware); \
             `regex`, `path`, and `ext` narrow it. A table of path, line, text; the first 100 \
             matches unless full.",
            &[
                ("pattern", "string", true),
                ("regex", "boolean", false),
                ("path", "string", false),
                ("ext", "string[]", false),
                FIELDS,
                FULL,
            ],
        ),
        tool(
            "index_build",
            "Build or incrementally rebuild the semantic index: this repo, or every git repo \
             directly under `root`. Unchanged files are reused by blob OID. Reports chunk counts \
             per repo.",
            &[("root", "string", false)],
        ),
        tool(
            "code_search",
            "Best general code search: fuses literal grep with semantic ranking, re-ranked by git \
             history. A table of path, line, source (lexical/semantic/both); extra field score. \
             Without an index (index_build) it degrades to grep. `root` searches every repo \
             under a directory.",
            &[
                ("query", "string", true),
                ("limit", "integer", false),
                ("root", "string", false),
                FIELDS,
            ],
        ),
        tool(
            "semantic_search",
            "Search by meaning only, using the semantic index, re-ranked by git history. A table \
             of path, lines, score; extra field preview. Errors with help when there is no index. \
             `root` searches every repo under a directory.",
            &[
                ("query", "string", true),
                ("limit", "integer", false),
                ("root", "string", false),
                FIELDS,
            ],
        ),
        tool(
            "git_branches",
            "Branches as a table of name, current. `all` adds remote-tracking branches; `remotes` \
             lists only those. `pattern` keeps matching names; `merged`, `no_merged` and \
             `contains` filter by a commit. `verbose` adds id, summary, upstream, ahead, behind.",
            &[
                ("all", "boolean", false),
                ("remotes", "boolean", false),
                ("pattern", "paths", false),
                ("merged", "string", false),
                ("no_merged", "string", false),
                ("contains", "string", false),
                ("verbose", "boolean", false),
                FIELDS,
            ],
        ),
        tool(
            "git_tags",
            "Tags as a table of name, when; extra field message. `pattern` keeps matching names \
             (a glob or an array); `contains` keeps tags containing that commit; `points_at` keeps \
             tags on that commit.",
            &[
                ("pattern", "paths", false),
                ("contains", "string", false),
                ("points_at", "string", false),
                FIELDS,
            ],
        ),
        tool(
            "git_stashes",
            "The stash list as a table of index, message.",
            &[FIELDS],
        ),
        tool(
            "git_remotes",
            "Configured remotes as a table of name, url; extra field push (the push URLs). \
             `verbose` shows push too.",
            &[("verbose", "boolean", false), FIELDS],
        ),
        tool(
            "git_worktrees",
            "Worktrees as a table of name, branch, path; extra fields head, dirty, locked, main.",
            &[FIELDS],
        ),
        tool(
            "git_describe",
            "Describe a revision relative to the nearest tag (default HEAD). `tags` uses \
             lightweight tags too, `dirty` appends -dirty, `long` forces long format, `abbrev` \
             sets the oid length (0: the tag only), `match` limits tags to a glob, `exact_match` \
             fails unless a tag points at the revision.",
            &[
                ("rev", "string", false),
                ("tags", "boolean", false),
                ("dirty", "boolean", false),
                ("long", "boolean", false),
                ("abbrev", "integer", false),
                ("match", "string", false),
                ("exact_match", "boolean", false),
            ],
        ),
        tool(
            "git_stage",
            "Stage paths (files, folders, globs), some hunks of one path (`hunk` = new-side start lines), or `lines` within one hunk.",
            &[
                ("path", "paths", true),
                ("hunk", "integer[]", false),
                ("lines", "integer[]", false),
            ],
        ),
        tool(
            "git_unstage",
            "Unstage paths (files, folders, globs), some hunks of one path, or specific lines within one hunk.",
            &[
                ("path", "paths", true),
                ("hunk", "integer[]", false),
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
            "Discard a path's unstaged changes (or some hunks, or lines of one). Destructive; git_undo restores \
             them.",
            &[
                ("path", "paths", true),
                ("hunk", "integer[]", false),
                ("lines", "integer[]", false),
            ],
        ),
        tool(
            "git_add",
            "Stage paths as git add does: new, changed and deleted files under `paths` (files, \
             folders, globs, `.`). `all` with no paths stages the whole repo; `update` stages \
             only tracked files; `force` adds ignored files. `dry_run` lists what would be \
             added; `intent_to_add` records untracked paths with no content (git add -N).",
            &[
                ("paths", "paths", false),
                ("all", "boolean", false),
                ("update", "boolean", false),
                ("force", "boolean", false),
                ("dry_run", "boolean", false),
                ("intent_to_add", "boolean", false),
            ],
        ),
        tool(
            "git_restore",
            "Restore paths in the working tree from the index, or from `source` (a revision). \
             `staged` restores the index instead (unstage); with `worktree` too, both. \
             `ours`/`theirs` write that side of conflicted paths. Destructive; git_undo \
             restores them.",
            &[
                ("paths", "paths", true),
                ("source", "string", false),
                ("staged", "boolean", false),
                ("worktree", "boolean", false),
                ("ours", "boolean", false),
                ("theirs", "boolean", false),
            ],
        ),
        tool(
            "git_resolve",
            "Resolve a conflicted path by taking `ours` (else theirs) and staging it.",
            &[("path", "string", true), ("ours", "boolean", false)],
        ),
        tool(
            "git_commit",
            "Commit the index with a message (runs hooks). `paths` commits only those paths as \
             they are in the working tree. `amend` replaces HEAD (`no_edit` keeps its message), \
             `all` stages tracked changes first, `no_verify` skips hooks, `author` is \
             `Name <email>` or a pattern naming an existing author, `date` sets the author \
             date, `reuse_message` takes a revision's message and author (git -C), \
             `reset_author` makes you the author again, `signoff` adds Signed-off-by, \
             `allow_empty` commits no change, `fixup`/`squash` make an autosquash commit for \
             a revision. Returns the commit report.",
            &[
                ("message", "string", false),
                ("paths", "paths", false),
                ("amend", "boolean", false),
                ("no_edit", "boolean", false),
                ("all", "boolean", false),
                ("no_verify", "boolean", false),
                ("author", "string", false),
                ("date", "string", false),
                ("reuse_message", "string", false),
                ("reset_author", "boolean", false),
                ("signoff", "boolean", false),
                ("allow_empty", "boolean", false),
                ("fixup", "string", false),
                ("squash", "string", false),
            ],
        ),
        tool(
            "git_extend",
            "Amend HEAD with the current index, keeping its message.",
            none,
        ),
        tool(
            "git_fetch",
            "Fetch a remote. `all` fetches every remote, `prune` drops stale remote-tracking refs, \
             `remote` picks one, `refspecs` fetch just those refs (git syntax: `main`, \
             `src:dst`). `tags` fetches every tag, `depth` limits history, `dry_run` only \
             reports what would change.",
            &[
                ("all", "boolean", false),
                ("prune", "boolean", false),
                ("remote", "string", false),
                ("refspecs", "string[]", false),
                ("tags", "boolean", false),
                ("depth", "integer", false),
                ("dry_run", "boolean", false),
            ],
        ),
        tool(
            "git_pull",
            "Fetch and integrate the current branch's upstream, or `branch` of `remote`. \
             Merges when diverged unless `pull.rebase` is set; `rebase` rebases, `no_rebase` \
             merges, `ff_only` refuses anything but a fast-forward.",
            &[
                ("remote", "string", false),
                ("branch", "string", false),
                ("rebase", "boolean", false),
                ("no_rebase", "boolean", false),
                ("ff_only", "boolean", false),
            ],
        ),
        tool(
            "git_push",
            "Push the current branch to its upstream, or to `remote`. `refspecs` push those refs \
             instead (git syntax: `branch`, `src:dst`, `:branch` deletes, `+src:dst` forces); \
             `all` pushes every branch, `tags` all tags; `delete` deletes that branch on the \
             remote; `dry_run` only reports what would be pushed.",
            &[
                ("force", "boolean", false),
                ("force_with_lease", "boolean", false),
                ("set_upstream", "boolean", false),
                ("remote", "string", false),
                ("refspecs", "string[]", false),
                ("all", "boolean", false),
                ("tags", "boolean", false),
                ("delete", "string", false),
                ("dry_run", "boolean", false),
            ],
        ),
        tool(
            "git_checkout",
            "Check out a branch (or a revision/tag as detached HEAD; `-` is the previous branch). \
             A no-op when already on it. `create` makes a new branch at `rev` (`force` resets \
             an existing one), `detach` detaches, `track` sets the upstream (`no_track` never). \
             `discard_changes` throws local changes away (git -f), `merge` carries them over \
             with a three-way merge (git -m), `orphan` starts a branch with no history at \
             `rev`. With `paths`, restores them from `rev` (index and working tree) or from \
             the index instead; `ours`/`theirs` write that side of conflicted paths.",
            &[
                ("rev", "string", false),
                ("paths", "paths", false),
                ("create", "string", false),
                ("force", "boolean", false),
                ("detach", "boolean", false),
                ("track", "boolean", false),
                ("no_track", "boolean", false),
                ("discard_changes", "boolean", false),
                ("merge", "boolean", false),
                ("orphan", "string", false),
                ("ours", "boolean", false),
                ("theirs", "boolean", false),
            ],
        ),
        tool(
            "git_switch",
            "Switch to a branch (`-` is the previous one; a remote branch's name creates a \
             local tracking branch). `create` makes a new branch at `branch` or HEAD (`force` \
             resets an existing one), `detach` checks out a revision as a detached HEAD, \
             `track` sets the upstream (`no_track` never). `discard_changes` throws local \
             changes away, `merge` carries them over with a three-way merge, `orphan` starts \
             a branch with no history and an empty working tree.",
            &[
                ("branch", "string", false),
                ("create", "string", false),
                ("force", "boolean", false),
                ("detach", "boolean", false),
                ("track", "boolean", false),
                ("no_track", "boolean", false),
                ("discard_changes", "boolean", false),
                ("merge", "boolean", false),
                ("orphan", "string", false),
            ],
        ),
        tool(
            "git_merge",
            "Merge a revision (or several, an octopus merge) into the current branch. `no_ff` \
             forces a merge commit, `ff_only` refuses a non-fast-forward, `squash` stages the \
             result without a merge commit, `no_commit` stops before committing, `message` sets \
             the commit message, `strategy_option` (ours|theirs) settles conflicting hunks, \
             `strategy` (ort|recursive|resolve|octopus|ours; ours keeps HEAD's tree), \
             `allow_unrelated_histories` merges histories with no common commit, `log` adds up \
             to N merged subjects to the message, `signoff` adds Signed-off-by, `no_verify` \
             skips the pre-merge-commit and commit-msg hooks, `no_stat` drops the diffstat. \
             `continue` commits a resolved merge, `abort` cancels a conflicted one, `quit` \
             forgets it and keeps the index and working tree.",
            &[
                ("rev", "paths", false),
                ("no_ff", "boolean", false),
                ("ff_only", "boolean", false),
                ("squash", "boolean", false),
                ("no_commit", "boolean", false),
                ("message", "string", false),
                ("strategy_option", "string", false),
                ("strategy", "string", false),
                ("allow_unrelated_histories", "boolean", false),
                ("log", "integer", false),
                ("signoff", "boolean", false),
                ("no_verify", "boolean", false),
                ("no_stat", "boolean", false),
                ("continue", "boolean", false),
                ("abort", "boolean", false),
                ("quit", "boolean", false),
            ],
        ),
        tool(
            "git_rebase",
            "Rebase the current branch onto a revision (`onto`; the upstream when omitted). \
             `newbase` replays the commits after `onto` onto it (git's --onto). `root` rebases \
             down to the root commit, `autosquash` folds fixup!/squash! commits, `exec` runs \
             shell commands after each commit, `update_refs` moves branches inside the range, \
             `strategy_option` (ours|theirs) settles conflicting hunks. `branch` is checked out \
             first. `keep_empty`, `force_rebase` (replay every commit), `rebase_merges` \
             (recreate merges), `keep_base`, `committer_date_is_author_date`, \
             `reset_author_date`, `reapply_cherry_picks`, `empty` (drop|keep|stop), `signoff`, \
             `autostash` and `no_verify` work as in git.",
            &[
                ("onto", "string", false),
                ("newbase", "string", false),
                ("branch", "string", false),
                ("root", "boolean", false),
                ("autosquash", "boolean", false),
                ("exec", "string[]", false),
                ("update_refs", "boolean", false),
                ("strategy_option", "string", false),
                ("keep_empty", "boolean", false),
                ("force_rebase", "boolean", false),
                ("rebase_merges", "boolean", false),
                ("keep_base", "boolean", false),
                ("committer_date_is_author_date", "boolean", false),
                ("reset_author_date", "boolean", false),
                ("reapply_cherry_picks", "boolean", false),
                ("empty", "string", false),
                ("signoff", "boolean", false),
                ("autostash", "boolean", false),
                ("no_verify", "boolean", false),
            ],
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
            "git_rebase_quit",
            "Stop an in-progress rebase, leaving HEAD, the index and the working tree as they are.",
            none,
        ),
        tool(
            "git_rebase_current_patch",
            "Show the commit an in-progress rebase stopped at.",
            none,
        ),
        tool(
            "git_cherry_pick",
            "Cherry-pick commits onto HEAD in order; `rev` is a commit, a range `A..B`, or a list. \
             `record_origin` appends \"(cherry picked from commit ...)\", `mainline` picks the \
             parent (from 1) of a merge commit, `strategy_option` (ours|theirs) settles \
             conflicting hunks, `signoff` adds Signed-off-by, `ff` fast-forwards over a commit \
             whose parent is HEAD, `allow_empty` keeps commits that were empty, `empty` \
             (stop|drop|keep) handles commits already in HEAD. After a conflict: `continue`, \
             `skip`, `abort` or `quit` (forget the sequence, keep the files).",
            &[
                ("rev", "paths", false),
                ("no_commit", "boolean", false),
                ("record_origin", "boolean", false),
                ("mainline", "integer", false),
                ("strategy_option", "string", false),
                ("signoff", "boolean", false),
                ("ff", "boolean", false),
                ("allow_empty", "boolean", false),
                ("empty", "string", false),
                ("continue", "boolean", false),
                ("skip", "boolean", false),
                ("abort", "boolean", false),
                ("quit", "boolean", false),
            ],
        ),
        tool(
            "git_revert",
            "Revert commits on HEAD; `rev` is a commit, a range `A..B` (newest first), or a list. \
             `mainline` picks the parent (from 1) of a merge commit, `strategy_option` \
             (ours|theirs) settles conflicting hunks, `signoff` adds Signed-off-by, `reference` \
             names the commit as `abbrev (subject, date)`. After a conflict: `continue`, `skip`, \
             `abort` or `quit`.",
            &[
                ("rev", "paths", false),
                ("no_commit", "boolean", false),
                ("mainline", "integer", false),
                ("strategy_option", "string", false),
                ("signoff", "boolean", false),
                ("reference", "boolean", false),
                ("continue", "boolean", false),
                ("skip", "boolean", false),
                ("abort", "boolean", false),
                ("quit", "boolean", false),
            ],
        ),
        tool(
            "git_reset",
            "Reset HEAD to a revision. `mode` is soft, mixed (default), hard, keep, or merge \
             (keeps unstaged changes; aborts a conflicted merge). With \
             `paths`, resets only their index entries to `rev` (default HEAD), leaving HEAD \
             alone.",
            &[
                ("rev", "string", false),
                ("mode", "string", false),
                ("paths", "paths", false),
            ],
        ),
        tool(
            "git_undo",
            "Undo the last operation from the op-log, restoring HEAD and the working tree \
             (recovers uncommitted work).",
            none,
        ),
        tool("git_redo", "Redo the operation most recently undone.", none),
        tool(
            "git_oplog",
            "The operation log (undo stack), newest first: a table of id, label, head, when.",
            &[FIELDS],
        ),
        tool(
            "git_smartlog",
            "Your local/draft commits and the trunk they branch from: a table of id, mark \
             (head/trunk/draft), summary, refs; extra fields when, author, change.",
            &[FIELDS],
        ),
        tool(
            "git_absorb",
            "Fold each modified file's changes into the newest local commit that touched it \
             (fixup + autosquash).",
            none,
        ),
        tool(
            "git_reword",
            "Change a commit's message (default HEAD) and restack descendants.",
            &[("message", "string", true), ("rev", "string", false)],
        ),
        tool(
            "git_uncommit",
            "Undo the last `n` commits (default 1), keeping the changes staged.",
            &[("n", "integer", false)],
        ),
        tool(
            "git_squash",
            "Fold a commit into its parent (default HEAD); with `from`, fold every commit after \
             `from` up to HEAD into one. Restacks descendants.",
            &[("rev", "string", false), ("from", "string", false)],
        ),
        tool(
            "git_split",
            "Split a commit (default HEAD) into two by path: the given `paths`' changes first, the \
             rest second. Restacks descendants.",
            &[("paths", "string[]", true), ("rev", "string", false)],
        ),
        tool(
            "git_move",
            "Reorder a commit before or after another in the current branch's history. Pass \
             exactly one of `before`/`after`.",
            &[
                ("rev", "string", true),
                ("before", "string", false),
                ("after", "string", false),
            ],
        ),
        tool(
            "git_prune",
            "Delete local branches fully merged into a base (default HEAD). Returns the deleted \
             names.",
            &[("base", "string", false)],
        ),
        tool(
            "git_sync",
            "Fetch, fast-forward branches to their upstreams, and restack the stack.",
            none,
        ),
        tool(
            "git_submit",
            "Push every branch in the current stack and open a pull request per branch (via \
             gh/glab).",
            none,
        ),
        tool(
            "git_flow_init",
            "Set the active branching workflow: gitflow, github, gitlab, trunk, or release-flow.",
            &[("preset", "string", true)],
        ),
        tool(
            "git_flow_start",
            "Start a feature branch per the active workflow.",
            &[("name", "string", true)],
        ),
        tool(
            "git_flow_finish",
            "Finish the current feature (local merge, or push + PR per the workflow).",
            none,
        ),
        tool(
            "git_flow_release",
            "Start a release (or finish it with finish=true) per the active workflow.",
            &[("version", "string", true), ("finish", "boolean", false)],
        ),
        tool(
            "git_flow_status",
            "The active workflow and its policy fields; errors with help when none is set.",
            none,
        ),
        tool(
            "git_workspace_new",
            "Create a copy-on-write clone of the repo on a new branch (parallel isolated work). A \
             no-op when it exists.",
            &[("name", "string", true)],
        ),
        tool(
            "git_workspace_list",
            "This repo's copy-on-write workspaces as a table of name, branch, path.",
            &[FIELDS],
        ),
        tool(
            "git_workspace_remove",
            "Remove a copy-on-write workspace. A no-op when it does not exist.",
            &[("name", "string", true)],
        ),
        tool(
            "git_stack_new",
            "Create a new branch stacked on the current one.",
            &[("name", "string", true)],
        ),
        tool(
            "git_stack_list",
            "The current branch's stack from tip to base: a table of branch, parent, current.",
            &[FIELDS],
        ),
        tool(
            "git_stack_restack",
            "Rebase every stacked branch onto its parent's new tip.",
            none,
        ),
        tool(
            "git_lanes_list",
            "Lanes as a table of name, branch, files, commits; says so when lanes are off.",
            &[FIELDS],
        ),
        tool(
            "git_lanes_init",
            "Enter lanes mode: record the fork point and a default lane.",
            none,
        ),
        tool(
            "git_lanes_off",
            "Leave lanes mode (lane branches are kept).",
            none,
        ),
        tool(
            "git_lanes_new",
            "Create a new lane that commits to a same-named branch. A no-op when it exists.",
            &[("name", "string", true)],
        ),
        tool(
            "git_lanes_stack",
            "Create a new lane stacked on another (its commits build on that lane's branch).",
            &[("name", "string", true), ("on", "string", true)],
        ),
        tool(
            "git_lanes_assign",
            "Assign a worktree path to a lane, or a single hunk with `hunk` set to its new-file \
             start line.",
            &[
                ("lane", "string", true),
                ("path", "string", true),
                ("hunk", "integer", false),
            ],
        ),
        tool(
            "git_lanes_unassign",
            "Return a path to the default lane.",
            &[("path", "string", true)],
        ),
        tool(
            "git_lanes_commit",
            "Commit a lane's owned changes to its branch.",
            &[("lane", "string", true), ("message", "string", true)],
        ),
        tool(
            "git_lanes_rename",
            "Rename a lane and its branch.",
            &[("old", "string", true), ("new", "string", true)],
        ),
        tool(
            "git_lanes_delete",
            "Delete a lane (its changes return to default; its branch is kept). A no-op when it \
             does not exist.",
            &[("name", "string", true)],
        ),
        tool(
            "git_lanes_push",
            "Push a lane's branch to the remote and set its upstream.",
            &[("lane", "string", true)],
        ),
        tool(
            "git_lanes_pr",
            "Push a lane's branch and open a pull/merge request via gh/glab.",
            &[("lane", "string", true)],
        ),
        tool(
            "git_lanes_restack",
            "Move each stacked lane onto its parent lane's new tip (in the odb; the worktree is \
             untouched).",
            none,
        ),
        tool(
            "git_bisect",
            "Find the commit that introduced a change by binary search. `command`: start (revs: \
             bad then good ones; paths, term_new, term_old, no_checkout, first_parent), \
             good|bad|new|old|skip (revs, HEAD by default), reset (revs: where to end up), log, \
             replay (file), run (cmd: the test command and its arguments), visualize (the \
             commits left), terms. Returns the step: remaining, steps, current, or first_bad.",
            &[
                ("command", "string", true),
                ("revs", "string[]", false),
                ("paths", "string[]", false),
                ("term_new", "string", false),
                ("term_old", "string", false),
                ("no_checkout", "boolean", false),
                ("first_parent", "boolean", false),
                ("file", "string", false),
                ("cmd", "string[]", false),
                FULL,
            ],
        ),
        tool(
            "git_stash_push",
            "Stash the working tree and index (with an optional message), or only `paths`. \
             `include_untracked` also stashes untracked files; `keep_index` leaves staged \
             changes in the index.",
            &[
                ("message", "string", false),
                ("include_untracked", "boolean", false),
                ("keep_index", "boolean", false),
                ("paths", "paths", false),
            ],
        ),
        tool(
            "git_stash_pop",
            "Apply and drop the stash at `index` (default 0, the most recent). \
             `restore_index` restores its staged changes too.",
            &[
                ("index", "integer", false),
                ("restore_index", "boolean", false),
            ],
        ),
        tool(
            "git_stash_apply",
            "Apply the stash at `index` without dropping it (default 0). `restore_index` \
             restores its staged changes too.",
            &[
                ("index", "integer", false),
                ("restore_index", "boolean", false),
            ],
        ),
        tool(
            "git_stash_drop",
            "Drop the stash at `index` without applying it (default 0).",
            &[("index", "integer", false)],
        ),
        tool(
            "git_stash_show",
            "The changes the stash at `index` records (default 0), as a diffstat table; \
             `patch` returns the patch, `name_only` the file names.",
            &[
                ("index", "integer", false),
                ("patch", "boolean", false),
                ("name_only", "boolean", false),
            ],
        ),
        tool(
            "git_stash_branch",
            "Create and check out branch `name` at the stash's base commit, apply the stash \
             there and drop it (default stash 0).",
            &[("name", "string", true), ("index", "integer", false)],
        ),
        tool("git_stash_clear", "Drop every stash. Destructive.", none),
        tool(
            "git_branch_create",
            "Create a branch at `start` (default HEAD) and check it out. A no-op when it exists.",
            &[("name", "string", true), ("start", "string", false)],
        ),
        tool(
            "git_branch_delete",
            "Delete local branches (`name` is one or an array); `force` deletes them even if not \
             fully merged. A no-op when none exist.",
            &[("name", "paths", true), ("force", "boolean", false)],
        ),
        tool(
            "git_branch_rename",
            "Rename a local branch.",
            &[("old", "string", true), ("new", "string", true)],
        ),
        tool(
            "git_branch_set_upstream",
            "Make branch `name` (default current) track `upstream`, or stop tracking when \
             `upstream` is omitted.",
            &[("name", "string", false), ("upstream", "string", false)],
        ),
        tool(
            "git_tag_create",
            "Create a tag at `rev` (default HEAD; annotated when `message` is given; `force` \
             replaces one). A no-op when it exists.",
            &[
                ("name", "string", true),
                ("rev", "string", false),
                ("message", "string", false),
                ("force", "boolean", false),
            ],
        ),
        tool(
            "git_tag_delete",
            "Delete tags (`name` is one or an array). A no-op when none exist.",
            &[("name", "paths", true)],
        ),
        tool(
            "git_remote_add",
            "Add a remote. A no-op when it already points at that url.",
            &[("name", "string", true), ("url", "string", true)],
        ),
        tool(
            "git_remote_remove",
            "Remove a remote. A no-op when it does not exist.",
            &[("name", "string", true)],
        ),
        tool(
            "git_remote_set_url",
            "Change a remote's URL; `push` sets the push URL instead.",
            &[
                ("name", "string", true),
                ("url", "string", true),
                ("push", "boolean", false),
            ],
        ),
        tool(
            "git_remote_get_url",
            "A remote's URL; `push` gives the push URL, `all` every URL.",
            &[
                ("name", "string", true),
                ("push", "boolean", false),
                ("all", "boolean", false),
            ],
        ),
        tool(
            "git_remote_prune",
            "Delete remote-tracking branches that no longer exist on the remote `name` (one or \
             an array).",
            &[("name", "paths", true)],
        ),
        tool(
            "git_remote_rename",
            "Rename a remote.",
            &[("old", "string", true), ("new", "string", true)],
        ),
        tool(
            "git_worktree_add",
            "Add a linked worktree at `path`: on `branch` (an existing branch, or a commit to \
             detach at), on `new_branch` created at `branch` or HEAD, detached with `detach`, or \
             by default on a branch named after the path's last folder. A no-op when a worktree \
             is already there.",
            &[
                ("path", "string", true),
                ("branch", "string", false),
                ("new_branch", "string", false),
                ("detach", "boolean", false),
            ],
        ),
        tool(
            "git_worktree_lock",
            "Lock a worktree (`name` is its name or path) so prune leaves it alone.",
            &[("name", "string", true), ("reason", "string", false)],
        ),
        tool(
            "git_worktree_unlock",
            "Unlock a worktree (`name` is its name or path).",
            &[("name", "string", true)],
        ),
        tool(
            "git_worktree_move",
            "Move a worktree (`name` is its name or path) to `new_path`.",
            &[("name", "string", true), ("new_path", "string", true)],
        ),
        tool(
            "git_worktree_remove",
            "Remove a linked worktree (`name` is its name or path) and its folder; `force` \
             removes it even with changes or when locked. A no-op when it does not exist.",
            &[("name", "string", true), ("force", "boolean", false)],
        ),
        tool(
            "git_worktree_prune",
            "Prune worktree entries whose working tree is gone.",
            none,
        ),
        tool(
            "git_config",
            "Read config: the value of `key` (`all` returns every value of a multi-valued key), or \
             every key=value when no key is given. `global` or `local` reads only that file.",
            &[
                ("key", "string", false),
                ("all", "boolean", false),
                ("global", "boolean", false),
                ("local", "boolean", false),
            ],
        ),
        tool(
            "git_config_set",
            "Set config `key` to `value` in the repository's config (`global`: the user's). `add` \
             appends to a multi-valued key; `unset` removes the key instead (no value needed).",
            &[
                ("key", "string", true),
                ("value", "string", false),
                ("global", "boolean", false),
                ("add", "boolean", false),
                ("unset", "boolean", false),
            ],
        ),
        tool(
            "git_apply",
            "Apply patch files (unified diffs) to the working tree; `cached` applies to the index \
             only, `index` to both. `reverse` undoes a patch, `check` only tests that it applies, \
             `stat` returns its diffstat without applying. Undo with git_undo.",
            &[
                ("patch", "paths", true),
                ("cached", "boolean", false),
                ("index", "boolean", false),
                ("reverse", "boolean", false),
                ("check", "boolean", false),
                ("stat", "boolean", false),
            ],
        ),
        tool(
            "git_notes",
            "Notes attached to commits: the note of `rev`, or every note as `<note id> <object \
             id>` lines when no rev is given. `ref` picks a notes ref (default refs/notes/commits).",
            &[("rev", "string", false), ("ref", "string", false)],
        ),
        tool(
            "git_note_add",
            "Attach `message` as the note of `rev` (default HEAD). An existing note needs `force` \
             to be replaced; `append` adds the message as a new paragraph instead.",
            &[
                ("message", "string", true),
                ("rev", "string", false),
                ("force", "boolean", false),
                ("append", "boolean", false),
                ("ref", "string", false),
            ],
        ),
        tool(
            "git_note_remove",
            "Remove the note of `rev` (default HEAD).",
            &[("rev", "string", false), ("ref", "string", false)],
        ),
        tool(
            "git_update_ref",
            "Point ref `name` (e.g. refs/heads/topic) at revision `new`, or delete it with \
             `delete`. Destructive: it can move or delete a branch; undo with git_undo. `old` makes \
             it conditional on the ref's current value (all zeros: the ref must not exist). A \
             symbolic ref like HEAD is followed unless `no_deref`.",
            &[
                ("name", "string", true),
                ("new", "string", false),
                ("old", "string", false),
                ("delete", "boolean", false),
                ("no_deref", "boolean", false),
                ("message", "string", false),
            ],
        ),
        tool(
            "git_hash_object",
            "Object ids of files, one per line; `write` stores them in the repository. `type` is \
             blob (default), tree, commit or tag.",
            &[
                ("path", "paths", true),
                ("write", "boolean", false),
                ("type", "string", false),
            ],
        ),
        tool(
            "git_format_patch",
            "Write commits as mbox patch files, oldest first, and return their paths. `range` is \
             `<a>..<b>`, or a base revision for the commits after it up to HEAD; `count` takes the \
             newest n (ending at `range` when given). `output_dir` is the folder (default the \
             current one); `stdout` returns the patches instead of writing files.",
            &[
                ("range", "string", false),
                ("count", "integer", false),
                ("output_dir", "string", false),
                ("stdout", "boolean", false),
            ],
        ),
        tool(
            "git_am",
            "Apply mbox patch files (from git_format_patch) as commits. When a patch does not \
             apply, resolve it and call with `continue`, or `skip` it, or `abort` to restore the \
             branch. `three_way` falls back to a three-way merge; `signoff` adds Signed-off-by. \
             Undo with git_undo.",
            &[
                ("mbox", "paths", false),
                ("continue", "boolean", false),
                ("skip", "boolean", false),
                ("abort", "boolean", false),
                ("three_way", "boolean", false),
                ("signoff", "boolean", false),
            ],
        ),
        tool(
            "git_archive",
            "Write the files of `rev` (default HEAD) to the archive file `output`: tar, tgz or zip \
             (`format`, else from output's extension). `paths` limits it; `prefix` puts every \
             entry under a folder (e.g. project/).",
            &[
                ("output", "string", true),
                ("rev", "string", false),
                ("paths", "paths", false),
                ("format", "string", false),
                ("prefix", "string", false),
            ],
        ),
        tool(
            "git_gc",
            "Pack the object database and prune unreachable loose objects (git gc). Destructive: \
             pruned objects are gone for good (rgit's undo history is kept). `prune` is the age \
             cutoff (e.g. now; default 2 weeks ago); `aggressive` repacks thoroughly (slow).",
            &[("prune", "string", false), ("aggressive", "boolean", false)],
        ),
        tool(
            "git_fsck",
            "Check the object database for corruption; returns problems and dangling objects. \
             `full` checks packed objects too, `unreachable` lists unreachable objects, \
             `no_dangling` hides dangling ones.",
            &[
                ("full", "boolean", false),
                ("strict", "boolean", false),
                ("unreachable", "boolean", false),
                ("no_dangling", "boolean", false),
            ],
        ),
        tool(
            "git_clean",
            "Remove untracked files and directories. Destructive. `dry_run` lists what would be \
             removed without deleting. `paths` limits it; `ignored` also removes ignored files \
             (-x); `only_ignored` removes only those (-X); `exclude` keeps matching patterns.",
            &[
                ("dry_run", "boolean", false),
                ("paths", "paths", false),
                ("ignored", "boolean", false),
                ("only_ignored", "boolean", false),
                ("exclude", "string[]", false),
            ],
        ),
        tool(
            "git_rm",
            "Remove tracked paths (files, folders, globs) from the index and the working tree. \
             `cached` removes them from the index only; `recursive` is needed for a folder. \
             Files with staged or unstaged changes are refused unless `force`. `dry_run` lists \
             what would be removed; `ignore_unmatch` allows paths that match nothing.",
            &[
                ("path", "paths", true),
                ("cached", "boolean", false),
                ("recursive", "boolean", false),
                ("force", "boolean", false),
                ("dry_run", "boolean", false),
                ("ignore_unmatch", "boolean", false),
            ],
        ),
        tool(
            "git_mv",
            "Rename/move tracked files or folders. `from` may list several paths when `to` is a \
             folder. `force` overwrites an existing destination, `skip_errors` skips moves that \
             would fail, `dry_run` only reports the moves.",
            &[
                ("from", "paths", true),
                ("to", "string", true),
                ("force", "boolean", false),
                ("skip_errors", "boolean", false),
                ("dry_run", "boolean", false),
            ],
        ),
        tool(
            "git_rev_parse",
            "Resolve revisions (`HEAD`, `main~2`, `v1^{commit}`, `HEAD:path`, `A..B`) to object \
             ids, like git rev-parse. `short` abbreviates to N digits; `abbrev_ref` / \
             `symbolic_full_name` give ref names; `verify` needs exactly one revision; \
             `show_toplevel` / `git_dir` print paths.",
            &[
                ("revs", "string[]", false),
                ("short", "integer", false),
                ("abbrev_ref", "boolean", false),
                ("symbolic_full_name", "boolean", false),
                ("verify", "boolean", false),
                ("show_toplevel", "boolean", false),
                ("git_dir", "boolean", false),
            ],
        ),
        tool(
            "git_ls_files",
            "Index and working-tree files like git ls-files: a table of path (tracked files by \
             default). `stage` adds mode, object, stage; `others` lists untracked files \
             (`exclude_standard` honors .gitignore; with `ignored`, only ignored ones); \
             `modified`, `deleted`, `unmerged` filter.",
            &[
                ("paths", "string[]", false),
                ("stage", "boolean", false),
                ("others", "boolean", false),
                ("exclude_standard", "boolean", false),
                ("ignored", "boolean", false),
                ("modified", "boolean", false),
                ("deleted", "boolean", false),
                ("unmerged", "boolean", false),
                FIELDS,
            ],
        ),
        tool(
            "git_ls_tree",
            "A tree's entries like git ls-tree: a table of mode, type, object, path. `recursive` \
             descends into subtrees; `long` adds size; `paths` limits (`dir/` lists inside dir).",
            &[
                ("rev", "string", true),
                ("paths", "string[]", false),
                ("recursive", "boolean", false),
                ("long", "boolean", false),
                FIELDS,
            ],
        ),
        tool(
            "git_cat_file",
            "An object's content like git cat-file -p (`object` is `HEAD`, `HEAD:path`, an id), \
             truncated unless full. `kind` gives only its type, `size` only its size, `exists` \
             only whether it exists.",
            &[
                ("object", "string", true),
                ("kind", "boolean", false),
                ("size", "boolean", false),
                ("exists", "boolean", false),
                FULL,
            ],
        ),
        tool(
            "git_show_ref",
            "Refs with their object ids like git show-ref: a table of object, ref. `heads` / \
             `tags` filter; `patterns` match name endings (`main`); `dereference` adds what \
             annotated tags point at.",
            &[
                ("patterns", "string[]", false),
                ("heads", "boolean", false),
                ("tags", "boolean", false),
                ("dereference", "boolean", false),
                FIELDS,
            ],
        ),
        tool(
            "git_for_each_ref",
            "Refs like git for-each-ref: a table of refname, objectname, objecttype, or lines in \
             `format` (`%(refname:short) %(objectname:short) %(committerdate:iso) %(subject)`). \
             `sort` keys (`-committerdate`), `count` limits, `patterns` are prefixes or globs.",
            &[
                ("patterns", "string[]", false),
                ("format", "string", false),
                ("sort", "string[]", false),
                ("count", "integer", false),
                FIELDS,
            ],
        ),
        tool(
            "git_rev_list",
            "Commit ids reachable from `revs` (`HEAD`, `^A`, `A..B`, `A...B`) like git rev-list, \
             newest first. `count` returns only the number; `all` walks every ref.",
            &[
                ("revs", "string[]", false),
                ("all", "boolean", false),
                ("count", "boolean", false),
                ("max_count", "integer", false),
                ("reverse", "boolean", false),
                ("first_parent", "boolean", false),
                ("merges", "boolean", false),
                ("no_merges", "boolean", false),
            ],
        ),
        tool(
            "git_merge_base",
            "The best common ancestor of commits `a` and `b` (`all` for every one). With \
             `is_ancestor`, returns ancestor: true/false for whether a is an ancestor of b.",
            &[
                ("a", "string", true),
                ("b", "string", true),
                ("all", "boolean", false),
                ("is_ancestor", "boolean", false),
            ],
        ),
        tool(
            "git_reflog",
            "A ref's reflog (default HEAD) like git reflog: a table of id, selector, message.",
            &[
                ("ref", "string", false),
                ("max_count", "integer", false),
                FIELDS,
            ],
        ),
        tool(
            "git_shortlog",
            "Commits grouped by author like git shortlog: a table of author, count; extra field \
             subjects. `revs` default to HEAD; `email` adds emails; `numbered` sorts by count.",
            &[
                ("revs", "string[]", false),
                ("all", "boolean", false),
                ("email", "boolean", false),
                ("numbered", "boolean", false),
                FIELDS,
            ],
        ),
        tool(
            "git_grep_tracked",
            "Search tracked files like git grep (regexes, case-sensitive): a table of path, \
             line, text. `rev` searches a revision, `cached` the index; `fixed` takes literal \
             strings; `ignore_case`, `word`, `invert` as in git; `paths` limit.",
            &[
                ("patterns", "string[]", true),
                ("rev", "string", false),
                ("cached", "boolean", false),
                ("fixed", "boolean", false),
                ("ignore_case", "boolean", false),
                ("word", "boolean", false),
                ("invert", "boolean", false),
                ("paths", "string[]", false),
                FIELDS,
            ],
        ),
        tool(
            "git_check_ignore",
            "Which of `paths` are ignored, and by which rule: a table of path, source, line, \
             pattern.",
            &[("paths", "string[]", true), FIELDS],
        ),
        tool(
            "git_var",
            "A git variable: GIT_AUTHOR_IDENT, GIT_COMMITTER_IDENT, GIT_EDITOR, \
             GIT_SEQUENCE_EDITOR, GIT_PAGER or GIT_DEFAULT_BRANCH.",
            &[("name", "string", true)],
        ),
        tool(
            "git_symbolic_ref",
            "Where a symbolic ref (default HEAD) points, e.g. refs/heads/main; `short` gives main.",
            &[("name", "string", false), ("short", "boolean", false)],
        ),
        tool(
            "git_count_objects",
            "Loose object count and size; `verbose` adds packs and packed objects.",
            &[("verbose", "boolean", false)],
        ),
        tool(
            "git_run",
            "Run any git subcommand and return its output as lines (the escape hatch); the first \
             200 lines unless full.",
            &[("args", "string[]", true), FULL],
        ),
    ]
}

/// Stands in for the CLI invocation in `finalize`, so its `--full` hints can be
/// rewritten into the MCP `full` argument.
const RERUN: &str = "\u{1}";
const GREP_SHOWN: usize = 100;
const FILES_SHOWN: usize = 200;

/// Run one tool call to the object `rgit --toon` would print for it.
fn dispatch(backend: &Arc<dyn GitBackend>, name: &str, args: &Value) -> anyhow::Result<Obj> {
    let a = Args { tool: name, args };
    let fields = a.strings("fields")?;
    let full = a.flag("full");
    let since = backend.index_second();
    let output = match command(&a)? {
        Some(command) => crate::axi::run(backend, command, false),
        None => local(backend, &a, full),
    };
    let _ = backend.smudge_racy(since);
    let output = output?;
    let mut value = output.finalize(&fields, full, RERUN)?;
    if let Some((_, Node::List(help))) = value.iter_mut().find(|(k, _)| k == "help") {
        let cli = format!("Run `{RERUN} --full`");
        let mcp = format!("Call {name} with full=true");
        for line in help {
            if let Node::Str(text) = line {
                *text = as_tool_call(&text.replace(&cli, &mcp));
            }
        }
    }
    Ok(value)
}

/// CLI command words, the MCP tool they map to, and that tool's positional
/// parameters in CLI order. Longer word sequences come first so they win.
const CLI_TOOLS: &[(&str, &str, &[&str])] = &[
    ("rebase --continue", "git_rebase_continue", &[]),
    ("rebase --abort", "git_rebase_abort", &[]),
    ("rebase --skip", "git_rebase_skip", &[]),
    ("rebase --quit", "git_rebase_quit", &[]),
    (
        "rebase --show-current-patch",
        "git_rebase_current_patch",
        &[],
    ),
    ("bisect", "git_bisect", &["command", "revs"]),
    ("stash pop", "git_stash_pop", &["index"]),
    ("stash list", "git_stashes", &[]),
    ("stash show", "git_stash_show", &["index"]),
    ("stash push", "git_stash_push", &["paths"]),
    ("branch create", "git_branch_create", &["name", "start"]),
    ("branch delete", "git_branch_delete", &["name"]),
    ("branch rename", "git_branch_rename", &["old", "new"]),
    ("remote add", "git_remote_add", &["name", "url"]),
    ("remote set-url", "git_remote_set_url", &["name", "url"]),
    ("workspace new", "git_workspace_new", &["name"]),
    ("stack new", "git_stack_new", &["name"]),
    ("lanes init", "git_lanes_init", &[]),
    ("lanes assign", "git_lanes_assign", &["lane", "path"]),
    ("lanes commit", "git_lanes_commit", &["lane"]),
    ("lanes push", "git_lanes_push", &["lane"]),
    ("flow init", "git_flow_init", &["preset"]),
    ("flow start", "git_flow_start", &["name"]),
    ("flow finish", "git_flow_finish", &[]),
    ("notes add", "git_note_add", &["rev"]),
    ("notes remove", "git_note_remove", &["rev"]),
    ("notes show", "git_notes", &["rev"]),
    ("ls-files", "git_ls_files", &["paths"]),
    ("status", "git_status", &[]),
    ("log", "git_log", &["rev"]),
    ("diff", "git_diff", &["from", "to"]),
    ("show", "git_show", &["rev"]),
    ("blame", "git_blame", &["path"]),
    ("stage", "git_stage", &["path"]),
    ("add", "git_add", &["paths"]),
    ("restore", "git_restore", &["paths"]),
    ("switch", "git_switch", &["branch"]),
    ("unstage", "git_unstage", &["path"]),
    ("discard", "git_discard", &["path"]),
    ("resolve", "git_resolve", &["path"]),
    ("commit", "git_commit", &[]),
    ("push", "git_push", &[]),
    ("pull", "git_pull", &["remote", "branch"]),
    ("fetch", "git_fetch", &[]),
    ("sync", "git_sync", &[]),
    ("submit", "git_submit", &[]),
    ("undo", "git_undo", &[]),
    ("redo", "git_redo", &[]),
    ("smartlog", "git_smartlog", &[]),
    ("checkout", "git_checkout", &["rev"]),
    ("merge", "git_merge", &["rev"]),
    ("rebase", "git_rebase", &["onto"]),
    ("cherry-pick", "git_cherry_pick", &["rev"]),
    ("revert", "git_revert", &["rev"]),
    ("stash", "git_stash_push", &[]),
    ("branch", "git_branches", &[]),
    ("tag", "git_tag_create", &["name", "rev"]),
    ("remote", "git_remotes", &[]),
    ("worktree", "git_worktrees", &[]),
    ("workspace", "git_workspace_list", &[]),
    ("stack", "git_stack_list", &[]),
    ("notes", "git_notes", &[]),
    ("config", "git_config", &["key"]),
    ("apply", "git_apply", &["patch"]),
    ("format-patch", "git_format_patch", &["range"]),
    ("am", "git_am", &["mbox"]),
    ("update-ref", "git_update_ref", &["name", "new", "old"]),
    ("gc", "git_gc", &[]),
    ("fsck", "git_fsck", &[]),
];

/// CLI flags that take a value, and the tool parameter they set.
const VALUE_FLAGS: &[(&str, &str)] = &[
    ("-m", "message"),
    ("--message", "message"),
    ("-n", "limit"),
    ("--limit", "limit"),
    ("-L", "lines"),
    ("--remote", "remote"),
    ("--hunk", "hunk"),
    ("--on", "on"),
];

/// Split a hint's command on spaces, keeping `"..."` groups whole.
fn words(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in cmd.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ' ' if !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Rewrite a CLI hint (`Run `rgit stage <path>` to ...`) as the matching tool
/// call (`Call git_stage with path=<path> to ...`). Hints without a tool
/// equivalent (init, forge, index) stay as they are.
fn as_tool_call(hint: &str) -> String {
    let Some(rest) = hint.strip_prefix("Run `rgit ") else {
        return hint.to_owned();
    };
    let Some((cmd, tail)) = rest.split_once('`') else {
        return hint.to_owned();
    };
    let args = words(cmd);
    let Some((matched, tool, positional)) = CLI_TOOLS.iter().find_map(|(words_, tool, pos)| {
        let want: Vec<&str> = words_.split(' ').collect();
        (args.len() >= want.len() && args.iter().zip(&want).all(|(a, w)| a == w)).then_some((
            want.len(),
            *tool,
            *pos,
        ))
    }) else {
        return hint.to_owned();
    };
    let mut params = Vec::new();
    let mut positional = positional.iter();
    let mut it = args[matched..].iter();
    while let Some(arg) = it.next() {
        if let Some((_, param)) = VALUE_FLAGS.iter().find(|(f, _)| f == arg) {
            if let Some(value) = it.next() {
                params.push(format!("{param}={value}"));
            }
        } else if arg == "--ours|--theirs" {
            params.push("ours=true|false".to_owned());
        } else if let Some(flag) = arg.strip_prefix("--") {
            params.push(format!("{}=true", flag.replace('-', "_")));
        } else if let Some(param) = positional.next() {
            params.push(format!("{param}={arg}"));
        } else {
            return hint.to_owned();
        }
    }
    if params.is_empty() {
        format!("Call {tool}{tail}")
    } else {
        format!("Call {tool} with {}{tail}", params.join(", "))
    }
}

/// Typed access to a tool call's JSON arguments; bad input is a usage error.
struct Args<'a> {
    tool: &'a str,
    args: &'a Value,
}

impl Args<'_> {
    fn get(&self, key: &str) -> Option<&Value> {
        self.args.get(key).filter(|v| !v.is_null())
    }

    fn str(&self, key: &str) -> Option<String> {
        self.get(key).and_then(Value::as_str).map(str::to_owned)
    }

    fn or(&self, key: &str, default: &str) -> String {
        self.str(key).unwrap_or_else(|| default.to_owned())
    }

    fn req(&self, key: &str) -> anyhow::Result<String> {
        self.str(key).ok_or_else(|| self.missing(key))
    }

    /// One string or an array of strings; required.
    fn strs(&self, key: &str) -> anyhow::Result<Vec<String>> {
        match self.get(key) {
            Some(Value::String(s)) => Ok(vec![s.clone()]),
            Some(Value::Array(items)) if !items.is_empty() => items
                .iter()
                .map(|v| v.as_str().map(str::to_owned))
                .collect::<Option<_>>()
                .ok_or_else(|| self.invalid(key, "a string or an array of strings")),
            Some(_) => Err(self.invalid(key, "a string or an array of strings")),
            None => Err(self.missing(key)),
        }
    }

    fn flag(&self, key: &str) -> bool {
        self.get(key).and_then(Value::as_bool).unwrap_or(false)
    }

    fn num(&self, key: &str) -> anyhow::Result<Option<u64>> {
        let Some(v) = self.get(key) else {
            return Ok(None);
        };
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
            .map(Some)
            .ok_or_else(|| self.invalid(key, "a non-negative integer"))
    }

    fn nums(&self, key: &str) -> anyhow::Result<Vec<usize>> {
        match self.get(key) {
            None => Ok(Vec::new()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| {
                    v.as_u64()
                        .map(|n| n as usize)
                        .ok_or_else(|| self.invalid(key, "an array of integers"))
                })
                .collect(),
            Some(Value::String(s)) => s
                .split(',')
                .map(|n| n.trim().parse())
                .collect::<Result<_, _>>()
                .map_err(|_| self.invalid(key, "an array of integers")),
            Some(v) => v
                .as_u64()
                .map(|n| vec![n as usize])
                .ok_or_else(|| self.invalid(key, "an array of integers")),
        }
    }

    /// An array of strings, or one comma-separated string.
    fn strings(&self, key: &str) -> anyhow::Result<Vec<String>> {
        match self.get(key) {
            None => Ok(Vec::new()),
            Some(Value::String(s)) => Ok(s
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| self.invalid(key, "an array of strings"))
                })
                .collect(),
            Some(_) => Err(self.invalid(key, "an array of strings")),
        }
    }

    fn req_strings(&self, key: &str) -> anyhow::Result<Vec<String>> {
        let items = self.strings(key)?;
        if items.is_empty() {
            return Err(self.missing(key));
        }
        Ok(items)
    }

    fn missing(&self, key: &str) -> anyhow::Error {
        anyhow::Error::new(CliError {
            message: format!("{key} required"),
            help: Some(format!("Call {} with `{key}` set", self.tool)),
            code: 2,
        })
    }

    fn invalid(&self, key: &str, want: &str) -> anyhow::Error {
        anyhow::Error::new(CliError {
            message: format!("{key} must be {want}"),
            help: Some(format!("Call {} with `{key}` as {want}", self.tool)),
            code: 2,
        })
    }
}

/// The CLI command a tool stands for, or `None` for a tool with no CLI twin.
fn command(a: &Args) -> anyhow::Result<Option<Command>> {
    let branch = |cmd| Command::Branch {
        cmd: Some(cmd),
        opts: BranchOpts::default(),
    };
    let rebase = |step: &str| Command::Rebase {
        onto: None,
        branch: None,
        onto_new: None,
        edit: false,
        root: false,
        autosquash: false,
        exec: Vec::new(),
        update_refs: false,
        strategy_option: None,
        more: RebaseFlags::default(),
        cont: step == "continue",
        skip: step == "skip",
        abort: step == "abort",
        quit: step == "quit",
        edit_todo: false,
        show_current_patch: step == "show-current-patch",
    };
    let stash = |cmd| Command::Stash {
        cmd: Some(cmd),
        push: StashPush::default(),
        paths: Vec::new(),
    };
    let remote = |cmd| Command::Remote {
        cmd: Some(cmd),
        verbose: false,
    };
    let worktree = |cmd| Command::Worktree { cmd: Some(cmd) };
    let workspace = |cmd| Command::Workspace { cmd: Some(cmd) };
    let stack = |cmd| Command::Stack { cmd: Some(cmd) };
    let lanes = |cmd| Command::Lanes { cmd: Some(cmd) };
    let flow = |cmd| Command::Flow { cmd };
    let index = |action| Command::Index { action };
    let tag = |names, message, force, delete| Command::Tag {
        names,
        message,
        annotate: false,
        force,
        delete,
        list: false,
        lines: None,
        contains: None,
        points_at: None,
    };
    let stash_index = || match a.str("index") {
        Some(s) => crate::cli::stash_ref(&s)
            .map(Some)
            .map_err(|_| a.invalid("index", "N or stash@{N}")),
        None => a.num("index").map(|n| n.map(|n| n as usize)),
    };
    let format = || DiffFormat {
        patch: a.flag("patch"),
        stat: a.flag("stat"),
        name_only: a.flag("name_only"),
        name_status: a.flag("name_status"),
        numstat: a.flag("numstat"),
    };
    Ok(Some(match a.tool {
        "git_status" => Command::Status {
            porcelain: None,
            short: false,
            branch: false,
            z: false,
            untracked: a.str("untracked"),
            ignored: a.flag("ignored"),
            verbose: 0,
            ahead_behind: false,
            no_ahead_behind: false,
            paths: a.strs("paths").unwrap_or_default(),
        },
        "git_log" => Command::Log {
            limit: a.num("limit")?.map_or(20, |n| n as usize),
            skip: a.num("skip")?.map_or(0, |n| n as usize),
            all: a.flag("all"),
            author: a.str("author"),
            since: a.str("since"),
            until: a.str("until"),
            oneline: false,
            grep: a.strs("grep").unwrap_or_default(),
            ignore_case: a.flag("ignore_case"),
            first_parent: a.flag("first_parent"),
            merges: a.flag("merges"),
            no_merges: a.flag("no_merges"),
            reverse: a.flag("reverse"),
            follow: a.flag("follow"),
            format: format(),
            revs: a.strs("rev").unwrap_or_default(),
            paths: a.strs("path").unwrap_or_default(),
        },
        "git_diff" => Command::Diff {
            revs: a.str("from").into_iter().chain(a.str("to")).collect(),
            paths: a.strs("paths").unwrap_or_default(),
            cached: a.flag("cached"),
            format: format(),
            unified: a.num("unified")?.map(|n| n as u32),
            ignore_all_space: a.flag("ignore_all_space"),
            ignore_space_change: false,
        },
        "git_show" => Command::Show {
            revs: a.strs("rev").unwrap_or_default(),
            paths: a.strs("paths").unwrap_or_default(),
            format: format(),
            no_patch: a.flag("no_patch"),
        },
        "git_blame" => Command::Blame {
            args: a.str("rev").into_iter().chain([a.req("path")?]).collect(),
            lines: a.str("lines"),
        },
        "git_refs" => Command::Refs,
        "git_branches" => Command::Branch {
            cmd: None,
            opts: BranchOpts {
                all: a.flag("all"),
                remotes: a.flag("remotes"),
                list: true,
                verbose: if a.flag("verbose") { 2 } else { 0 },
                merged: a.str("merged"),
                no_merged: a.str("no_merged"),
                contains: a.str("contains"),
                args: a.strs("pattern").unwrap_or_default(),
                ..BranchOpts::default()
            },
        },
        "git_tags" => Command::Tag {
            names: a.strs("pattern").unwrap_or_default(),
            message: None,
            annotate: false,
            force: false,
            delete: false,
            list: true,
            lines: None,
            contains: a.str("contains"),
            points_at: a.str("points_at"),
        },
        "git_stashes" => stash(StashCmd::List),
        "git_remotes" => Command::Remote {
            cmd: None,
            verbose: a.flag("verbose"),
        },
        "git_worktrees" => Command::Worktree { cmd: None },
        "git_describe" => Command::Describe {
            rev: a.str("rev"),
            tags: a.flag("tags"),
            dirty: a.flag("dirty"),
            long: a.flag("long"),
            abbrev: a.num("abbrev")?.map(|n| n as u32),
            always: false,
            pattern: a.str("match"),
            exact_match: a.flag("exact_match"),
        },
        "index_build" => index(IndexCmd::Build {
            root: a.str("root"),
        }),
        "code_search" => index(IndexCmd::Code {
            query: a.req("query")?,
            limit: a.num("limit")?.map_or(8, |n| n as usize),
            root: a.str("root"),
        }),
        "semantic_search" => index(IndexCmd::Search {
            query: a.req("query")?,
            limit: a.num("limit")?.map_or(8, |n| n as usize),
            root: a.str("root"),
        }),

        "git_stage" => Command::Stage {
            paths: a.strs("path")?,
            hunk: a.nums("hunk")?.into_iter().map(|n| n as u32).collect(),
            lines: a.nums("lines")?,
        },
        "git_unstage" => Command::Unstage {
            paths: a.strs("path")?,
            hunk: a.nums("hunk")?.into_iter().map(|n| n as u32).collect(),
            lines: a.nums("lines")?,
        },
        "git_stage_all" => Command::StageAll,
        "git_unstage_all" => Command::UnstageAll,
        "git_discard" => Command::Discard {
            paths: a.strs("path")?,
            hunk: a.nums("hunk")?.into_iter().map(|n| n as u32).collect(),
            lines: a.nums("lines")?,
        },
        "git_add" => Command::Add {
            paths: a.strs("paths").unwrap_or_default(),
            all: a.flag("all"),
            update: a.flag("update"),
            force: a.flag("force"),
            dry_run: a.flag("dry_run"),
            verbose: false,
            intent_to_add: a.flag("intent_to_add"),
            ignore_errors: false,
            patch: false,
        },
        "git_restore" => Command::Restore {
            paths: a.strs("paths")?,
            source: a.str("source"),
            staged: a.flag("staged"),
            worktree: a.flag("worktree"),
            ours: a.flag("ours"),
            theirs: a.flag("theirs"),
            overlay: false,
            no_overlay: false,
            patch: false,
            quiet: false,
        },
        "git_resolve" => Command::Resolve {
            path: a.req("path")?,
            ours: a.flag("ours"),
            theirs: !a.flag("ours"),
        },
        "git_commit" => Command::Commit {
            message: a.str("message").into_iter().collect(),
            file: None,
            amend: a.flag("amend"),
            no_edit: a.flag("no_edit"),
            all: a.flag("all"),
            no_verify: a.flag("no_verify"),
            author: a.str("author"),
            signoff: a.flag("signoff"),
            allow_empty: a.flag("allow_empty"),
            fixup: a.str("fixup"),
            squash: a.str("squash"),
            edit: false,
            reuse_message: a.str("reuse_message"),
            reedit_message: None,
            reset_author: a.flag("reset_author"),
            date: a.str("date"),
            dry_run: false,
            include: false,
            only: false,
            verbose: false,
            quiet: false,
            paths: a.strs("paths").unwrap_or_default(),
        },
        "git_extend" => Command::Extend,
        "git_fetch" => Command::Fetch {
            repository: None,
            refspecs: a.strs("refspecs").unwrap_or_default(),
            all: a.flag("all"),
            prune: a.flag("prune"),
            remote: a.str("remote"),
            tags: a.flag("tags"),
            depth: a.num("depth")?.map_or(0, |n| n as i32),
            dry_run: a.flag("dry_run"),
        },
        "git_pull" => Command::Pull {
            repository: a.str("remote"),
            branch: a.str("branch"),
            rebase: a.flag("rebase"),
            no_rebase: a.flag("no_rebase"),
            ff_only: a.flag("ff_only"),
        },
        "git_push" => Command::Push {
            repository: None,
            refspecs: a
                .strs("refspecs")
                .unwrap_or_default()
                .into_iter()
                .chain(a.str("delete"))
                .collect(),
            force: a.flag("force"),
            force_with_lease: a.flag("force_with_lease"),
            set_upstream: a.flag("set_upstream"),
            remote: a.str("remote"),
            tags: a.flag("tags"),
            all: a.flag("all"),
            delete: a.str("delete").is_some(),
            dry_run: a.flag("dry_run"),
        },
        "git_checkout" => {
            let (branch, force_branch) = match a.flag("force") {
                true => (None, a.str("create")),
                false => (a.str("create"), None),
            };
            Command::Checkout {
                rev: a.str("rev"),
                pathspec: Vec::new(),
                branch,
                force_branch,
                detach: a.flag("detach"),
                track: a.flag("track"),
                no_track: a.flag("no_track"),
                force: a.flag("discard_changes"),
                merge: a.flag("merge"),
                conflict: None,
                orphan: a.str("orphan"),
                ours: a.flag("ours"),
                theirs: a.flag("theirs"),
                no_guess: false,
                guess: false,
                patch: false,
                quiet: false,
                paths: a.strs("paths").unwrap_or_default(),
            }
        }
        "git_switch" => {
            let (create, force_create) = match a.flag("force") {
                true => (None, a.str("create")),
                false => (a.str("create"), None),
            };
            Command::Switch {
                rev: a.str("branch"),
                create,
                force_create,
                detach: a.flag("detach"),
                track: a.flag("track"),
                no_track: a.flag("no_track"),
                discard_changes: a.flag("discard_changes"),
                merge: a.flag("merge"),
                conflict: None,
                orphan: a.str("orphan"),
                no_guess: false,
                guess: false,
                quiet: false,
            }
        }
        "git_merge" => Command::Merge {
            revs: a.strs("rev").unwrap_or_default(),
            no_ff: a.flag("no_ff"),
            ff_only: a.flag("ff_only"),
            squash: a.flag("squash"),
            no_commit: a.flag("no_commit"),
            message: a.str("message"),
            file: None,
            strategy_option: a.str("strategy_option"),
            strategy: a.str("strategy"),
            allow_unrelated_histories: a.flag("allow_unrelated_histories"),
            log: a.num("log")?.map(|n| n as usize),
            stat: false,
            no_stat: a.flag("no_stat"),
            edit: false,
            no_edit: false,
            no_verify: a.flag("no_verify"),
            verify: false,
            signoff: a.flag("signoff"),
            quiet: false,
            cont: a.flag("continue"),
            abort: a.flag("abort"),
            quit: a.flag("quit"),
        },
        "git_rebase" => Command::Rebase {
            onto: a.str("onto"),
            branch: a.str("branch"),
            onto_new: a.str("newbase"),
            edit: false,
            root: a.flag("root"),
            autosquash: a.flag("autosquash"),
            exec: a.strings("exec")?,
            update_refs: a.flag("update_refs"),
            strategy_option: a.str("strategy_option"),
            more: RebaseFlags {
                keep_empty: a.flag("keep_empty"),
                force_rebase: a.flag("force_rebase"),
                rebase_merges: a
                    .flag("rebase_merges")
                    .then(|| "no-rebase-cousins".to_owned()),
                keep_base: a.flag("keep_base"),
                committer_date_is_author_date: a.flag("committer_date_is_author_date"),
                reset_author_date: a.flag("reset_author_date"),
                reapply_cherry_picks: a.flag("reapply_cherry_picks"),
                empty: a.str("empty"),
                signoff: a.flag("signoff"),
                autostash: a.flag("autostash"),
                no_verify: a.flag("no_verify"),
                ..Default::default()
            },
            cont: false,
            skip: false,
            abort: false,
            quit: false,
            edit_todo: false,
            show_current_patch: false,
        },
        "git_rebase_continue" => rebase("continue"),
        "git_rebase_skip" => rebase("skip"),
        "git_rebase_abort" => rebase("abort"),
        "git_rebase_quit" => rebase("quit"),
        "git_rebase_current_patch" => rebase("show-current-patch"),
        "git_cherry_pick" => Command::CherryPick {
            revs: a.strs("rev").unwrap_or_default(),
            no_commit: a.flag("no_commit"),
            record_origin: a.flag("record_origin"),
            mainline: a.num("mainline")?.map(|m| m as u32),
            strategy_option: a.str("strategy_option"),
            edit: false,
            no_edit: false,
            signoff: a.flag("signoff"),
            ff: a.flag("ff"),
            allow_empty: a.flag("allow_empty"),
            keep_redundant_commits: false,
            empty: a.str("empty"),
            cont: a.flag("continue"),
            skip: a.flag("skip"),
            abort: a.flag("abort"),
            quit: a.flag("quit"),
        },
        "git_revert" => Command::Revert {
            revs: a.strs("rev").unwrap_or_default(),
            no_commit: a.flag("no_commit"),
            mainline: a.num("mainline")?.map(|m| m as u32),
            strategy_option: a.str("strategy_option"),
            edit: false,
            no_edit: false,
            signoff: a.flag("signoff"),
            reference: a.flag("reference"),
            cont: a.flag("continue"),
            skip: a.flag("skip"),
            abort: a.flag("abort"),
            quit: a.flag("quit"),
        },
        "git_reset" => {
            let mode = a.str("mode");
            let (soft, hard, keep, merge) = match mode.as_deref() {
                None | Some("mixed") => (false, false, false, false),
                Some("soft") => (true, false, false, false),
                Some("hard") => (false, true, false, false),
                Some("keep") => (false, false, true, false),
                Some("merge") => (false, false, false, true),
                Some(_) => {
                    return Err(a.invalid("mode", "one of soft, mixed, hard, keep, merge"));
                }
            };
            Command::Reset {
                rev: a.str("rev"),
                pathspec: Vec::new(),
                soft,
                mixed: false,
                hard,
                keep,
                merge,
                patch: false,
                quiet: false,
                paths: a.strs("paths").unwrap_or_default(),
            }
        }
        "git_undo" => Command::Undo,
        "git_redo" => Command::Redo,
        "git_oplog" => Command::Oplog,
        "git_smartlog" => Command::Smartlog,
        "git_absorb" => Command::Absorb,
        "git_reword" => Command::Reword {
            message: a.req("message")?,
            rev: a.or("rev", "HEAD"),
        },
        "git_uncommit" => Command::Uncommit {
            n: a.num("n")?.map_or(1, |n| n as usize),
        },
        "git_squash" => Command::Squash {
            rev: a.or("rev", "HEAD"),
            from: a.str("from"),
        },
        "git_split" => Command::Split {
            rev: a.or("rev", "HEAD"),
            paths: a.req_strings("paths")?,
        },
        "git_move" => Command::Move {
            rev: a.req("rev")?,
            before: a.str("before"),
            after: a.str("after"),
        },
        "git_prune" => branch(BranchCmd::Prune {
            base: a.or("base", "HEAD"),
        }),
        "git_sync" => Command::Sync,
        "git_submit" => Command::Submit,

        "git_flow_init" => flow(FlowCmd::Init {
            preset: a.req("preset")?,
        }),
        "git_flow_start" => flow(FlowCmd::Start {
            name: a.req("name")?,
        }),
        "git_flow_finish" => flow(FlowCmd::Finish),
        "git_flow_release" => flow(FlowCmd::Release {
            version: a.req("version")?,
            finish: a.flag("finish"),
        }),
        "git_flow_status" => flow(FlowCmd::Status),

        "git_workspace_new" => workspace(WorkspaceCmd::New {
            name: a.req("name")?,
        }),
        "git_workspace_list" => workspace(WorkspaceCmd::List),
        "git_workspace_remove" => workspace(WorkspaceCmd::Remove {
            name: a.req("name")?,
        }),

        "git_stack_new" => stack(StackCmd::New {
            name: a.req("name")?,
        }),
        "git_stack_list" => stack(StackCmd::List),
        "git_stack_restack" => stack(StackCmd::Restack),

        "git_lanes_list" => lanes(LanesCmd::List),
        "git_lanes_init" => lanes(LanesCmd::Init),
        "git_lanes_off" => lanes(LanesCmd::Off),
        "git_lanes_new" => lanes(LanesCmd::New {
            name: a.req("name")?,
        }),
        "git_lanes_stack" => lanes(LanesCmd::Stack {
            name: a.req("name")?,
            on: a.req("on")?,
        }),
        "git_lanes_assign" => lanes(LanesCmd::Assign {
            lane: a.req("lane")?,
            path: a.req("path")?,
            hunk: a.num("hunk")?.map(|n| n as u32),
        }),
        "git_lanes_unassign" => lanes(LanesCmd::Unassign {
            path: a.req("path")?,
        }),
        "git_lanes_commit" => lanes(LanesCmd::Commit {
            lane: a.req("lane")?,
            message: a.req("message")?,
        }),
        "git_lanes_rename" => lanes(LanesCmd::Rename {
            old: a.req("old")?,
            new: a.req("new")?,
        }),
        "git_lanes_delete" => lanes(LanesCmd::Delete {
            name: a.req("name")?,
        }),
        "git_lanes_push" => lanes(LanesCmd::Push {
            lane: a.req("lane")?,
        }),
        "git_lanes_pr" => lanes(LanesCmd::Pr {
            lane: a.req("lane")?,
        }),
        "git_lanes_restack" => lanes(LanesCmd::Restack),
        "git_bisect" => {
            let revs = a.strings("revs")?;
            let cmd = match a.req("command")?.as_str() {
                "start" => BisectCmd::Start {
                    revs,
                    term_new: a.str("term_new"),
                    term_old: a.str("term_old"),
                    no_checkout: a.flag("no_checkout"),
                    first_parent: a.flag("first_parent"),
                    paths: a.strings("paths")?,
                },
                "bad" => BisectCmd::Bad { revs },
                "good" => BisectCmd::Good { revs },
                "new" => BisectCmd::New { revs },
                "old" => BisectCmd::Old { revs },
                "skip" => BisectCmd::Skip { revs },
                "reset" => BisectCmd::Reset {
                    commit: revs.into_iter().next(),
                },
                "log" => BisectCmd::Log,
                "replay" => BisectCmd::Replay {
                    file: a.req("file")?,
                },
                "run" => BisectCmd::Run {
                    cmd: a.req_strings("cmd")?,
                },
                "visualize" | "view" => BisectCmd::Visualize,
                "terms" => BisectCmd::Terms {
                    good: false,
                    bad: false,
                },
                term => BisectCmd::Mark(std::iter::once(term.to_owned()).chain(revs).collect()),
            };
            Command::Bisect { cmd }
        }

        "git_stash_push" => stash(StashCmd::Push {
            push: StashPush {
                message: a.str("message"),
                include_untracked: a.flag("include_untracked"),
                keep_index: a.flag("keep_index"),
            },
            paths: a.strs("paths").unwrap_or_default(),
        }),
        "git_stash_pop" => stash(StashCmd::Pop {
            index: stash_index()?,
            restore_index: a.flag("restore_index"),
        }),
        "git_stash_apply" => stash(StashCmd::Apply {
            index: stash_index()?,
            restore_index: a.flag("restore_index"),
        }),
        "git_stash_drop" => stash(StashCmd::Drop {
            index: stash_index()?,
        }),
        "git_stash_show" => stash(StashCmd::Show {
            index: stash_index()?,
            patch: a.flag("patch"),
            name_only: a.flag("name_only"),
            stat: false,
        }),
        "git_stash_branch" => stash(StashCmd::Branch {
            name: a.req("name")?,
            index: stash_index()?,
        }),
        "git_stash_clear" => stash(StashCmd::Clear),

        "git_branch_create" => branch(BranchCmd::Create {
            name: a.req("name")?,
            start: a.str("start"),
        }),
        "git_branch_delete" => branch(BranchCmd::Delete {
            names: a.strs("name")?,
            force: a.flag("force"),
        }),
        "git_branch_rename" => branch(BranchCmd::Rename {
            old: a.req("old")?,
            new: a.req("new")?,
        }),
        "git_branch_set_upstream" => Command::Branch {
            cmd: None,
            opts: BranchOpts {
                set_upstream_to: a.str("upstream"),
                unset_upstream: a.str("upstream").is_none(),
                args: a.str("name").into_iter().collect(),
                ..BranchOpts::default()
            },
        },

        "git_tag_create" => tag(
            [Some(a.req("name")?), a.str("rev")]
                .into_iter()
                .flatten()
                .collect(),
            a.str("message"),
            a.flag("force"),
            false,
        ),
        "git_tag_delete" => tag(a.strs("name")?, None, false, true),

        "git_remote_add" => remote(RemoteCmd::Add {
            name: a.req("name")?,
            url: a.req("url")?,
        }),
        "git_remote_remove" => remote(RemoteCmd::Remove {
            name: a.req("name")?,
        }),
        "git_remote_set_url" => remote(RemoteCmd::SetUrl {
            name: a.req("name")?,
            url: a.req("url")?,
            push: a.flag("push"),
        }),
        "git_remote_rename" => remote(RemoteCmd::Rename {
            old: a.req("old")?,
            new: a.req("new")?,
        }),
        "git_remote_get_url" => remote(RemoteCmd::GetUrl {
            name: a.req("name")?,
            push: a.flag("push"),
            all: a.flag("all"),
        }),
        "git_remote_prune" => remote(RemoteCmd::Prune {
            names: a.strs("name")?,
        }),

        "git_worktree_add" => worktree(WorktreeCmd::Add {
            path: a.req("path")?,
            commitish: a.str("branch"),
            new_branch: a.str("new_branch"),
            detach: a.flag("detach"),
        }),
        "git_worktree_lock" => worktree(WorktreeCmd::Lock {
            name: a.req("name")?,
            reason: a.str("reason"),
        }),
        "git_worktree_unlock" => worktree(WorktreeCmd::Unlock {
            name: a.req("name")?,
        }),
        "git_worktree_move" => worktree(WorktreeCmd::Move {
            name: a.req("name")?,
            new_path: a.req("new_path")?,
        }),
        "git_worktree_remove" => worktree(WorktreeCmd::Remove {
            name: a.req("name")?,
            force: a.flag("force"),
        }),
        "git_worktree_prune" => worktree(WorktreeCmd::Prune),

        "git_config" | "git_config_set" => {
            let set = a.tool == "git_config_set";
            let unset = set && a.flag("unset");
            Command::Config {
                key: if set {
                    Some(a.req("key")?)
                } else {
                    a.str("key")
                },
                value: if set && !unset {
                    Some(a.req("value")?)
                } else {
                    None
                },
                global: a.flag("global"),
                local: a.flag("local"),
                get: false,
                get_all: a.flag("all"),
                unset,
                unset_all: false,
                list: !set && a.str("key").is_none(),
                add: a.flag("add"),
                as_bool: false,
                as_int: false,
            }
        }
        "git_apply" => Command::Apply {
            patches: a.strs("patch")?,
            cached: a.flag("cached"),
            index: a.flag("index"),
            check: a.flag("check"),
            reverse: a.flag("reverse"),
            stat: a.flag("stat"),
        },
        "git_notes" | "git_note_add" | "git_note_remove" => Command::Notes {
            notes_ref: a.str("ref"),
            cmd: Some(match a.tool {
                "git_notes" => match a.str("rev") {
                    Some(rev) => NotesCmd::Show { rev: Some(rev) },
                    None => NotesCmd::List { rev: None },
                },
                "git_note_remove" => NotesCmd::Remove { rev: a.str("rev") },
                _ if a.flag("append") => NotesCmd::Append {
                    rev: a.str("rev"),
                    message: vec![a.req("message")?],
                },
                _ => NotesCmd::Add {
                    rev: a.str("rev"),
                    message: vec![a.req("message")?],
                    force: a.flag("force"),
                },
            }),
        },
        "git_update_ref" => {
            let delete = a.flag("delete");
            Command::UpdateRef {
                name: a.req("name")?,
                new: if delete {
                    a.str("old")
                } else {
                    Some(a.req("new")?)
                },
                old: if delete { None } else { a.str("old") },
                delete,
                no_deref: a.flag("no_deref"),
                message: a.str("message"),
            }
        }
        "git_hash_object" => Command::HashObject {
            paths: a.strs("path")?,
            write: a.flag("write"),
            stdin: false,
            kind: a.or("type", "blob"),
        },
        "git_format_patch" => Command::FormatPatch {
            revs: a
                .num("count")?
                .map(|n| format!("-{n}"))
                .into_iter()
                .chain(a.str("range"))
                .collect(),
            output_dir: a.str("output_dir"),
            stdout: a.flag("stdout"),
        },
        "git_am" => {
            let (abort, cont, skip) = (a.flag("abort"), a.flag("continue"), a.flag("skip"));
            Command::Am {
                // Never read stdin: it carries the MCP protocol.
                mbox: if abort || cont || skip {
                    Vec::new()
                } else {
                    a.strs("mbox")?
                },
                abort,
                cont,
                skip,
                three_way: a.flag("three_way"),
                signoff: a.flag("signoff"),
            }
        }
        "git_archive" => Command::Archive {
            rev: a.str("rev"),
            paths: a.strs("paths").unwrap_or_default(),
            format: a.str("format"),
            output: Some(a.req("output")?),
            prefix: a.str("prefix"),
        },
        "git_gc" => Command::Gc {
            prune: a.str("prune"),
            aggressive: a.flag("aggressive"),
            auto: false,
        },
        "git_fsck" => Command::Fsck {
            full: a.flag("full"),
            strict: a.flag("strict"),
            unreachable: a.flag("unreachable"),
            no_dangling: a.flag("no_dangling"),
            connectivity_only: false,
        },
        "git_clean" => Command::Clean {
            dry_run: a.flag("dry_run"),
            ignored_too: a.flag("ignored"),
            only_ignored: a.flag("only_ignored"),
            exclude: a.strs("exclude").unwrap_or_default(),
            dirs: true,
            force: true,
            quiet: false,
            paths: a.strs("paths").unwrap_or_default(),
        },
        "git_rm" => Command::Rm {
            paths: a.strs("path")?,
            cached: a.flag("cached"),
            recursive: a.flag("recursive"),
            force: a.flag("force"),
            dry_run: a.flag("dry_run"),
            quiet: false,
            ignore_unmatch: a.flag("ignore_unmatch"),
        },
        "git_mv" => Command::Mv {
            paths: [a.strs("from")?, vec![a.req("to")?]].concat(),
            force: a.flag("force"),
            skip_errors: a.flag("skip_errors"),
            dry_run: a.flag("dry_run"),
            verbose: false,
        },
        "git_rev_parse" => Command::Plumbing(Plumbing::RevParse {
            short: a.num("short")?.map(|n| n as usize),
            abbrev_ref: a.flag("abbrev_ref"),
            symbolic_full_name: a.flag("symbolic_full_name"),
            verify: a.flag("verify"),
            quiet: false,
            show_toplevel: a.flag("show_toplevel"),
            git_dir: a.flag("git_dir"),
            absolute_git_dir: false,
            show_prefix: false,
            show_cdup: false,
            inside_work_tree: false,
            inside_git_dir: false,
            bare: false,
            revs: a.strings("revs")?,
        }),
        "git_ls_files" => Command::Plumbing(Plumbing::LsFiles {
            cached: false,
            stage: a.flag("stage"),
            others: a.flag("others"),
            ignored: a.flag("ignored"),
            exclude_standard: a.flag("exclude_standard"),
            modified: a.flag("modified"),
            deleted: a.flag("deleted"),
            unmerged: a.flag("unmerged"),
            z: false,
            full_name: false,
            paths: a.strings("paths")?,
        }),
        "git_ls_tree" => Command::Plumbing(Plumbing::LsTree {
            recursive: a.flag("recursive"),
            only_trees: false,
            show_trees: false,
            long: a.flag("long"),
            name_only: false,
            object_only: false,
            abbrev: None,
            z: false,
            full_name: false,
            full_tree: false,
            rev: a.req("rev")?,
            paths: a.strings("paths")?,
        }),
        "git_cat_file" => {
            let (kind, size, exists) = (a.flag("kind"), a.flag("size"), a.flag("exists"));
            Command::Plumbing(Plumbing::CatFile {
                kind,
                size,
                pretty: !(kind || size || exists),
                exists,
                args: vec![a.req("object")?],
            })
        }
        "git_show_ref" => Command::Plumbing(Plumbing::ShowRef {
            heads: a.flag("heads"),
            tags: a.flag("tags"),
            verify: false,
            dereference: a.flag("dereference"),
            hash: None,
            head: false,
            quiet: false,
            patterns: a.strings("patterns")?,
        }),
        "git_for_each_ref" => Command::Plumbing(Plumbing::ForEachRef {
            format: a.str("format"),
            sort: a.strings("sort")?,
            count: a.num("count")?.map(|n| n as usize),
            patterns: a.strings("patterns")?,
        }),
        "git_rev_list" => Command::Plumbing(Plumbing::RevList {
            max_count: a.num("max_count")?.map(|n| n as usize),
            count: a.flag("count"),
            all: a.flag("all"),
            reverse: a.flag("reverse"),
            first_parent: a.flag("first_parent"),
            merges: a.flag("merges"),
            no_merges: a.flag("no_merges"),
            parents: false,
            revs: a.strings("revs")?,
        }),
        "git_merge_base" => Command::Plumbing(Plumbing::MergeBase {
            all: a.flag("all"),
            is_ancestor: a.flag("is_ancestor"),
            a: a.req("a")?,
            b: a.req("b")?,
        }),
        "git_reflog" => Command::Plumbing(Plumbing::Reflog {
            max_count: a.num("max_count")?.map(|n| n as usize),
            args: a.str("ref").into_iter().collect(),
        }),
        "git_shortlog" => Command::Plumbing(Plumbing::Shortlog {
            summary: false,
            numbered: a.flag("numbered"),
            email: a.flag("email"),
            committer: false,
            all: a.flag("all"),
            revs: a.strings("revs")?,
        }),
        "git_grep_tracked" => Command::Plumbing(Plumbing::Grep {
            ignore_case: a.flag("ignore_case"),
            word: a.flag("word"),
            invert: a.flag("invert"),
            line_number: true,
            files: false,
            count: false,
            quiet: false,
            fixed: a.flag("fixed"),
            extended: true,
            perl: false,
            patterns: a.req_strings("patterns")?,
            cached: a.flag("cached"),
            args: a.str("rev").into_iter().collect(),
            paths: a.strings("paths")?,
        }),
        "git_check_ignore" => Command::Plumbing(Plumbing::CheckIgnore {
            verbose: true,
            quiet: false,
            non_matching: false,
            no_index: false,
            paths: a.req_strings("paths")?,
        }),
        "git_var" => Command::Plumbing(Plumbing::Var {
            name: a.req("name")?,
        }),
        "git_symbolic_ref" => Command::Plumbing(Plumbing::SymbolicRef {
            short: a.flag("short"),
            quiet: false,
            name: a.or("name", "HEAD"),
            target: None,
        }),
        "git_count_objects" => Command::Plumbing(Plumbing::CountObjects {
            verbose: a.flag("verbose"),
        }),
        "git_run" => Command::Git {
            args: a.req_strings("args")?,
        },
        _ => return Ok(None),
    }))
}

/// Tools that read the object store directly and have no CLI command.
fn local(backend: &Arc<dyn GitBackend>, a: &Args, full: bool) -> anyhow::Result<Output> {
    Ok(match a.tool {
        "git_tree" => {
            let rev = a.or("rev", "HEAD");
            let path = a.or("path", "");
            let entries = backend.list_tree(&rev, &path)?;
            let rows = entries
                .iter()
                .map(|e| {
                    crate::obj! {
                        "name" => e.name,
                        "kind" => if e.is_dir { "dir" } else { "file" },
                        "size" => e.size as usize,
                        "path" => e.path,
                    }
                })
                .collect();
            let at = if path.is_empty() {
                rev
            } else {
                format!("{rev}:{path}")
            };
            Output::new(crate::render::tree(&entries))
                .list(
                    "entries",
                    rows,
                    &["name", "kind", "size"],
                    format!("0 entries at {at}"),
                )
                .help("Call git_blob with a file's path to read it")
                .help("Call git_tree with a dir's path to list it")
        }
        "git_blob" => {
            let blob = backend.read_blob(&a.or("rev", "HEAD"), &a.req("path")?)?;
            let out = Output::new(crate::render::blob(&blob))
                .with("path", blob.path.as_str())
                .with("size", blob.size as usize);
            match blob.text {
                Some(text) => out.long("content", text),
                None => out.with("content", format!("binary file, {} bytes", blob.size)),
            }
        }
        "git_files" => {
            let rev = a.or("rev", "HEAD");
            let mut files = backend.list_files(&rev)?;
            let out = Output::new(files.join("\n"));
            let total = files.len();
            if total == 0 {
                return Ok(out.with("files", format!("0 files in {rev}")));
            }
            if full || total <= FILES_SHOWN {
                return Ok(out.with("count", total).with("files", files));
            }
            files.truncate(FILES_SHOWN);
            out.with("count", format!("{FILES_SHOWN} of {total} files"))
                .with("files", files)
                .help(format!(
                    "Call git_files with full=true to see all {total} files"
                ))
        }
        "git_grep" => {
            let query = GrepQuery {
                pattern: a.req("pattern")?,
                regex: a.flag("regex"),
                path: a.str("path"),
                exts: a
                    .strings("ext")?
                    .iter()
                    .map(|e| e.trim_start_matches('.').to_lowercase())
                    .collect(),
            };
            let matches = backend.grep_query(&query)?;
            let total = matches.len();
            let shown = if full { total } else { total.min(GREP_SHOWN) };
            let rows = matches[..shown]
                .iter()
                .map(|m| crate::obj! { "path" => m.path, "line" => m.line, "text" => m.text })
                .collect();
            let mut out = Output::new(crate::render::grep(&matches));
            if shown < total {
                out = out
                    .with("count", format!("{shown} of {total} matches"))
                    .help(format!(
                        "Call git_grep with full=true to see all {total} matches"
                    ));
            }
            out.list(
                "matches",
                rows,
                &["path", "line", "text"],
                format!("0 matches for {:?}", query.pattern),
            )
        }
        other => {
            return Err(anyhow::Error::new(CliError {
                message: format!("unknown tool: {other}"),
                help: Some("Call tools/list to see the available tools".to_owned()),
                code: 2,
            }));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn init_repo(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rgit-mcp-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@e"],
            vec!["config", "user.name", "t"],
        ] {
            git(&dir, &args);
        }
        std::fs::write(dir.join("f.txt"), "x\n").unwrap();
        git(&dir, &["add", "f.txt"]);
        git(&dir, &["commit", "-qm", "c0"]);
        dir
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }

    fn open(dir: &std::path::Path) -> Arc<dyn GitBackend> {
        Arc::new(Git2Backend::discover(dir).unwrap())
    }

    /// A call's TOON text, or its TOON error, as the client would see it.
    fn call(backend: &Arc<dyn GitBackend>, name: &str, args: Value) -> Result<String, String> {
        dispatch(backend, name, &args)
            .map(|v| crate::toon::encode(&v))
            .map_err(|e| crate::toon::encode(&failure(&e)))
    }

    fn field<'a>(value: &'a Obj, key: &str) -> Option<&'a Node> {
        value.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    #[test]
    fn registry_defaults_to_bound_repo_and_opens_others_by_path() {
        let a = init_repo("reg-a");
        let b = init_repo("reg-b");
        let reg = Registry::new(open(&a));

        let canon_a = std::fs::canonicalize(&a).unwrap();
        assert_eq!(reg.resolve(None).unwrap().workdir(), canon_a);
        assert_eq!(reg.resolve(Some("")).unwrap().workdir(), canon_a);

        let canon_b = std::fs::canonicalize(&b).unwrap();
        let first = reg.resolve(Some(b.to_str().unwrap())).unwrap();
        assert_eq!(first.workdir(), canon_b);
        let second = reg.resolve(Some(b.to_str().unwrap())).unwrap();
        assert!(
            Arc::ptr_eq(&first, &second),
            "same repo resolves to a cached backend"
        );

        assert!(reg.resolve(Some("/no/such/repo/xyzzy")).is_err());

        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn dispatch_wires_the_new_flags() {
        let dir = init_repo("dispatch");
        let backend = open(&dir);
        let run = |name: &str, args: Value| call(&backend, name, args);

        std::fs::write(dir.join("junk"), "j\n").unwrap();
        let out = run("git_clean", json!({ "dry_run": true })).unwrap();
        assert!(out.contains("junk"), "dry-run lists junk: {out}");
        assert!(dir.join("junk").exists(), "dry-run does not delete");

        std::fs::write(dir.join("f.txt"), "x\nmore\n").unwrap();
        run("git_stash_push", json!({})).unwrap();
        assert!(dir.join("junk").exists(), "bare stash keeps untracked junk");
        run("git_stash_pop", json!({})).unwrap();

        git(&dir, &["checkout", "-qb", "feat"]);
        std::fs::write(dir.join("f.txt"), "onfeat\n").unwrap();
        git(&dir, &["commit", "-qam", "feat"]);
        git(&dir, &["checkout", "-q", "main"]);
        assert!(
            run("git_branch_delete", json!({ "name": "feat" })).is_err(),
            "unmerged delete without force errors"
        );
        let deleted = run(
            "git_branch_delete",
            json!({ "name": "feat", "force": true }),
        )
        .unwrap();
        assert!(
            deleted.starts_with("result: deleted branch feat"),
            "{deleted}"
        );

        run(
            "git_remote_add",
            json!({ "name": "origin", "url": "https://e.com/a.git" }),
        )
        .unwrap();
        run(
            "git_remote_set_url",
            json!({ "name": "origin", "url": "https://e.com/b.git" }),
        )
        .unwrap();
        assert!(
            backend
                .remotes()
                .unwrap()
                .iter()
                .any(|r| r.url.contains("b.git"))
        );
        run("git_remote_rename", json!({ "old": "origin", "new": "up" })).unwrap();
        assert!(backend.remotes().unwrap().iter().any(|r| r.name == "up"));

        run("git_tag_create", json!({ "name": "v1", "message": "one" })).unwrap();
        std::fs::write(dir.join("f.txt"), "x\ndirty\n").unwrap();
        assert_eq!(run("git_describe", json!({})).unwrap(), "result: v1");
        assert_eq!(
            run("git_describe", json!({ "dirty": true })).unwrap(),
            "result: v1-dirty"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_restore_and_switch_tools() {
        let dir = init_repo("add-switch");
        let backend = open(&dir);
        let run = |name: &str, args: Value| call(&backend, name, args);
        std::fs::write(dir.join("f.txt"), "y\n").unwrap();
        std::fs::write(dir.join("n.txt"), "n\n").unwrap();
        run("git_add", json!({ "paths": "." })).unwrap();
        run(
            "git_restore",
            json!({ "paths": ["f.txt"], "staged": true, "worktree": true }),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "x\n");
        let staged: Vec<String> = backend
            .status()
            .unwrap()
            .staged
            .into_iter()
            .map(|d| d.path)
            .collect();
        assert_eq!(staged, ["n.txt"]);

        run("git_switch", json!({ "create": "feat" })).unwrap();
        let back = run("git_switch", json!({ "branch": "-" })).unwrap();
        assert!(back.starts_with("result: checked out main"), "{back}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A tool with a CLI twin prints exactly what `axi::run` + `finalize` does,
    /// with its CLI hints rewritten as tool calls.
    #[test]
    fn tools_match_the_cli_agent_output() {
        let dir = init_repo("parity");
        let backend = open(&dir);
        std::fs::write(dir.join("new.txt"), "n\n").unwrap();
        let cli = |command: crate::cli::Command| {
            let mut out = crate::axi::run(&backend, command, false).unwrap();
            out.help = out.help.iter().map(|h| as_tool_call(h)).collect();
            crate::toon::encode(&out.finalize(&[], false, "rgit").unwrap())
        };
        assert_eq!(
            call(&backend, "git_status", json!({})).unwrap(),
            cli(crate::cli::Command::Status {
                porcelain: None,
                short: false,
                branch: false,
                z: false,
                untracked: None,
                ignored: false,
                verbose: 0,
                ahead_behind: false,
                no_ahead_behind: false,
                paths: Vec::new(),
            })
        );
        assert_eq!(
            call(&backend, "git_smartlog", json!({})).unwrap(),
            cli(crate::cli::Command::Smartlog)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fields_full_and_no_ops() {
        let dir = init_repo("fields");
        let backend = open(&dir);

        let log = dispatch(&backend, "git_log", &json!({ "fields": ["oid"] })).unwrap();
        let Some(Node::List(rows)) = field(&log, "commits") else {
            panic!("commits table");
        };
        let Node::Obj(row) = &rows[0] else {
            panic!("row");
        };
        let columns: Vec<&str> = row.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(columns, ["id", "summary", "author", "when", "oid"]);

        let err = call(&backend, "git_log", json!({ "fields": "nope" })).unwrap_err();
        assert!(err.contains("unknown field nope"), "{err}");
        assert!(err.contains("valid fields for commits"), "{err}");

        std::fs::write(dir.join("f.txt"), "y\n".repeat(2000)).unwrap();
        let patch = call(&backend, "git_diff", json!({ "patch": true })).unwrap();
        assert!(patch.contains("truncated"), "{patch}");
        assert!(
            patch.contains("Call git_diff with full=true to see the complete patch"),
            "{patch}"
        );
        let whole = call(&backend, "git_diff", json!({ "patch": true, "full": true })).unwrap();
        assert!(!whole.contains("truncated"));

        let created = call(&backend, "git_branch_create", json!({ "name": "b1" })).unwrap();
        assert!(
            created.starts_with("result: created and checked out branch b1"),
            "{created}"
        );
        let again = call(&backend, "git_branch_create", json!({ "name": "b1" })).unwrap();
        assert!(
            again.starts_with("result: branch b1 already exists (no-op)"),
            "{again}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cli_hints_become_tool_calls() {
        let cases = [
            (
                "Run `rgit stage <path>` to stage a file",
                "Call git_stage with path=<path> to stage a file",
            ),
            (
                "Run `rgit diff` to see unstaged changes",
                "Call git_diff to see unstaged changes",
            ),
            (
                "Run `rgit commit -m \"<message>\"` to commit staged changes",
                "Call git_commit with message=\"<message>\" to commit staged changes",
            ),
            (
                "Run `rgit push --set-upstream` to publish topic",
                "Call git_push with set_upstream=true to publish topic",
            ),
            (
                "Run `rgit log --limit 65` to see all 65 commits",
                "Call git_log with limit=65 to see all 65 commits",
            ),
            (
                "Run `rgit rebase --continue` after resolving conflicts",
                "Call git_rebase_continue after resolving conflicts",
            ),
            (
                "Run `rgit stash pop` to restore the newest one",
                "Call git_stash_pop to restore the newest one",
            ),
            (
                "Run `rgit init` to create one here",
                "Run `rgit init` to create one here",
            ),
            (
                "Call git_diff with full=true",
                "Call git_diff with full=true",
            ),
        ];
        for (cli, mcp) in cases {
            assert_eq!(as_tool_call(cli), mcp);
        }
    }

    #[test]
    fn history_tools_take_ranges_paths_and_revisions() {
        let dir = init_repo("history");
        let backend = open(&dir);
        std::fs::write(dir.join("g.txt"), "gee\n").unwrap();
        std::fs::write(dir.join("f.txt"), "y\n").unwrap();
        git(&dir, &["commit", "-qam", "c1"]);
        git(&dir, &["add", "g.txt"]);
        git(&dir, &["commit", "-qm", "c2"]);

        let log = call(&backend, "git_log", json!({ "rev": "HEAD~1..HEAD" })).unwrap();
        assert!(log.contains("c2") && !log.contains("c1"), "{log}");
        let log = call(
            &backend,
            "git_log",
            json!({ "path": ["f.txt"], "grep": "1$" }),
        )
        .unwrap();
        assert!(log.contains("c1") && !log.contains("c0"), "{log}");
        let diff = call(
            &backend,
            "git_diff",
            json!({ "from": "HEAD~2..HEAD", "paths": "g.txt", "name_only": true }),
        )
        .unwrap();
        assert!(diff.contains("g.txt") && !diff.contains("f.txt"), "{diff}");
        let show = call(&backend, "git_show", json!({ "rev": "HEAD:g.txt" })).unwrap();
        assert!(show.contains("gee"), "{show}");
        let blame = call(
            &backend,
            "git_blame",
            json!({ "path": "f.txt", "rev": "HEAD~2" }),
        )
        .unwrap();
        assert!(blame.contains(",x"), "{blame}");
    }

    #[test]
    fn errors_carry_help() {
        let dir = init_repo("errors");
        let backend = open(&dir);

        let missing = call(&backend, "git_blame", json!({})).unwrap_err();
        assert_eq!(
            missing,
            "error: path required\nhelp[1]: Call git_blame with `path` set"
        );

        let nothing = call(&backend, "git_commit", json!({ "message": "m" })).unwrap_err();
        assert!(nothing.starts_with("error: "), "{nothing}");
        assert!(
            nothing.contains("Call git_stage with path=<path>"),
            "{nothing}"
        );

        let unknown = call(&backend, "git_nope", json!({})).unwrap_err();
        assert!(unknown.contains("tools/list"), "{unknown}");

        let bad = call(&backend, "git_reset", json!({ "rev": "HEAD", "mode": "x" })).unwrap_err();
        assert!(
            bad.contains("mode must be one of soft, mixed, hard"),
            "{bad}"
        );

        let result = reply(failure(&anyhow::anyhow!("boom")), true);
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.structured_content, Some(json!({ "error": "boom" })));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_tools_print_toon() {
        let dir = init_repo("local");
        let backend = open(&dir);
        let tree = call(&backend, "git_tree", json!({})).unwrap();
        assert!(tree.starts_with("entries[1]{name,kind,size}:"), "{tree}");
        let files = call(&backend, "git_files", json!({})).unwrap();
        assert!(files.starts_with("count: 1\nfiles[1]: f.txt"), "{files}");
        let grep = call(&backend, "git_grep", json!({ "pattern": "zzz" })).unwrap();
        assert!(grep.starts_with("matches: \"0 matches for "), "{grep}");
        let blob = call(&backend, "git_blob", json!({ "path": "f.txt" })).unwrap();
        assert!(blob.contains("path: f.txt"), "{blob}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_tool_is_wired() {
        const LOCAL: &[&str] = &["git_tree", "git_blob", "git_files", "git_grep"];
        for t in tools() {
            let args = json!({});
            let wired = command(&Args {
                tool: &t.name,
                args: &args,
            });
            assert!(
                !matches!(wired, Ok(None)) || LOCAL.contains(&t.name.as_ref()),
                "{} is not wired",
                t.name
            );
        }
        for name in READ_ONLY_TOOLS {
            assert!(tools().iter().any(|t| t.name == *name), "{name} is listed");
        }
    }
}
