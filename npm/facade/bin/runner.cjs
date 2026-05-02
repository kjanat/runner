#!/usr/bin/env node
'use strict';

const { spawnSync } = require('node:child_process');
const { resolveBinary } = require('#resolve');

try {
  const result = spawnSync(resolveBinary('runner'), process.argv.slice(2), {
    stdio: 'inherit',
    windowsHide: false,
  });
  if (result.error) throw result.error;
  process.exit(result.status ?? 1);
} catch (err) {
  process.stderr.write(`runner: ${err.message}\n`);
  process.exit(1);
}
