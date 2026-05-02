#!/usr/bin/env node
/** Publishes every generated package under `npm/dist/` to the npm registry.
 *
 * Sub-packages publish first, façade last — so when users `npm install runner-run`,
 * the optionalDependencies are already resolvable.
 *
 * Usage:
 * ```sh
 * node npm/scripts/publish.ts --tag latest          # tag from facade pkg.version
 * node npm/scripts/publish.ts --tag next --dry-run
 * ```
 *
 * Reads `npm/targets.json` to determine ordering and skips packages that don't exist under `npm/dist/`
 * (allows building a partial matrix without failing the publish).
 */

import { spawnSync } from "node:child_process";
import { PathLike } from "node:fs";
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

function parseAccess(v: string): "public" | "restricted" {
	if (v === "public" || v === "restricted") return v;
	throw new Error(`--access must be "public" or "restricted", got "${v}"`);
}

/** Checks if the given path exists and is accessible.
 * @param p - The path to check.
 * @returns Promise resolves to true if path exists.
 */
async function exists(p: PathLike): Promise<boolean> {
	try {
		await stat(p);
		return true;
	} catch {
		return false;
	}
}
/** Reads and parses the targets.json file from the npm directory.
 * @returns The parsed targets configuration.
 * @throws If the file cannot be read or parsed.
 */
async function readTargets(): Promise<{ scope: string; facade: string; targets: Array<{ pkg: string }> }> {
	return JSON.parse(await readFile(join(npmDir, "targets.json"), "utf8"));
}

/** Checks if the package in the given directory has already been published to the npm registry.
 * @param pkgDir - The directory containing the package to check (must have package.json).
 * @returns A promise that resolves to true if the package version is already published, or false otherwise.
 * @throws If there is an error reading the package.json or executing the npm command.
 */
async function alreadyPublished(pkgDir: string): Promise<boolean> {
	/** @type {{name: string, version: string}} */
	const pkg: { name: string; version: string } = JSON.parse(await readFile(join(pkgDir, "package.json"), "utf8"));
	const { name, version } = pkg;
	const res = spawnSync("npm", ["view", `${name}@${version}`, "version"], {
		encoding: "utf8",
	});
	return res.status === 0 && res.stdout.trim() === version;
}

/** Publishes the package at `pkgDir` to the npm registry, with the given options.
 * Throws if the publish fails for any reason other than "version already published".
 *
 * @param pkgDir - The directory containing the package to publish (must have package.json).
 * @param opts - Publish options.
 */
function publish(pkgDir: string, opts: {
	access: "restricted" | "public";
	tag: string;
	"dry-run": boolean;
	"no-provenance": boolean;
}) {
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
		throw new Error(`façade not generated at ${facadeDir} — run build-packages.ts first`);
	}
	if (!opts["dry-run"] && (await alreadyPublished(facadeDir))) {
		console.log(`skipping ${matrix.facade}: version already on registry`);
	} else {
		publish(facadeDir, opts);
	}
}

if (import.meta.main) {
	main().catch((err) => {
		stderr.write(`publish: ${err.stack || err.message}\n`);
		exit(1);
	});
}
