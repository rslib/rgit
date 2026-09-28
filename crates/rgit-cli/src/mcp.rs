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
    BranchCmd, CliError, Command, DiffFormat, FlowCmd, IndexCmd, LanesCmd, RemoteCmd, StackCmd,
    StashCmd, WorkspaceCmd, WorktreeCmd,
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
    "git_worktrees",
    "git_describe",
    "git_smartlog",
    "git_oplog",
    "git_stashes",
    "git_branches",
    "git_lanes_list",
    "git_stack_list",
    "git_workspace_list",
    "git_flow_status",
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
             lists only those.",
            &[
                ("all", "boolean", false),
                ("remotes", "boolean", false),
                FIELDS,
            ],
        ),
        tool(
            "git_tags",
            "Tags as a table of name, when; extra field message.",
            &[FIELDS],
        ),
        tool(
            "git_stashes",
            "The stash list as a table of index, message.",
            &[FIELDS],
        ),
        tool(
            "git_remotes",
            "Configured remotes as a table of name, url.",
            &[FIELDS],
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
            "git_resolve",
            "Resolve a conflicted path by taking `ours` (else theirs) and staging it.",
            &[("path", "string", true), ("ours", "boolean", false)],
        ),
        tool(
            "git_commit",
            "Commit the index with a message (runs hooks). `amend` replaces HEAD, `all` stages \
             tracked changes first, `no_verify` skips hooks. Returns the commit report.",
            &[
                ("message", "string", true),
                ("amend", "boolean", false),
                ("all", "boolean", false),
                ("no_verify", "boolean", false),
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
            "Check out a branch (or a revision/tag as detached HEAD). A no-op when already on it.",
            &[("rev", "string", true)],
        ),
        tool(
            "git_merge",
            "Merge a revision (or several, an octopus merge) into the current branch. `no_ff` \
             forces a merge commit, `ff_only` refuses a non-fast-forward, `squash` stages the \
             result without a merge commit, `no_commit` stops before committing, `message` sets \
             the commit message, `strategy_option` (ours|theirs) settles conflicting hunks. \
             `continue` commits a resolved merge, `abort` cancels a conflicted one.",
            &[
                ("rev", "paths", false),
                ("no_ff", "boolean", false),
                ("ff_only", "boolean", false),
                ("squash", "boolean", false),
                ("no_commit", "boolean", false),
                ("message", "string", false),
                ("strategy_option", "string", false),
                ("continue", "boolean", false),
                ("abort", "boolean", false),
            ],
        ),
        tool(
            "git_rebase",
            "Rebase the current branch onto a revision (`onto`; the upstream when omitted). \
             `newbase` replays the commits after `onto` onto it (git's --onto). `root` rebases \
             down to the root commit, `autosquash` folds fixup!/squash! commits, `exec` runs \
             shell commands after each commit, `update_refs` moves branches inside the range, \
             `strategy_option` (ours|theirs) settles conflicting hunks.",
            &[
                ("onto", "string", false),
                ("newbase", "string", false),
                ("root", "boolean", false),
                ("autosquash", "boolean", false),
                ("exec", "string[]", false),
                ("update_refs", "boolean", false),
                ("strategy_option", "string", false),
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
            "git_cherry_pick",
            "Cherry-pick commits onto HEAD in order; `rev` is a commit, a range `A..B`, or a list. \
             `record_origin` appends \"(cherry picked from commit ...)\", `mainline` picks the \
             parent (from 1) of a merge commit, `strategy_option` (ours|theirs) settles \
             conflicting hunks. After a conflict: `continue`, `skip` or `abort`.",
            &[
                ("rev", "paths", false),
                ("no_commit", "boolean", false),
                ("record_origin", "boolean", false),
                ("mainline", "integer", false),
                ("strategy_option", "string", false),
                ("continue", "boolean", false),
                ("skip", "boolean", false),
                ("abort", "boolean", false),
            ],
        ),
        tool(
            "git_revert",
            "Revert commits on HEAD; `rev` is a commit, a range `A..B` (newest first), or a list. \
             `mainline` picks the parent (from 1) of a merge commit, `strategy_option` \
             (ours|theirs) settles conflicting hunks. After a conflict: `continue`, `skip` or \
             `abort`.",
            &[
                ("rev", "paths", false),
                ("no_commit", "boolean", false),
                ("mainline", "integer", false),
                ("strategy_option", "string", false),
                ("continue", "boolean", false),
                ("skip", "boolean", false),
                ("abort", "boolean", false),
            ],
        ),
        tool(
            "git_reset",
            "Reset HEAD to a revision. `mode` is soft, mixed (default), or hard.",
            &[("rev", "string", true), ("mode", "string", false)],
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
            "Run a `git bisect` subcommand (e.g. [\"start\",\"<bad>\",\"<good>\"], [\"good\"], \
             [\"bad\"], [\"reset\"]).",
            &[("args", "string[]", true), FULL],
        ),
        tool(
            "git_stash_push",
            "Stash the working tree and index (with an optional message). `include_untracked` \
             also stashes untracked files.",
            &[
                ("message", "string", false),
                ("include_untracked", "boolean", false),
            ],
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
            "Create a branch at HEAD and check it out. A no-op when it exists.",
            &[("name", "string", true)],
        ),
        tool(
            "git_branch_delete",
            "Delete a local branch; `force` deletes it even if not fully merged. A no-op when it \
             does not exist.",
            &[("name", "string", true), ("force", "boolean", false)],
        ),
        tool(
            "git_branch_rename",
            "Rename a local branch.",
            &[("old", "string", true), ("new", "string", true)],
        ),
        tool(
            "git_tag_create",
            "Create a tag at HEAD (annotated when `message` is given; `force` replaces one). A \
             no-op when it exists.",
            &[
                ("name", "string", true),
                ("message", "string", false),
                ("force", "boolean", false),
            ],
        ),
        tool(
            "git_tag_delete",
            "Delete a tag. A no-op when it does not exist.",
            &[("name", "string", true)],
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
            "Change a remote's URL.",
            &[("name", "string", true), ("url", "string", true)],
        ),
        tool(
            "git_remote_rename",
            "Rename a remote.",
            &[("old", "string", true), ("new", "string", true)],
        ),
        tool(
            "git_worktree_add",
            "Create a linked worktree at `path` on a new branch `name`. A no-op when it exists.",
            &[("name", "string", true), ("path", "string", true)],
        ),
        tool(
            "git_worktree_remove",
            "Remove a linked worktree; `force` removes it even if locked. A no-op when it does \
             not exist.",
            &[("name", "string", true), ("force", "boolean", false)],
        ),
        tool(
            "git_worktree_prune",
            "Prune worktree entries whose working tree is gone.",
            none,
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
             `cached` removes them from the index only; `recursive` is needed for a folder.",
            &[
                ("path", "paths", true),
                ("cached", "boolean", false),
                ("recursive", "boolean", false),
            ],
        ),
        tool(
            "git_mv",
            "Rename/move tracked files or folders. `from` may list several paths when `to` is a \
             folder. `force` overwrites an existing destination.",
            &[
                ("from", "paths", true),
                ("to", "string", true),
                ("force", "boolean", false),
            ],
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
    let output = match command(&a)? {
        Some(command) => crate::axi::run(backend, command, false)?,
        None => local(backend, &a, full)?,
    };
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
    ("stash pop", "git_stash_pop", &["index"]),
    ("stash list", "git_stashes", &[]),
    ("stash push", "git_stash_push", &["message"]),
    ("branch create", "git_branch_create", &["name"]),
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
    ("git ls-files", "git_files", &[]),
    ("status", "git_status", &[]),
    ("log", "git_log", &["rev"]),
    ("diff", "git_diff", &["from", "to"]),
    ("show", "git_show", &["rev"]),
    ("blame", "git_blame", &["path"]),
    ("stage", "git_stage", &["path"]),
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
    ("tag", "git_tag_create", &["name"]),
    ("remote", "git_remotes", &[]),
    ("worktree", "git_worktrees", &[]),
    ("workspace", "git_workspace_list", &[]),
    ("stack", "git_stack_list", &[]),
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
        all: false,
        remotes: false,
    };
    let rebase = |onto, cont, skip, abort| Command::Rebase {
        onto,
        onto_new: None,
        edit: false,
        root: false,
        autosquash: false,
        exec: Vec::new(),
        update_refs: false,
        strategy_option: None,
        cont,
        skip,
        abort,
    };
    let stash = |cmd| Command::Stash { cmd: Some(cmd) };
    let remote = |cmd| Command::Remote { cmd: Some(cmd) };
    let worktree = |cmd| Command::Worktree { cmd: Some(cmd) };
    let workspace = |cmd| Command::Workspace { cmd: Some(cmd) };
    let stack = |cmd| Command::Stack { cmd: Some(cmd) };
    let lanes = |cmd| Command::Lanes { cmd: Some(cmd) };
    let flow = |cmd| Command::Flow { cmd };
    let index = |action| Command::Index { action };
    let tag = |name, message, force, delete| Command::Tag {
        name,
        message,
        annotate: false,
        force,
        delete,
        list: false,
    };
    let stash_index = || a.num("index").map(|n| n.map(|n| n as usize));
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
            all: a.flag("all"),
            remotes: a.flag("remotes"),
        },
        "git_tags" => tag(None, None, false, None),
        "git_stashes" => stash(StashCmd::List),
        "git_remotes" => Command::Remote { cmd: None },
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
        "git_resolve" => Command::Resolve {
            path: a.req("path")?,
            ours: a.flag("ours"),
            theirs: !a.flag("ours"),
        },
        "git_commit" => Command::Commit {
            message: Some(a.req("message")?),
            amend: a.flag("amend"),
            all: a.flag("all"),
            no_verify: a.flag("no_verify"),
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
        "git_checkout" => Command::Checkout {
            rev: Some(a.req("rev")?),
            branch: None,
        },
        "git_merge" => Command::Merge {
            revs: a.strs("rev").unwrap_or_default(),
            no_ff: a.flag("no_ff"),
            ff_only: a.flag("ff_only"),
            squash: a.flag("squash"),
            no_commit: a.flag("no_commit"),
            message: a.str("message"),
            strategy_option: a.str("strategy_option"),
            no_edit: false,
            cont: a.flag("continue"),
            abort: a.flag("abort"),
        },
        "git_rebase" => Command::Rebase {
            onto: a.str("onto"),
            onto_new: a.str("newbase"),
            edit: false,
            root: a.flag("root"),
            autosquash: a.flag("autosquash"),
            exec: a.strings("exec")?,
            update_refs: a.flag("update_refs"),
            strategy_option: a.str("strategy_option"),
            cont: false,
            skip: false,
            abort: false,
        },
        "git_rebase_continue" => rebase(None, true, false, false),
        "git_rebase_skip" => rebase(None, false, true, false),
        "git_rebase_abort" => rebase(None, false, false, true),
        "git_cherry_pick" => Command::CherryPick {
            revs: a.strs("rev").unwrap_or_default(),
            no_commit: a.flag("no_commit"),
            record_origin: a.flag("record_origin"),
            mainline: a.num("mainline")?.map(|m| m as u32),
            strategy_option: a.str("strategy_option"),
            no_edit: false,
            cont: a.flag("continue"),
            skip: a.flag("skip"),
            abort: a.flag("abort"),
        },
        "git_revert" => Command::Revert {
            revs: a.strs("rev").unwrap_or_default(),
            no_commit: a.flag("no_commit"),
            mainline: a.num("mainline")?.map(|m| m as u32),
            strategy_option: a.str("strategy_option"),
            no_edit: false,
            cont: a.flag("continue"),
            skip: a.flag("skip"),
            abort: a.flag("abort"),
        },
        "git_reset" => {
            let (soft, hard) = match a.str("mode").as_deref() {
                None | Some("mixed") => (false, false),
                Some("soft") => (true, false),
                Some("hard") => (false, true),
                Some(_) => return Err(a.invalid("mode", "one of soft, mixed, hard")),
            };
            Command::Reset {
                rev: Some(a.req("rev")?),
                soft,
                hard,
                paths: Vec::new(),
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
        "git_bisect" => Command::Bisect {
            args: a.req_strings("args")?,
        },

        "git_stash_push" => stash(StashCmd::Push {
            message: a.str("message"),
            include_untracked: a.flag("include_untracked"),
        }),
        "git_stash_pop" => stash(StashCmd::Pop {
            index: stash_index()?,
        }),
        "git_stash_apply" => stash(StashCmd::Apply {
            index: stash_index()?,
        }),
        "git_stash_drop" => stash(StashCmd::Drop {
            index: stash_index()?,
        }),

        "git_branch_create" => branch(BranchCmd::Create {
            name: a.req("name")?,
        }),
        "git_branch_delete" => branch(BranchCmd::Delete {
            name: Some(a.req("name")?),
            force: a.flag("force"),
        }),
        "git_branch_rename" => branch(BranchCmd::Rename {
            old: a.req("old")?,
            new: a.req("new")?,
        }),

        "git_tag_create" => tag(
            Some(a.req("name")?),
            a.str("message"),
            a.flag("force"),
            None,
        ),
        "git_tag_delete" => tag(None, None, false, Some(a.req("name")?)),

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
        }),
        "git_remote_rename" => remote(RemoteCmd::Rename {
            old: a.req("old")?,
            new: a.req("new")?,
        }),

        "git_worktree_add" => worktree(WorktreeCmd::Add {
            name: a.req("name")?,
            path: a.req("path")?,
        }),
        "git_worktree_remove" => worktree(WorktreeCmd::Remove {
            name: a.req("name")?,
            force: a.flag("force"),
        }),
        "git_worktree_prune" => worktree(WorktreeCmd::Prune),

        "git_clean" => Command::Clean {
            dry_run: a.flag("dry_run"),
            ignored_too: a.flag("ignored"),
            only_ignored: a.flag("only_ignored"),
            exclude: a.strs("exclude").unwrap_or_default(),
            dirs: true,
            force: true,
            paths: a.strs("paths").unwrap_or_default(),
        },
        "git_rm" => Command::Rm {
            paths: a.strs("path")?,
            cached: a.flag("cached"),
            recursive: a.flag("recursive"),
            force: false,
        },
        "git_mv" => Command::Mv {
            paths: [a.strs("from")?, vec![a.req("to")?]].concat(),
            force: a.flag("force"),
        },
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
