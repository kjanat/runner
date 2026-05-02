#!/usr/bin/env node
const { argv, exit, stderr } = require("node:process");
const { spawnSync } = require("node:child_process");
const { resolveBinary } = require("#resolve");

try {
	const result = spawnSync(resolveBinary("run"), argv.slice(2), {
		stdio: "inherit",
		windowsHide: false,
	});
	if (result.error) throw result.error;
	exit(result.status ?? 1);
} catch (err) {
	stderr.write(`run: ${err instanceof Error ? err.message : String(err)}\n}\n`);
	exit(1);
}
