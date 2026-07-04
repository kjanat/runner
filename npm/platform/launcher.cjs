#!/usr/bin/env node
/**
 * Standalone launcher shipped inside every `@runner-run/*` platform
 * package, copied to `runner.cjs` and `run.cjs` at build time; each
 * copy spawns the package-relative binary matching its own filename.
 * Same spawn semantics as the facade's launcher.
 */
"use strict";

const process = require("node:process");
const { argv, exit, stderr } = process;
const { spawnSync } = require("node:child_process");
const { basename, join } = require("node:path");

const name = basename(__filename, ".cjs");
const exe = process.platform === "win32" ? `${name}.exe` : name;

try {
	const result = spawnSync(join(__dirname, "bin", exe), argv.slice(2), {
		stdio: "inherit",
		windowsHide: false,
	});
	if (result.error) throw result.error;
	// Child died from a signal (SIGINT, SIGTERM, …). Re-raise it on
	// ourselves so the parent shell sees `WIFSIGNALED` / exit code
	// 128 + N instead of a generic 1.
	if (result.signal) {
		process.removeAllListeners(result.signal);
		process.kill(process.pid, result.signal);
	} else {
		exit(result.status ?? 1);
	}
} catch (err) {
	stderr.write(`${name}: ${err instanceof Error ? err.message : String(err)}\n`);
	exit(1);
}
