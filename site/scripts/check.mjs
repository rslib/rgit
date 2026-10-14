// Usage: node scripts/check.mjs <guides|reference|workflow|theme>
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const site = join(dirname(fileURLToPath(import.meta.url)), '..');
const root = join(site, '..');
const dist = join(site, 'dist');

function ok(message) {
  console.log(`ok: ${message}`);
}

function fail(message) {
  console.error(`FAIL: ${message}`);
  process.exit(1);
}

function assert(condition, message) {
  if (!condition) fail(message);
  ok(message);
}

function build(args = [], env = {}) {
  return spawnSync('npm', ['run', 'build', '--silent', ...(args.length ? ['--', ...args] : [])], {
    cwd: site,
    env: { ...process.env, ...env },
    encoding: 'utf8',
  });
}

function buildOrFail(args, env) {
  const result = build(args, env);
  if (result.status !== 0) fail(`site build failed\n${result.stdout}\n${result.stderr}`);
  return result;
}

function page(outDir, route) {
  const file = join(outDir, route, 'index.html');
  if (!existsSync(file)) fail(`missing built page ${file}`);
  return readFileSync(file, 'utf8');
}

function titleOf(html) {
  return (html.match(/<title>([\s\S]*?)<\/title>/) || [])[1] ?? '';
}

function firstHeading(file) {
  const line = readFileSync(file, 'utf8').split('\n').find((l) => l.startsWith('# '));
  return line.slice(2).trim();
}

