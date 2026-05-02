#!/usr/bin/env node
// Publishes every generated package under `npm/dist/` to the npm registry.
// Sub-packages publish first, façade last — so when users `npm install
// runner-run`, the optionalDependencies are already resolvable.
//
// Usage:
//   node npm/scripts/publish.mjs --tag latest          # tag from facade pkg.version
//   node npm/scripts/publish.mjs --tag next --dry-run
//
// Reads npm/targets.json to determine ordering and skips packages that
// don't exist under npm/dist/ (allows building a partial matrix without
// failing the publish).

import { spawnSync } from 'node:child_process';
import { readFile, stat } from 'node:fs/promises';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const npmDir = path.resolve(here, '..');
const distDir = path.join(npmDir, 'dist');

function parseArgs(argv) {
  const out = { tag: 'latest', dryRun: false, access: 'public' };
  for (let i = 2; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--tag') out.tag = argv[++i];
    else if (a.startsWith('--tag=')) out.tag = a.slice('--tag='.length);
    else if (a === '--dry-run') out.dryRun = true;
    else if (a === '--no-provenance') out.noProvenance = true;
    else if (a === '--access') out.access = argv[++i];
    else if (a.startsWith('--access=')) out.access = a.slice('--access='.length);
    else throw new Error(`unknown arg: ${a}`);
  }
  return out;
}

async function exists(p) {
  try {
    await stat(p);
    return true;
  } catch {
    return false;
  }
}

async function readTargets() {
  return JSON.parse(await readFile(path.join(npmDir, 'targets.json'), 'utf8'));
}

function publish(pkgDir, opts) {
  const args = ['publish', '--access', opts.access, '--tag', opts.tag];
  if (opts.dryRun) args.push('--dry-run');
  if (!opts.noProvenance && process.env.GITHUB_ACTIONS === 'true') {
    args.push('--provenance');
  }
  console.log(`+ npm ${args.join(' ')}  (cwd: ${path.relative(npmDir, pkgDir)})`);
  const res = spawnSync('npm', args, { cwd: pkgDir, stdio: 'inherit' });
  if (res.status !== 0) {
    throw new Error(`npm publish failed for ${pkgDir} (exit ${res.status})`);
  }
}

async function main() {
  const opts = parseArgs(process.argv);
  const matrix = await readTargets();

  for (const target of matrix.targets) {
    const dir = path.join(distDir, target.pkg);
    if (!(await exists(dir))) {
      console.warn(`skipping ${matrix.scope}/${target.pkg}: not generated`);
      continue;
    }
    publish(dir, opts);
  }

  const facadeDir = path.join(distDir, matrix.facade);
  if (!(await exists(facadeDir))) {
    throw new Error(`façade not generated at ${facadeDir} — run build-packages.mjs first`);
  }
  publish(facadeDir, opts);
}

main().catch((err) => {
  process.stderr.write(`publish: ${err.stack || err.message}\n`);
  process.exit(1);
});
