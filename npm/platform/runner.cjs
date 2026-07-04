#!/usr/bin/env node
/**
 * Standalone launcher shipped inside every `@runner-run/*` platform
 * package. Resolves the prebuilt binary package-relative (`bin/runner`
 * next to this file), so the platform package works on its own:
 *
 *   npx @runner-run/<platform> install -f task1 task2
 *   npx --package=@runner-run/<platform> runner list
 *
 * The facade (`runner-run`) remains the right dependency for anything
 * portable — this exists so a pinned platform package is still a
 * functional CLI rather than dead weight. Same spawn semantics as the
 * facade's launcher: inherited stdio, exit-code passthrough, and
 * signal re-raise so `set -e` / Ctrl+C chaining behave.
 */
"use strict";

const process = require("node:process");
const { argv, exit, stderr } = process;
const { spawnSync } = require("node:child_process");
const { join } = require("node:path");

const exe = process.platform === "win32" ? "runner.exe" : "runner";

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
	stderr.write(`runner: ${err instanceof Error ? err.message : String(err)}\n`);
	exit(1);
}
