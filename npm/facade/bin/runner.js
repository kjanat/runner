#!/usr/bin/env node
'use strict';

const { spawnSync } = require('node:child_process');
const { resolveBinary } = require('../lib/resolve.js');

const result = spawnSync(resolveBinary('runner'), process.argv.slice(2), {
  stdio: 'inherit',
  windowsHide: false,
});

if (result.error) {
  process.stderr.write(`runner: ${result.error.message}\n`);
  process.exit(1);
}
process.exit(result.status ?? 1);
