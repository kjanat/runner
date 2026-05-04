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
import type { PathLike } from "node:fs";
import { readFile, stat } from "node:fs/promises";
import { dirname, join, relative, resolve } from "node:path";
import { argv, exit, stderr } from "node:process";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

const here = dirname(fileURLToPath(import.meta.url));
const npmDir = resolve(here, "..");
const distDir = join(npmDir, "dist");

// Hard cap on every npm subprocess. Stops a stuck DNS/TCP/registry path
// from burning the whole CI job timeout (default 6h on GitHub Actions).
// 2 min is generous: our tarballs are ~1.5MB, so even on slow networks
// the upload portion is seconds. If this ever bites in practice (extreme
// flake on a real network), retry the job rather than raising the cap.
const NPM_TIMEOUT_MS = 120_000;

const { values } = parseArgs({
	args: argv.slice(2),
	options: {
		tag: { type: "string", default: "latest" },
		registry: { type: "string", default: "https://registry.npmjs.org/" },
		"dry-run": { type: "boolean", default: false },
		"no-provenance": { type: "boolean", default: false },
		access: { type: "string", default: "public" },
	},
	strict: true,
});

/** Narrows the `--access` CLI string to npm's accepted enum.
 * @throws If the value is anything other than `"public"` or `"restricted"`.
 */
function parseAccess(v: string): "public" | "restricted" {
	if (v === "public" || v === "restricted") return v;
	throw new Error(`--access must be "public" or "restricted", got "${v}"`);
}

/** Checks if the given path exists. Only treats `ENOENT` as "doesn't exist";
 * permission/IO errors propagate so the publish flow fails loud instead of
 * silently skipping a generated package because of a misconfigured runner.
 * @param p - The path to check.
 * @returns `true` if path exists, `false` if it's missing.
 * @throws Any non-`ENOENT` `stat()` error (`EACCES`, `EIO`, `ELOOP`, …).
 */
async function exists(p: PathLike): Promise<boolean> {
	try {
		await stat(p);
		return true;
	} catch (err) {
		if (err instanceof Error && "code" in err && err.code === "ENOENT") return false;
		throw err;
	}
}
/** Reads and parses the targets.json file from the npm directory.
 * @returns The parsed targets configuration.
 * @throws If the file cannot be read or parsed.
 */
async function readTargets(): Promise<{ scope: string; facade: string; targets: Array<{ pkg: string }> }> {
	return JSON.parse(await readFile(join(npmDir, "targets.json"), "utf8"));
}

/** Structural assertion that a `npm/dist/<pkg>` directory only contains
 * what `build-packages.ts` is supposed to emit. Refuses to publish if:
 *
 * - A per-package `.npmrc` is present (could redirect publish via registry config).
 * - `package.json` declares `publishConfig` (same threat — overrides we can't see).
 * - `package.json#name` doesn't match the expected scoped/facade name (catches
 *   build-time mistakes where a sub-package was scribbled into the wrong dir).
 *
 * The CLI flags `--registry` and `--access` already override `.npmrc` /
 * `publishConfig` for the actual publish call, but rejecting the unexpected
 * file structure outright is cheaper than reasoning about precedence — and
 * stops a tampered artifact from publishing under the wrong name in the
 * first place.
 *
 * @param pkgDir - Directory containing `package.json` to validate.
 * @param expectedName - Fully-qualified npm name we expect (e.g. `@runner-run/linux-x64-gnu`).
 */
async function validatePackageDir(pkgDir: string, expectedName: string): Promise<void> {
	if (await exists(join(pkgDir, ".npmrc"))) {
		throw new Error(`${pkgDir}/.npmrc is forbidden — could redirect publish via per-package registry config`);
	}
	const pkg = JSON.parse(await readFile(join(pkgDir, "package.json"), "utf8")) as {
		name?: unknown;
		publishConfig?: unknown;
	};
	if (pkg.publishConfig !== undefined) {
		throw new Error(`${pkgDir}/package.json has publishConfig — could redirect publish or change access`);
	}
	if (typeof pkg.name !== "string") {
		throw new Error(`${pkgDir}/package.json missing string \`name\` field`);
	}
	if (pkg.name !== expectedName) {
		throw new Error(`${pkgDir}/package.json declares '${pkg.name}', expected '${expectedName}'`);
	}
}

