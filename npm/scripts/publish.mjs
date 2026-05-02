#!/usr/bin/env node
/** Publishes every generated package under `npm/dist/` to the npm registry.
 *
 * Sub-packages publish first, façade last — so when users `npm install runner-run`,
 * the optionalDependencies are already resolvable.
 *
 * Usage:
 * ```sh
 * node npm/scripts/publish.mjs --tag latest          # tag from facade pkg.version
 * node npm/scripts/publish.mjs --tag next --dry-run
 * ```
 *
 * Reads `npm/targets.json` to determine ordering and skips packages that don't exist under `npm/dist/`
 * (allows building a partial matrix without failing the publish).
 */

import { spawnSync } from "node:child_process";
import { readFile, stat } from "node:fs/promises";
import { dirname, join, relative, resolve } from "node:path";
import { argv, exit, stderr } from "node:process";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

const here = dirname(fileURLToPath(import.meta.url));
const npmDir = resolve(here, "..");
const distDir = join(npmDir, "dist");

const { values } = parseArgs({
	args: argv.slice(2),
	options: {
		tag: { type: "string", default: "latest" },
		"dry-run": { type: "boolean", default: false },
		"no-provenance": { type: "boolean", default: false },
		access: { type: "string", default: "public" },
	},
	strict: true,
});

/** @param {string} v @returns {"public" | "restricted"} */
function parseAccess(v) {
	if (v === "public" || v === "restricted") return v;
	throw new Error(`--access must be "public" or "restricted", got "${v}"`);
}

/** @typedef {import('node:fs').PathLike} PathLike */

/** Checks if the given path exists and is accessible.
 * @param {PathLike} p - The path to check.
 * @returns {Promise<boolean>} Promise resolves to true if path exists.
 */
async function exists(p) {
	try {
		await stat(p);
		return true;
	} catch {
		return false;
	}
}
/** Reads and parses the targets.json file from the npm directory.
 * @returns {Promise<{scope: string, facade: string, targets: Array<{pkg: string}>}>} The parsed targets configuration.
 * @throws {Error} If the file cannot be read or parsed.
 */
async function readTargets() {
	return JSON.parse(await readFile(join(npmDir, "targets.json"), "utf8"));
}

/** Checks if the package in the given directory has already been published to the npm registry.
 * @param {string} pkgDir - The directory containing the package to check (must have package.json).
 * @returns {Promise<boolean>} A promise that resolves to true if the package version is already published, or false otherwise.
 * @throws {Error} If there is an error reading the package.json or executing the npm command.
 */
async function alreadyPublished(pkgDir) {
	/** @type {{name: string, version: string}} */
	const pkg = JSON.parse(await readFile(join(pkgDir, "package.json"), "utf8"));
	const { name, version } = pkg;
	const res = spawnSync("npm", ["view", `${name}@${version}`, "version"], {
		encoding: "utf8",
	});
	return res.status === 0 && res.stdout.trim() === version;
}

/** Publishes the package at `pkgDir` to the npm registry, with the given options.
 * Throws if the publish fails for any reason other than "version already published".
 *
 * @param {string} pkgDir - The directory containing the package to publish (must have package.json).
 * @param {{
 *   access: "restricted" | "public",
 *   tag: string,
 *   "dry-run": boolean,
 *   "no-provenance": boolean,
 * }} opts - Publish options.
 */
function publish(pkgDir, opts) {
	const args = ["publish", "--access", opts["access"], "--tag", opts["tag"]];
	if (opts["dry-run"]) args.push("--dry-run");
	if (!opts["no-provenance"] && process.env.GITHUB_ACTIONS === "true") {
		args.push("--provenance");
	}
	console.log(`+ npm ${args.join(" ")}  (cwd: ${relative(npmDir, pkgDir)})`);
	const res = spawnSync("npm", args, { cwd: pkgDir, stdio: ["inherit", "inherit", "pipe"] });
	if (res.status === 0) return;
	const stderrR = (res.stderr || "").toString();
	stderr.write(stderrR);
	// Treat "version already published" as a no-op so partial reruns succeed.
	if (/EPUBLISHCONFLICT|cannot publish over the previously published versions/i.test(stderrR)) {
		console.log(`  -> already published, skipping`);
		return;
	}
	throw new Error(`npm publish failed for ${pkgDir} (exit ${res.status})`);
}

async function main() {
	const opts = { ...values, access: parseAccess(values.access) };
	const matrix = await readTargets();

	for (const target of matrix.targets) {
		const dir = join(distDir, target.pkg);
		if (!(await exists(dir))) {
			console.warn(`skipping ${matrix.scope}/${target.pkg}: not generated`);
			continue;
		}
		if (!opts["dry-run"] && (await alreadyPublished(dir))) {
			console.log(`skipping ${matrix.scope}/${target.pkg}: version already on registry`);
			continue;
		}
		publish(dir, opts);
	}

	const facadeDir = join(distDir, matrix.facade);
	if (!(await exists(facadeDir))) {
		throw new Error(`façade not generated at ${facadeDir} — run build-packages.mjs first`);
	}
	if (!opts["dry-run"] && (await alreadyPublished(facadeDir))) {
		console.log(`skipping ${matrix.facade}: version already on registry`);
	} else {
		publish(facadeDir, opts);
	}
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
	main().catch((err) => {
		stderr.write(`publish: ${err.stack || err.message}\n`);
		exit(1);
	});
}
