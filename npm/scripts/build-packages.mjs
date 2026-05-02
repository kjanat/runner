#!/usr/bin/env node
// Generates `npm/dist/<pkg>/` trees for the façade and every per-platform
// sub-package listed in `npm/targets.json`. Tarballs containing the native
// binaries are read from `npm/downloads/` (populated by `gh release download`
// in CI, or manually for local runs).
//
// Usage:
//   node npm/scripts/build-packages.mjs --version 0.5.0
//   node npm/scripts/build-packages.mjs --version 0.0.0-dev --only=linux-x64-gnu

import { createReadStream } from 'node:fs';
import { cp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { createGunzip } from 'node:zlib';

const here = path.dirname(fileURLToPath(import.meta.url));
const npmDir = path.resolve(here, '..');
const repoDir = path.resolve(npmDir, '..');

function readOpts() {
  const { values } = parseArgs({
    args: process.argv.slice(2),
    options: {
      version: { type: 'string' },
      only: { type: 'string' },
      'skip-missing': { type: 'boolean', default: false },
      downloads: { type: 'string' },
    },
    strict: true,
  });
  if (!values.version) throw new Error('missing --version');
  return {
    version: values.version,
    only: values.only ? values.only.split(',') : null,
    skipMissing: values['skip-missing'],
    downloads: values.downloads,
  };
}

async function readTargets() {
  const raw = await readFile(path.join(npmDir, 'targets.json'), 'utf8');
  return JSON.parse(raw);
}

async function clean() {
  await rm(path.join(npmDir, 'dist'), { recursive: true, force: true });
  await mkdir(path.join(npmDir, 'dist'), { recursive: true });
}

async function buildFacade(matrix, version, builtTargets) {
  const tplPath = path.join(npmDir, 'facade', 'package.json');
  const tpl = JSON.parse(await readFile(tplPath, 'utf8'));
  tpl.version = version;
  tpl.optionalDependencies = Object.fromEntries(
    builtTargets.map((t) => [`${matrix.scope}/${t.pkg}`, version])
  );
  const dest = path.join(npmDir, 'dist', matrix.facade);
  await mkdir(path.join(dest, 'bin'), { recursive: true });
  await mkdir(path.join(dest, 'lib'), { recursive: true });
  await writeFile(path.join(dest, 'package.json'), `${JSON.stringify(tpl, null, 2)}\n`);
  await cp(path.join(npmDir, 'facade', 'README.md'), path.join(dest, 'README.md'));
  await cp(path.join(repoDir, 'LICENSE'), path.join(dest, 'LICENSE'));
  for (const f of ['runner.js', 'run.js']) {
    await cp(path.join(npmDir, 'facade', 'bin', f), path.join(dest, 'bin', f));
  }
  await cp(path.join(npmDir, 'facade', 'lib', 'resolve.js'), path.join(dest, 'lib', 'resolve.js'));
  console.log(`built ${matrix.facade}@${version}`);
}

// Streamed tar.gz extractor that pulls just the bin paths we need.
// Avoids pulling in the `tar` npm dep — Node has zlib, and tar is a simple
// fixed-block format.
async function extractBinariesFromTarball(tarballPath, names) {
  const stream = createReadStream(tarballPath).pipe(createGunzip());
  const chunks = [];
  for await (const chunk of stream) chunks.push(chunk);
  const buf = Buffer.concat(chunks);

  const wanted = new Set(names);
  const wantedExe = new Set(names.map((n) => `${n}.exe`));
  const found = {};

  let off = 0;
  while (off + 512 <= buf.length) {
    const header = buf.subarray(off, off + 512);
    if (header[0] === 0) break; // end-of-archive zero block
    const rawName = header.subarray(0, 100);
    const nameEnd = rawName.indexOf(0);
    const name = rawName.subarray(0, nameEnd === -1 ? 100 : nameEnd).toString('utf8');
    const sizeStr = header.subarray(124, 136).toString('utf8').trim().replace(/\0/g, '');
    const size = parseInt(sizeStr, 8) || 0;
    const typeflag = String.fromCharCode(header[156] || 0x30);
    off += 512;
    if (typeflag === '0' || typeflag === '\0') {
      const base = path.basename(name);
      if (wanted.has(base) || wantedExe.has(base)) {
        found[base] = buf.subarray(off, off + size);
      }
    }
    off += Math.ceil(size / 512) * 512;
  }
  return found;
}

async function buildPlatformPackage(matrix, target, version, opts) {
  const pkgName = `${matrix.scope}/${target.pkg}`;
  const dest = path.join(npmDir, 'dist', target.pkg);
  await mkdir(path.join(dest, 'bin'), { recursive: true });

  const downloadsDir = opts.downloads
    ? path.resolve(opts.downloads)
    : path.join(npmDir, 'downloads');
  const tag = version.startsWith('v') ? version : `v${version}`;
  const tarball = path.join(downloadsDir, `runner-${tag}-${target.rust}.tar.gz`);

  let binaries;
  try {
    binaries = await extractBinariesFromTarball(tarball, matrix.binaries);
  } catch (err) {
    if (opts.skipMissing) {
      console.warn(`skipping ${pkgName}: ${err.code || err.message}`);
      await rm(dest, { recursive: true, force: true });
      return null;
    }
    throw new Error(`failed to read ${tarball}: ${err.message}`);
  }

  const isWin = target.os.includes('win32');
  for (const name of matrix.binaries) {
    const file = isWin ? `${name}.exe` : name;
    const data = binaries[file];
    if (!data) {
      if (opts.skipMissing) {
        console.warn(`skipping ${pkgName}: missing ${file} in archive`);
        await rm(dest, { recursive: true, force: true });
        return null;
      }
      throw new Error(`missing ${file} in ${tarball}`);
    }
    await writeFile(path.join(dest, 'bin', file), data, { mode: 0o755 });
  }

  const pkg = {
    name: pkgName,
    version,
    description: `${target.pkg} prebuilt binaries for runner-run`,
    license: 'MIT',
    author: 'Kaj Kowalski <info+runner@kajkowalski.nl>',
    homepage: 'https://github.com/kjanat/runner#readme',
    repository: { type: 'git', url: 'git+https://github.com/kjanat/runner.git' },
    bugs: { url: 'https://github.com/kjanat/runner/issues' },
    os: target.os,
    cpu: target.cpu,
    ...(target.libc ? { libc: target.libc } : {}),
    engines: { node: '>=18' },
    files: ['bin/'],
  };
  await writeFile(path.join(dest, 'package.json'), `${JSON.stringify(pkg, null, 2)}\n`);
  await cp(path.join(repoDir, 'LICENSE'), path.join(dest, 'LICENSE'));

  const readme =
    `# ${pkgName}\n\n` +
    `Prebuilt \`runner\` and \`run\` binaries for \`${target.pkg}\` ` +
    `(rustc target: \`${target.rust}\`).\n\n` +
    `This package is an internal implementation detail of [\`runner-run\`](https://www.npmjs.com/package/runner-run). ` +
    `Don't depend on it directly — install \`runner-run\` and let npm pick the right sub-package for your platform.\n`;
  await writeFile(path.join(dest, 'README.md'), readme);

  console.log(`built ${pkgName}@${version}`);
  return pkgName;
}

async function main() {
  const opts = readOpts();
  const matrix = await readTargets();
  await clean();
  const filter = opts.only ? new Set(opts.only) : null;
  const built = [];
  for (const target of matrix.targets) {
    if (filter && !filter.has(target.pkg)) continue;
    const result = await buildPlatformPackage(matrix, target, opts.version, opts);
    if (result) built.push(target);
  }
  if (built.length === 0) {
    throw new Error('no platform packages were built — refusing to publish a façade with empty optionalDependencies');
  }
  await buildFacade(matrix, opts.version, built);
}

if (import.meta.main) {
  main().catch((err) => {
    process.stderr.write(`build-packages: ${err.stack || err.message}\n`);
    process.exit(1);
  });
}
