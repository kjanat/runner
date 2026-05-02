'use strict';

const path = require('node:path');
const facadePkg = require('runner-run/package.json');

const subPackages = Object.keys(facadePkg.optionalDependencies || {});

function resolveBinary(name) {
  const exe = process.platform === 'win32' ? `${name}.exe` : name;
  const errors = [];
  for (const subPkg of subPackages) {
    let pkgJsonPath;
    try {
      pkgJsonPath = require.resolve(`${subPkg}/package.json`);
    } catch (err) {
      errors.push(`${subPkg}: ${err.code || err.message}`);
      continue;
    }
    return path.join(path.dirname(pkgJsonPath), 'bin', exe);
  }
  const detail = errors.length ? `\nTried:\n  - ${errors.join('\n  - ')}` : '';
  throw new Error(
    `runner-run: no prebuilt binary found for ${process.platform}-${process.arch}.\n` +
      `This usually means your package manager skipped optionalDependencies ` +
      `(common with --no-optional, --omit=optional, or some Docker/CI setups).\n` +
      `Workarounds:\n` +
      `  - reinstall without --no-optional / --omit=optional\n` +
      `  - install from source: cargo install --git=https://github.com/kjanat/runner/ runner\n` +
      `  - file an issue if your platform is unsupported: https://github.com/kjanat/runner/issues${detail}`
  );
}

module.exports = { resolveBinary };