/** Checks if the package in the given directory has already been published to
 * the npm registry. Distinguishes "version genuinely not yet published"
 * (`E404`) from auth/network failures so the latter aren't silently treated
 * as "go ahead and publish" — which would either fail later with the same
 * error, or worse, succeed in a wrong direction if the env shifts mid-flow.
 *
 * @param pkgDir - The directory containing the package to check (must have package.json).
 * @param registry - Registry URL to query (must be passed explicitly so we don't inherit ambient config).
 * @returns `true` if `name@version` is already on the registry, `false` if it's a clean `E404`.
 * @throws On auth (`E401`, `ENEEDAUTH`), network (`ENOTFOUND`, `ETIMEDOUT`, `EAI_AGAIN`),
 * or any other unrecognized npm CLI failure — with the captured stderr in the message.
 */
async function alreadyPublished(pkgDir: string, registry: string): Promise<boolean> {
	const pkg: { name: string; version: string } = JSON.parse(await readFile(join(pkgDir, "package.json"), "utf8"));
	const { name, version } = pkg;
	const res = spawnSync("npm", ["view", `${name}@${version}`, "--registry", registry, "version"], {
		encoding: "utf8",
		timeout: NPM_TIMEOUT_MS,
	});
	if (res.signal) {
		throw new Error(`npm view ${name}@${version} timed out after ${NPM_TIMEOUT_MS}ms (killed with ${res.signal})`);
	}
	if (res.status === 0) return res.stdout.trim() === version;
	const errOut = (res.stderr || "").toString();
	// `npm view foo@missing-version` prints an `E404` line and exits non-zero.
	// That's the only non-zero we want to swallow — everything else (auth,
	// network, malformed registry response) bubbles up.
	if (/E404|404 Not Found/i.test(errOut)) return false;
	throw new Error(`npm view ${name}@${version} failed (exit ${res.status}): ${errOut.trim() || "no stderr"}`);
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
	registry: string;
	"dry-run": boolean;
	"no-provenance": boolean;
}) {
	// --registry: pin destination, don't inherit ambient .npmrc.
	// --ignore-scripts: refuse to run lifecycle hooks (prepare/postpublish)
	// from the package being published. Defense against a tampered or
	// future-edited package.json adding scripts that execute with the
	// publishing credentials in scope.
	const args = [
		"publish",
		"--registry",
		opts.registry,
		"--access",
		opts.access,
		"--tag",
		opts.tag,
		"--ignore-scripts",
	];
	if (opts["dry-run"]) args.push("--dry-run");
	if (!opts["no-provenance"] && process.env.GITHUB_ACTIONS === "true") {
		args.push("--provenance");
	}
	console.log(`+ npm ${args.join(" ")}  (cwd: ${relative(npmDir, pkgDir)})`);
	const res = spawnSync("npm", args, {
		cwd: pkgDir,
		stdio: ["inherit", "inherit", "pipe"],
		timeout: NPM_TIMEOUT_MS,
	});
	if (res.signal) {
		throw new Error(`npm publish for ${pkgDir} timed out after ${NPM_TIMEOUT_MS}ms (killed with ${res.signal})`);
	}
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

/** Entry point: publishes every per-platform sub-package found under
 * `npm/dist/`, then the facade last. Skips packages whose version is already
 * on the registry so partial reruns are idempotent. Throws if the facade
 * directory is missing entirely (means `build-packages.ts` never ran).
 */
async function main() {
	const opts = { ...values, access: parseAccess(values.access) };
	const matrix = await readTargets();

	for (const target of matrix.targets) {
		const dir = join(distDir, target.pkg);
		const expectedName = `${matrix.scope}/${target.pkg}`;
		if (!(await exists(dir))) {
			console.warn(`skipping ${expectedName}: not generated`);
			continue;
		}
		await validatePackageDir(dir, expectedName);
		if (!opts["dry-run"] && (await alreadyPublished(dir, opts.registry))) {
			console.log(`skipping ${expectedName}: version already on registry`);
			continue;
		}
		publish(dir, opts);
	}

	const facadeDir = join(distDir, matrix.facade);
	if (!(await exists(facadeDir))) {
		throw new Error(`façade not generated at ${facadeDir} — run build-packages.ts first`);
	}
	await validatePackageDir(facadeDir, matrix.facade);
	if (!opts["dry-run"] && (await alreadyPublished(facadeDir, opts.registry))) {
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
