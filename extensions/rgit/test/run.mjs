// Drives a generated rgit extension against a fake pi/omp API with the real rgit binary.
// Usage: node run.mjs <generated extension .ts> <repo dir> <mcp|native>
import assert from "node:assert/strict";

// The core set, as `CORE_TOOLS` in crates/rgit-cli/src/mcp.rs.
const CORE = [
  "git_status",
  "git_diff",
  "git_log",
  "git_show",
  "git_stage",
  "git_unstage",
  "git_commit",
  "git_smartlog",
  "git_undo",
  "git_grep",
];

const [file, repo, mode] = process.argv.slice(2);
const handlers = {};
const tools = {};
let server;
const api = {
  on: (name, fn) => (handlers[name] = fn),
  registerTool: (def) => (tools[def.name] = def),
};
// Only pi takes an MCP server; omp gets the core set as native tools.
if (mode === "mcp") api.registerMcpServer = (name, config) => (server = { name, config });
const mod = await import(file);
mod.default(api);
const ctx = { cwd: repo };

// Session start: the repo state goes in once, hidden, with the first prompt.
await handlers.session_start({ reason: "startup" }, ctx);
const first = await handlers.before_agent_start({ prompt: "hi" }, ctx);
assert.equal(first.message.customType, "rgit");
assert.equal(first.message.display, false);
assert.match(first.message.content, /branch: main/);
assert.match(first.message.content, /--toon/);
assert.equal(await handlers.before_agent_start({ prompt: "again" }, ctx), undefined);

if (mode === "mcp") {
  // rgit's MCP server, with the core tools in the prompt and the rest for codemode.
  assert.deepEqual(Object.keys(tools), []);
  assert.equal(server.name, "rgit");
  assert.equal(typeof server.config.command, "string");
  assert.deepEqual(server.config.args, ["mcp"]);
  assert.equal(server.config.exposure, "codemode");
  assert.ok(server.config.description.length > 0);
  for (const name of CORE) assert.equal(server.config.toolExposure[name], "direct");
} else {
  // The core tools only, as native tools that run `rgit tool <name>` in the session's folder.
  assert.deepEqual(Object.keys(tools).sort(), [...CORE].sort());
  assert.equal(tools.git_status.parameters.type, "object");
  assert.equal(tools.git_status.parameters.properties.repo.type, "string");
  const status = await tools.git_status.execute("t1", {}, undefined, undefined, ctx);
  assert.match(status.content[0].text, /branch: main/);
  const log = await tools.git_log.execute("t2", { limit: 1 }, undefined, undefined, ctx);
  assert.match(log.content[0].text, /init/);
  await assert.rejects(tools.git_show.execute("t3", { rev: "no-such-rev" }, undefined, undefined, ctx), /error/);
}

console.log(`ok ${mode}`);
