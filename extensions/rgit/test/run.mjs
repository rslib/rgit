// Drives a generated rgit extension against a fake pi/omp API with the real rgit binary.
// Usage: node run.mjs <generated extension .ts> <repo dir> <tools|no-tools>
import assert from "node:assert/strict";

const [file, repo, mode] = process.argv.slice(2);
const handlers = {};
const tools = {};
const mod = await import(file);
mod.default({
  on: (name, fn) => (handlers[name] = fn),
  registerTool: (def) => (tools[def.name] = def),
});
const ctx = { cwd: repo };

// Session start: the repo state goes in once, hidden, with the first prompt.
await handlers.session_start({ reason: "startup" }, ctx);
const first = await handlers.before_agent_start({ prompt: "hi" }, ctx);
assert.equal(first.message.customType, "rgit");
assert.equal(first.message.display, false);
assert.match(first.message.content, /branch: main/);
assert.match(first.message.content, /--toon/);
assert.equal(await handlers.before_agent_start({ prompt: "again" }, ctx), undefined);

if (mode === "no-tools") {
  assert.deepEqual(Object.keys(tools), []);
} else {
  // The MCP tools, as native tools that run `rgit tool <name>` in the session's folder.
  assert.ok(Object.keys(tools).length > 50, Object.keys(tools).join(" "));
  assert.equal(tools.git_status.parameters.type, "object");
  assert.equal(tools.git_status.parameters.properties.repo.type, "string");
  const status = await tools.git_status.execute("t1", {}, undefined, undefined, ctx);
  assert.match(status.content[0].text, /branch: main/);
  const log = await tools.git_log.execute("t2", { limit: 1 }, undefined, undefined, ctx);
  assert.match(log.content[0].text, /init/);
  await assert.rejects(tools.git_show.execute("t3", { rev: "no-such-rev" }, undefined, undefined, ctx), /error/);
}

console.log(`ok ${mode}`);
