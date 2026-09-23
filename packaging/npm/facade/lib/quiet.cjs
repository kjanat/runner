"use strict";

/** @param {string | undefined} raw */
const namedLevel = (raw) => {
	const value = raw?.trim().toLowerCase();
	if (value === undefined || value === "") return 0;
	if (/^\d+$/.test(value)) return Math.min(Number(value), 4);
	if (["1", "true", "yes", "on", "enabled"].includes(value)) return 1;
	return 0;
};

/** @param {string} name @param {string[]} args */
const quietCountFromArgs = (name, args) => {
	let count = 0;
	let sawQuiet = false;
	let index = 0;
	const valueFlags = new Set([
		"--dir",
		"--pm",
		"--runner",
		"--runtime",
		"--fallback",
		"--on-mismatch",
		"--host-stream",
		"--schema-version",
	]);
	const builtins = new Set([
		"clean",
		"install",
		"i",
		"list",
		"ls",
		"info",
		"why",
		"doctor",
		"config",
		"completions",
		"schema",
		"man",
		"lsp",
	]);
	let command = name === "run" ? "run" : null;
	while (index < args.length) {
		const arg = args[index++];
		if (arg === undefined) break;
		if (arg === "--") break;
		if (arg === "--quiet") {
			count++;
			sawQuiet = true;
			continue;
		}
		if (/^-q+$/.test(arg)) {
			count += arg.length - 1;
			sawQuiet = true;
			continue;
		}
		if (valueFlags.has(arg)) {
			index++;
			continue;
		}
		if ([...valueFlags].some((flag) => arg.startsWith(`${flag}=`))) continue;
		if (!arg.startsWith("-")) {
			if (command === "run") break;
			if (command !== null) continue;
			if (["run", "r"].includes(arg)) command = "run";
			else if (builtins.has(arg)) command = arg;
			else break;
		}
	}
	return sawQuiet ? Math.min(count, 4) : null;
};

/** @param {string} name @param {string[]} args */
const effectiveLevel = (name, args) => quietCountFromArgs(name, args) ?? namedLevel(process.env.RUNNER_QUIET);

/** @param {string} name @param {string[]} args */
const fatalOutputEnabled = (name, args) => effectiveLevel(name, args) < 4;

/** @param {string} name @param {string[]} args */
const warningOutputEnabled = (name, args) => effectiveLevel(name, args) < 2;

module.exports = { fatalOutputEnabled, warningOutputEnabled };
