#!/usr/bin/env node
'use strict';

const { spawnSync } = require('node:child_process');
const { resolveBinary } = require('#resolve');
process.arch
try {
	const result = spawnSync(resolveBinary('run'), process.argv.slice(2), {
		stdio: 'inherit',
		windowsHide: false,
	});
	if (result.error) throw result.error;
	process.exit(result.status ?? 1);
} catch (err) {
	process.stderr.write(`run: ${err.message}\n`);
	process.exit(1);
}
