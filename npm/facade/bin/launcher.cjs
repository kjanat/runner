#!/usr/bin/env node
const { argv, exit, stderr } = require("node:process");
const { spawnSync } = require("node:child_process");
const { basename, extname } = require("node:path");
const { resolveBinary } = require("#resolve");

const name = basename(__filename, extname(__filename));

try {
	const result = spawnSync(resolveBinary(name), argv.slice(2), {
		stdio: "inherit",
		windowsHide: false,
	});
	if (result.error) throw result.error;
	exit(result.status ?? 1);
} catch (err) {
	stderr.write(`${name}: ${err instanceof Error ? err.message : String(err)}\n`);
	exit(1);
}
