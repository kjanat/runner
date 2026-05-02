#!/usr/bin/env node
const { argv, exit, stderr } = require("node:process");
const { spawnSync } = require("node:child_process");
const { resolveBinary } = require("#resolve");

try {
	const result = spawnSync(resolveBinary("runner"), argv.slice(2), {
		stdio: "inherit",
		windowsHide: false,
	});
	if (result.error) throw result.error;
	exit(result.status ?? 1);
} catch (err) {
	stderr.write(`runner: ${err instanceof Error ? err.message : String(err)}\n`);
	exit(1);
}
