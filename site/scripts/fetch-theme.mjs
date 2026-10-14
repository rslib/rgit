// Fetch the rslib Starlight theme from rslib/misc at a pinned commit into
// site/.theme, so a local build and CI use the same theme. The theme is not
// on npm yet; site/package.json installs it from .theme/starlight.
// Usage: node site/scripts/fetch-theme.mjs
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, rmSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO = 'https://github.com/rslib/misc.git';
// Move this pin to take a newer theme.
const COMMIT = '7b56cad27acf2274ca0c0f0be78e5fab15564de3';

const site = join(dirname(fileURLToPath(import.meta.url)), '..');
const dest = join(site, '.theme');

function git(...args) {
  const result = spawnSync('git', ['-C', dest, ...args], { encoding: 'utf8' });
  if (result.status !== 0) {
    console.error(`git ${args.join(' ')} failed\n${result.stderr}`);
    process.exit(1);
  }
  return result.stdout.trim();
}

const current = existsSync(join(dest, '.git'))
  ? spawnSync('git', ['-C', dest, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).stdout.trim()
  : '';
if (current === COMMIT) {
  console.log(`theme already at ${COMMIT.slice(0, 7)}`);
  process.exit(0);
}

rmSync(dest, { recursive: true, force: true });
mkdirSync(dest, { recursive: true });
git('init', '-q');
git('remote', 'add', 'origin', REPO);
git('fetch', '-q', '--depth', '1', 'origin', COMMIT);
git('checkout', '-q', 'FETCH_HEAD');
console.log(`theme fetched at ${COMMIT.slice(0, 7)}`);
