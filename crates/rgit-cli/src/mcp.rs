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

use rgit_git::{Git2Backend, GitBackend, GrepQuery, LogOptions, ResetMode};
use serde_json::{Map, Value, json};

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
}

/// Maps a tool call's `repo` argument to a backend. Empty/absent uses the repo
/// the server started in; a value opens another repo on demand (cached).
struct Registry {
    default: Arc<dyn GitBackend>,
    cache: std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, Arc<dyn GitBackend>>>,
}

impl Registry {
    fn new(default: Arc<dyn GitBackend>) -> Self {
        Self {
            default,
            cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Resolve the `repo` argument to a backend. NOTE: this is the local, trusted
    /// resolver - a value is treated as a filesystem path. A hosted server must
    /// replace it with name-within-a-managed-root lookup plus access control, and
    /// never accept arbitrary paths.
    fn resolve(&self, repo: Option<&str>) -> Result<Arc<dyn GitBackend>, String> {
        let path = match repo {
            None => return Ok(self.default.clone()),
            Some(p) if p.trim().is_empty() => return Ok(self.default.clone()),
            Some(p) => p,
        };
        let canon = std::fs::canonicalize(path).map_err(|e| format!("no such repo {path:?}: {e}"))?;
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
    let service = RgitMcp {
        registry: Registry::new(backend),
    }
    .serve(stdio())
    .await?;
    service.waiting().await?;
    Ok(())
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
            "Drive a git repository. Each tool maps to a git operation; results are compact plain \
             text, one item per line. Key flows: inspect with git_status / git_smartlog / git_log \
             before acting; stage (git_stage / git_stage_all) then git_commit with a clear message; \
             sync with git_fetch then git_pull or git_rebase. Every destructive tool is auto-\
             snapshotted, so git_undo reverses the last operation (recovering uncommitted work), \
             git_redo replays it, and git_oplog lists the history. For step-by-step playbooks, read \
             the prompts (embedded skills): commit_changes, sync_with_remote, resolve_conflicts, \
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
        Ok(ListToolsResult::with_all_items(tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let name = request.name.to_string();
        let args = Value::Object(request.arguments.unwrap_or_default());
        let backend = match self
            .registry
            .resolve(args.get("repo").and_then(Value::as_str))
        {
            Ok(b) => b,
            Err(e) => return Ok(CallToolResult::error(vec![ContentBlock::text(e)]).into()),
        };
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
         3. git_diff (patch=true) shows staged changes; git_log shows recent history; git_show a \
         revision for one commit's detail.\n\
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
            "git_tree",
            "List one directory of a revision's tree (rev defaults to HEAD, path to the root).",
            &[("rev", "string", false), ("path", "string", false)],
        ),
        tool(
            "git_blob",
            "Read a file's contents at a revision (rev defaults to HEAD).",
            &[("path", "string", true), ("rev", "string", false)],
        ),
        tool(
            "git_files",
            "Every file path in a revision's tree (rev defaults to HEAD).",
            &[("rev", "string", false)],
        ),
        tool(
            "git_grep",
            "Search the working tree for a literal string (case-insensitive, parallel, gitignore-aware). Returns path:line: text matches. Optionally scope the search with `regex`, `path`, and `ext`.",
            &[
                ("pattern", "string", true),
                ("regex", "boolean", false),
                ("path", "string", false),
                ("ext", "string[]", false),
            ],
        ),
        tool(
            "index_build",
            "Build (or incrementally rebuild) the semantic index. Local by default (the current repo, or `repo`); `root` builds every git repo directly under a directory (global). Incremental: unchanged files are reused by git blob OID, only edited files are re-embedded. Reports per-repo chunk counts.",
            &[("repo", "string", false), ("root", "string", false)],
        ),
        tool(
            "code_search",
            "Best general code search: fuses literal grep and semantic ranking (reciprocal-rank fusion), then re-ranks by git history (churn and recency) so hot files surface first, and tags each hit lexical/semantic/both. Prefer this over git_grep or semantic_search alone. Local by default; `root` searches every repo under a directory (global). Needs an index (`index_build`); without it, degrades to grep. Returns score, path:line, and the tag.",
            &[
                ("query", "string", true),
                ("limit", "number", false),
                ("root", "string", false),
            ],
        ),
        tool(
            "semantic_search",
            "Search the codebase by meaning only, using the local embedding index (build it first with index_build), re-ranked by git history (churn and recency). Prefer code_search for general use; use this for purely conceptual matches. Local by default; `root` searches every repo under a directory (global). Returns score, path, and line range per hit.",
            &[
                ("query", "string", true),
                ("limit", "number", false),
                ("root", "string", false),
            ],
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
            "Push the current branch to its upstream, or to `remote` if given.",
            &[
                ("force", "boolean", false),
                ("force_with_lease", "boolean", false),
                ("set_upstream", "boolean", false),
                ("remote", "string", false),
            ],
        ),
        tool(
            "git_checkout",
            "Check out a branch (or a revision/tag as detached HEAD).",
            &[("rev", "string", true)],
        ),
        tool(
            "git_merge",
            "Merge a revision into the current branch (no_ff forces a merge commit).",
            &[("rev", "string", true), ("no_ff", "boolean", false), ("ff_only", "boolean", false)],
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
            "Undo the last operation from the op-log, restoring HEAD and the working tree (recovers uncommitted work).",
            none,
        ),
        tool("git_redo", "Redo the operation most recently undone.", none),
        tool(
            "git_oplog",
            "The operation log (undo stack), newest first.",
            none,
        ),
        tool(
            "git_smartlog",
            "Smartlog: your local/draft commits and the trunk they branch from.",
            none,
        ),
        tool(
            "git_absorb",
            "Fold each modified file's changes into the newest local commit that touched it (fixup + autosquash).",
            none,
        ),
        tool(
            "git_reword",
            "Change a commit's message (default HEAD) and restack descendants. Op-log-safe.",
            &[("message", "string", true), ("rev", "string", false)],
        ),
        tool(
            "git_uncommit",
            "Undo the last commit(s), keeping the changes staged (default 1).",
            &[("n", "number", false)],
        ),
        tool(
            "git_squash",
            "Fold a commit into its parent (default HEAD); with `from`, fold every commit after `from` up to HEAD into one. Restacks descendants.",
            &[("rev", "string", false), ("from", "string", false)],
        ),
        tool(
            "git_split",
            "Split a commit (default HEAD) into two by path: the given `paths`' changes first, the rest second. Restacks descendants.",
            &[("paths", "string[]", true), ("rev", "string", false)],
        ),
        tool(
            "git_move",
            "Reorder a commit before or after another in the current branch's history. Pass exactly one of `before`/`after`.",
            &[
                ("rev", "string", true),
                ("before", "string", false),
                ("after", "string", false),
            ],
        ),
        tool(
            "git_prune",
            "Delete local branches fully merged into a base (default HEAD).",
            &[("base", "string", false)],
        ),
        tool(
            "git_sync",
            "Fetch, fast-forward branches to their upstreams, and restack the stack.",
            none,
        ),
        tool(
            "git_submit",
            "Push every branch in the current stack and open a pull request per branch (via gh/glab).",
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
            "Show the active workflow and its policy.",
            none,
        ),
        tool(
            "git_workspace_new",
            "Create a copy-on-write clone of the repo on a new branch (parallel isolated work).",
            &[("name", "string", true)],
        ),
        tool(
            "git_workspace_list",
            "List this repo's copy-on-write workspaces.",
            none,
        ),
        tool(
            "git_workspace_remove",
            "Remove a copy-on-write workspace.",
            &[("name", "string", true)],
        ),
        tool(
            "git_stack_new",
            "Create a new branch stacked on the current one.",
            &[("name", "string", true)],
        ),
        tool(
            "git_stack_list",
            "List local branches with their stacked-branch parent.",
            none,
        ),
        tool(
            "git_stack_restack",
            "Rebase every stacked branch onto its parent's new tip.",
            none,
        ),
        tool(
            "git_lanes_list",
            "List the lanes and the uncommitted files each owns.",
            none,
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
            "Create a new lane that commits to a same-named branch.",
            &[("name", "string", true)],
        ),
        tool(
            "git_lanes_stack",
            "Create a new lane stacked on another (its commits build on that lane's branch).",
            &[("name", "string", true), ("on", "string", true)],
        ),
        tool(
            "git_lanes_assign",
            "Assign a worktree path to a lane, or a single hunk with `hunk` set to its new-file start line.",
            &[
                ("lane", "string", true),
                ("path", "string", true),
                ("hunk", "number", false),
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
            "Delete a lane (its changes return to default; its branch is kept).",
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
            "Move each stacked lane onto its parent lane's new tip (in the odb; the worktree is untouched).",
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
            "Delete a local branch. `force` deletes even if not fully merged.",
            &[("name", "string", true), ("force", "boolean", false)],
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
            "Remove every untracked file and directory. Destructive. `dry_run` lists what would be removed without deleting.",
            &[("dry_run", "boolean", false)],
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
                offset: args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize,
                all: flag("all"),
                author: s("author").map(str::to_owned),
                rev: s("rev").map(str::to_owned),
                ..LogOptions::default()
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
        "git_tree" => backend
            .list_tree(s("rev").unwrap_or("HEAD"), s("path").unwrap_or(""))
            .map(|t| crate::render::tree(&t))
            .map_err(emap),
        "git_blob" => backend
            .read_blob(s("rev").unwrap_or("HEAD"), req("path")?)
            .map(|b| crate::render::blob(&b))
            .map_err(emap),
        "git_files" => backend
            .list_files(s("rev").unwrap_or("HEAD"))
            .map(|f| f.join("\n"))
            .map_err(emap),
        "git_grep" => {
            let exts: Vec<String> = match args.get("ext") {
                Some(Value::Array(_)) => str_vec("ext"),
                Some(Value::String(s)) => s
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect(),
                _ => Vec::new(),
            }
            .iter()
            .map(|e| e.trim_start_matches('.').to_lowercase())
            .collect();
            let q = GrepQuery {
                pattern: req("pattern")?.to_owned(),
                regex: flag("regex"),
                path: s("path").map(str::to_owned),
                exts,
            };
            backend
                .grep_query(&q)
                .map(|m| crate::render::grep(&m))
                .map_err(emap)
        }
        "index_build" => crate::cli::index_build(backend, s("root")).map_err(|e| e.to_string()),
        "code_search" => {
            let query = req("query")?;
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(8) as usize;
            crate::cli::code_search(backend, s("root"), query, limit).map_err(|e| e.to_string())
        }
        "semantic_search" => {
            let query = req("query")?;
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(8) as usize;
            crate::cli::semantic_search(backend, s("root"), query, limit).map_err(|e| e.to_string())
        }
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
            } else if flag("no_verify") {
                backend.commit_no_verify(message)
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
            s("remote"),
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
        "git_merge" => done(backend.merge(req("rev")?, flag("no_ff"), flag("ff_only"), &|_| {})),
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
        "git_undo" => backend.undo().map_err(emap),
        "git_redo" => backend.redo().map_err(emap),
        "git_oplog" => backend
            .oplog()
            .map(|e| crate::render::oplog(&e))
            .map_err(emap),
        "git_smartlog" => backend
            .smartlog()
            .map(|e| crate::render::smartlog(&e))
            .map_err(emap),
        "git_absorb" => backend.absorb().map_err(emap),
        "git_reword" => done(backend.reword(s("rev").unwrap_or("HEAD"), req("message")?)),
        "git_uncommit" => done(backend.uncommit(
            args.get("n").and_then(Value::as_u64).unwrap_or(1) as usize,
        )),
        "git_squash" => match s("from") {
            Some(from) => done(backend.squash_range(from)),
            None => done(backend.squash(s("rev").unwrap_or("HEAD"))),
        },
        "git_split" => done(backend.split(s("rev").unwrap_or("HEAD"), &str_vec("paths"))),
        "git_move" => match (s("before"), s("after")) {
            (Some(t), None) => done(backend.reorder(req("rev")?, t, true)),
            (None, Some(t)) => done(backend.reorder(req("rev")?, t, false)),
            _ => Err("pass exactly one of before or after".to_owned()),
        },
        "git_prune" => backend
            .prune_merged(s("base").unwrap_or("HEAD"))
            .map(|d| {
                if d.is_empty() {
                    "no merged branches".to_owned()
                } else {
                    format!("deleted: {}", d.join(", "))
                }
            })
            .map_err(emap),
        "git_sync" => backend
            .sync(&|_| {})
            .map(|o| {
                let mut m = "synced".to_owned();
                if !o.restacked.is_empty() {
                    m.push_str(&format!("; restacked {}", o.restacked.join(", ")));
                }
                if !o.conflicted.is_empty() {
                    m.push_str(&format!("; conflicts in {}", o.conflicted.join(", ")));
                }
                m
            })
            .map_err(emap),
        "git_submit" => backend.submit_stack(&|_| {}).map(|n| n.join("\n")).map_err(emap),

        "git_flow_init" => rgit_git::workflow::init(backend.as_ref(), req("preset")?).map_err(emap),
        "git_flow_start" => rgit_git::workflow::start(backend.as_ref(), req("name")?).map_err(emap),
        "git_flow_finish" => rgit_git::workflow::finish(backend.as_ref()).map_err(emap),
        "git_flow_release" => {
            rgit_git::workflow::release(backend.as_ref(), req("version")?, flag("finish"))
                .map_err(emap)
        }
        "git_flow_status" => rgit_git::workflow::status(backend.as_ref()).map_err(emap),

        "git_workspace_new" => {
            rgit_git::workspace::create(backend.as_ref(), req("name")?).map_err(emap)
        }
        "git_workspace_list" => rgit_git::workspace::list(backend.as_ref()).map_err(emap),
        "git_workspace_remove" => {
            rgit_git::workspace::remove(backend.as_ref(), req("name")?).map_err(emap)
        }

        "git_stack_new" => backend.stack_new(req("name")?).map_err(emap),
        "git_stack_list" => backend
            .stack_parents()
            .map(|parents| {
                let lines: Vec<String> = parents
                    .iter()
                    .map(|(b, p)| match p {
                        Some(p) => format!("{b} (on {p})"),
                        None => format!("{b} (base)"),
                    })
                    .collect();
                if lines.is_empty() {
                    "no branches".to_owned()
                } else {
                    lines.join("\n")
                }
            })
            .map_err(emap),
        "git_stack_restack" => backend
            .restack()
            .map(|o| crate::stack::render_restack(&o))
            .map_err(emap),
        "git_lanes_list" => backend
            .lanes_state()
            .map(|state| {
                let mut lines = Vec::new();
                for lane in &state.lanes {
                    lines.push(format!("{} [{}]", lane.name, lane.branch));
                    for (short, summary) in &lane.commits {
                        lines.push(format!("  * {short} {summary}"));
                    }
                    for path in &lane.paths {
                        lines.push(format!("  {path}"));
                    }
                    for h in &lane.hunks {
                        let short: String = h.anchor.chars().take(7).collect();
                        lines.push(format!("  {} (hunk {short})", h.path));
                    }
                }
                if lines.is_empty() {
                    "no lanes".to_owned()
                } else {
                    lines.join("\n")
                }
            })
            .map_err(emap),
        "git_lanes_init" => backend.lanes_init().map(|()| "lanes on".to_owned()).map_err(emap),
        "git_lanes_off" => backend.lanes_off().map(|()| "lanes off".to_owned()).map_err(emap),
        "git_lanes_new" => backend
            .lane_new(req("name")?)
            .map(|()| "ok".to_owned())
            .map_err(emap),
        "git_lanes_stack" => backend
            .lane_stack(req("name")?, req("on")?)
            .map(|()| "ok".to_owned())
            .map_err(emap),
        "git_lanes_assign" => match hunk {
            Some(new_start) => backend.lane_assign_hunk(req("lane")?, req("path")?, new_start),
            None => backend.lane_assign(req("lane")?, req("path")?),
        }
        .map(|()| "ok".to_owned())
        .map_err(emap),
        "git_lanes_unassign" => backend
            .lane_unassign(req("path")?)
            .map(|()| "ok".to_owned())
            .map_err(emap),
        "git_lanes_commit" => backend
            .lane_commit(req("lane")?, req("message")?)
            .map_err(emap),
        "git_lanes_rename" => backend
            .lane_rename(req("old")?, req("new")?)
            .map(|()| "ok".to_owned())
            .map_err(emap),
        "git_lanes_delete" => backend
            .lane_delete(req("name")?)
            .map(|()| "ok".to_owned())
            .map_err(emap),
        "git_lanes_push" => backend.lane_push(req("lane")?).map_err(emap),
        "git_lanes_pr" => backend.lane_pr(req("lane")?).map_err(emap),
        "git_lanes_restack" => backend
            .lane_restack()
            .map(|o| crate::stack::render_restack(&o))
            .map_err(emap),
        "git_bisect" => match backend.bisect(&str_vec("args")) {
            Ok(out) if out.is_empty() => Ok("ok".to_owned()),
            Ok(out) => Ok(out),
            Err(e) => Err(e.to_string()),
        },

        "git_stash_push" => match s("message") {
            Some(m) => backend.stash_push_message(m),
            None => backend.stash_push(),
        }
        .map_err(emap),
        "git_stash_pop" => done(backend.stash_pop(index())),
        "git_stash_apply" => done(backend.stash_apply(index())),
        "git_stash_drop" => done(backend.stash_drop(index())),

        "git_branch_create" => done(backend.create_branch(req("name")?)),
        "git_branch_delete" => done(backend.delete_branch(req("name")?, flag("force"))),
        "git_branch_rename" => done(backend.rename_branch(req("old")?, req("new")?)),

        "git_tag_create" => done(backend.create_tag(req("name")?, s("message").unwrap_or(""))),
        "git_tag_delete" => done(backend.delete_tag(req("name")?)),

        "git_remote_add" => done(backend.add_remote(req("name")?, req("url")?)),
        "git_remote_remove" => done(backend.remove_remote(req("name")?)),

        "git_worktree_add" => done(backend.add_worktree(req("name")?, req("path")?)),
        "git_worktree_remove" => done(backend.remove_worktree(req("name")?)),

        "git_clean" => backend.clean(flag("dry_run")).map(|o| if flag("dry_run") { o } else { "ok".to_owned() }).map_err(emap),
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
            assert!(Command::new("git").arg("-C").arg(&dir).args(&args).status().unwrap().success());
        }
        std::fs::write(dir.join("f.txt"), "x\n").unwrap();
        for args in [vec!["add", "f.txt"], vec!["commit", "-qm", "c0"]] {
            assert!(Command::new("git").arg("-C").arg(&dir).args(&args).status().unwrap().success());
        }
        dir
    }

    #[test]
    fn registry_defaults_to_bound_repo_and_opens_others_by_path() {
        let a = init_repo("reg-a");
        let b = init_repo("reg-b");
        let default: Arc<dyn GitBackend> = Arc::new(Git2Backend::discover(&a).unwrap());
        let reg = Registry::new(default);

        // Empty / absent resolves to the bound repo.
        let canon_a = std::fs::canonicalize(&a).unwrap();
        assert_eq!(reg.resolve(None).unwrap().workdir(), canon_a);
        assert_eq!(reg.resolve(Some("")).unwrap().workdir(), canon_a);

        // A path opens that repo, and a second resolve is cached (same Arc).
        let canon_b = std::fs::canonicalize(&b).unwrap();
        let first = reg.resolve(Some(b.to_str().unwrap())).unwrap();
        assert_eq!(first.workdir(), canon_b);
        let second = reg.resolve(Some(b.to_str().unwrap())).unwrap();
        assert!(Arc::ptr_eq(&first, &second), "same repo resolves to a cached backend");

        // A missing path errors instead of panicking.
        assert!(reg.resolve(Some("/no/such/repo/xyzzy")).is_err());

        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }
}