function escapeHtml(text) {
  return text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

function guides() {
  buildOrFail([]);
  const docs = join(root, 'docs');
  for (const name of readdirSync(docs).filter((f) => f.endsWith('.md'))) {
    const slug = name.replace(/\.md$/, '');
    const title = firstHeading(join(docs, name));
    const html = page(dist, `guides/${slug}`);
    assert(titleOf(html).includes(escapeHtml(title)), `guides/${slug} is titled "${title}"`);
  }
  const changeId = page(dist, 'guides/change-id');
  assert(changeId.includes('How to add Change-Id trailers'), 'change-id page has the title How to add Change-Id trailers');
  assert(
    changeId.includes('rgit adds no trailers to commit messages by default.'),
    'change-id page has the guide text',
  );
  const home = page(dist, '');
  assert(home.includes('cargo install'), 'start page has the install text from the README');
  assert(home.includes('rgit --toon status'), 'start page has the quickstart from the README');

  const work = mkdtempSync(join(tmpdir(), 'rgit-site-'));
  try {
    const extra = join(work, 'docs');
    mkdirSync(extra);

    writeFileSync(join(extra, 'new-guide.md'), '# How to try a new guide\n\nA new guide needs no site change.\n');
    const out = join(work, 'out-new');
    buildOrFail(['--outDir', out], { RGIT_SITE_EXTRA_DOCS: extra });
    const newPage = page(out, 'guides/new-guide');
    assert(titleOf(newPage).includes('How to try a new guide'), 'new guide page has the title How to try a new guide');
    assert(
      page(out, '').includes('/rgit/guides/new-guide/'),
      'sidebar lists the new guide',
    );

    writeFileSync(join(extra, 'new-guide.md'), '# Broken\n\nSee [the missing guide](missing.md).\n');
    const bad = build(['--outDir', join(work, 'out-bad')], { RGIT_SITE_EXTRA_DOCS: extra });
    assert(bad.status !== 0, 'build exits non-zero on a link to missing.md');
    assert(/missing/.test(bad.stdout + bad.stderr), 'build output names missing');
  } finally {
    rmSync(work, { recursive: true, force: true });
    spawnSync('node', ['scripts/sync.mjs'], { cwd: site });
  }
}

function reference() {
  buildOrFail([]);
  const commands = page(dist, 'reference/commands');
  for (const name of ['status', 'stash', 'agent']) {
    assert(
      new RegExp(`<a href="/rgit/reference/commands/${name}/"[^>]*><code[^>]*>${name}</code></a>`).test(commands),
      `commands overview links ${name}`,
    );
  }
  const status = page(dist, 'reference/commands/status');
  for (const heading of ['Usage', 'Examples', 'Options']) {
    assert(new RegExp(`<h2 id="${heading.toLowerCase()}"`).test(status), `status page has ${heading}`);
  }
  assert(/<table[\s\S]*<code[^>]*>--porcelain<\/code>/.test(status), 'status options table names --porcelain');
  const stash = page(dist, 'reference/commands/stash');
  assert(/<h3 id="rgit-stash-pop"/.test(stash), 'stash page has a rgit stash pop section');
  const keys = page(dist, 'reference/tui-keys');
  assert(/<table[\s\S]*stage \/ unstage at point/.test(keys), 'tui-keys page has a table with stage / unstage at point');
  const config = page(dist, 'reference/configuration');
  assert(/<pre[^>]*data-language="toml"/.test(config), 'configuration page has a toml code block');
  assert(/<pre[^>]*data-language="toml"[\s\S]*\[theme\]/.test(config), 'configuration page shows [theme]');
}

function workflow() {
  const file = join(root, '.github/workflows/docs.yml');
  assert(existsSync(file), '.github/workflows/docs.yml exists');
  const text = readFileSync(file, 'utf8');
  const has = (re, message) => assert(re.test(text), message);
  has(/^on:\n  push:\n    branches: \[main\]/m, 'triggers on push to main');
  for (const path of ['docs/**', 'site/**', 'README.md', 'skills/rgit/**']) {
    has(new RegExp(`^      - "${path.replace(/[*.]/g, '\\$&')}"$`, 'm'), `push paths include ${path}`);
  }
  has(/uses: actions\/configure-pages@/, 'uses actions/configure-pages');
  has(/npm --prefix site run build|uses: withastro\/action@/, 'builds site/');
  has(/uses: actions\/upload-pages-artifact@/, 'uses actions/upload-pages-artifact');
  has(/uses: actions\/deploy-pages@/, 'uses actions/deploy-pages');
  has(/^  pages: write$/m, 'permissions pages: write');
  has(/^  id-token: write$/m, 'permissions id-token: write');
  const config = readFileSync(join(site, 'astro.config.mjs'), 'utf8');
  assert(/base: '\/rgit'/.test(config), 'site base is /rgit');
  const readme = readFileSync(join(root, 'README.md'), 'utf8');
  assert(readme.includes('https://rslib.github.io/rgit'), 'README links the site');
}

function cssOf(html) {
  return [...html.matchAll(/<link rel="stylesheet" href="\/rgit\/([^"]+\.css)"/g)]
    .map((m) => readFileSync(join(dist, m[1]), 'utf8'))
    .join('\n');
}

function theme() {
  buildOrFail([]);
  const index = page(dist, '');
  assert(/class="rs-title-org[^"]*"[^>]*>rslib</.test(index), 'the site title shows rslib/');
  assert(/class="rs-title-project[^"]*"[^>]*>rgit</.test(index), 'the site title shows rgit after rslib/');
  const css = cssOf(index);
  assert(css.includes('--rs-code-bg'), 'pages load the theme tokens');
  assert(css.includes('Bricolage Grotesque'), 'pages load the theme fonts');
  const guide = page(dist, 'guides/change-id');
  assert(/<pre[^>]*data-language="sh"/.test(guide), 'code blocks render');
  // #e0def4 is the rose-pine-moon text color; the theme draws its own code background.
  assert(/#e0def4/i.test(cssOf(guide) + guide), 'code blocks use the rose-pine colors');
  const workflowText = readFileSync(join(root, '.github/workflows/docs.yml'), 'utf8');
  const fetchAt = workflowText.indexOf('run: node site/scripts/fetch-theme.mjs');
  const installAt = workflowText.indexOf('run: npm --prefix site ci');
  assert(fetchAt !== -1 && fetchAt < installAt, 'CI fetches the theme before npm ci');
}

const parts = { guides, reference, workflow, theme };
const part = process.argv[2];
if (!parts[part]) fail(`unknown part "${part}"; use guides, reference, workflow or theme`);
parts[part]();
console.log(`${part} checks passed`);
