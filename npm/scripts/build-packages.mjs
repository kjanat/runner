#!/usr/bin/env node
/** Generates `npm/dist/<pkg>/` trees for the façade and every per-platform
 * sub-package listed in `npm/targets.json`. Tarballs containing the native
 * binaries are read from `npm/downloads/` (populated by `gh release download`
 * in CI, or manually for local runs).
 *
 * Usage:
 * ```sh
 * node npm/scripts/build-packages.mjs --version 0.5.0
 * node npm/scripts/build-packages.mjs --version 0.0.0-dev --only=linux-x64-gnu
 * ```
 */
import { createReadStream } from "node:fs";
import { cp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { basename, dirname, join, resolve } from "node:path";
import { argv, exit, stderr } from "node:process";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { createGunzip } from "node:zlib";

const here = dirname(fileURLToPath(import.meta.url));
const npmDir = resolve(here, "..");
const repoDir = resolve(npmDir, "..");

/**
 * @typedef {Object} Target
 * @property {string} pkg
 * @property {string} rust
 * @property {NodeJS.Platform[]} os
 * @property {NodeJS.Architecture[]} cpu
 * @property {("glibc" | "musl")[]} [libc]
 * @property {string} runner
 * @property {"cargo" | "cross"} build
 * @property {1 | 2 | 3} tier
 * @property {boolean} [experimental]
 */

/**
 * @typedef {Object} Matrix
 * @property {string} facade
 * @property {string} scope
 * @property {string[]} binaries
 * @property {Target[]} targets
 */

/**
 * @typedef {Object} BuildOpts
 * @property {string} version
 * @property {string[] | null} only
 * @property {boolean} skipMissing
 * @property {string | undefined} downloads
 */

/** @param {unknown} err @returns {string} */
function errMessage(err) {
	return err instanceof Error ? err.message : String(err);
}

/** @param {unknown} err @returns {string | undefined} */
function errCode(err) {
	if (err instanceof Error && "code" in err && typeof err.code === "string") return err.code;
	return undefined;
}

/** @returns {BuildOpts} */
function readOpts() {
	const { values } = parseArgs({
		args: process.argv.slice(2),
		options: {
			version: { type: "string" },
			only: { type: "string" },
			"skip-missing": { type: "boolean", default: false },
			downloads: { type: "string" },
		},
		strict: true,
	});
	if (!values.version) throw new Error("missing --version");
	return {
		version: values.version,
		only: values.only ? values.only.split(",") : null,
		skipMissing: values["skip-missing"],
		downloads: values.downloads,
	};
}

/** @returns {Promise<Matrix>} */
async function readTargets() {
	const raw = await readFile(join(npmDir, "targets.json"), "utf8");
	return JSON.parse(raw);
}

async function clean() {
	await rm(join(npmDir, "dist"), { recursive: true, force: true });
	await mkdir(join(npmDir, "dist"), { recursive: true });
}

/**
 * @param {Matrix} matrix
 * @param {string} version
 * @param {Target[]} builtTargets
 */
async function buildFacade(matrix, version, builtTargets) {
	const tplPath = join(npmDir, "facade", "package.json");
	/** @type {Record<string, unknown>} */
	const tpl = JSON.parse(await readFile(tplPath, "utf8"));
	tpl.version = version;
	tpl.optionalDependencies = Object.fromEntries(
		builtTargets.map((t) => [`${matrix.scope}/${t.pkg}`, version]),
	);
	const dest = join(npmDir, "dist", matrix.facade);
	await mkdir(join(dest, "bin"), { recursive: true });
	await mkdir(join(dest, "lib"), { recursive: true });
	await writeFile(join(dest, "package.json"), `${JSON.stringify(tpl, null, 2)}\n`);
	await cp(join(npmDir, "facade", "README.md"), join(dest, "README.md"));
	await cp(join(repoDir, "LICENSE"), join(dest, "LICENSE"));
	for (const f of ["runner.cjs", "run.cjs"]) {
		await cp(join(npmDir, "facade", "bin", f), join(dest, "bin", f));
	}
	await cp(join(npmDir, "facade", "lib", "resolve.cjs"), join(dest, "lib", "resolve.cjs"));
	console.log(`built ${matrix.facade}@${version}`);
}

// Streamed tar.gz extractor that pulls just the bin paths we need.
// Avoids pulling in the `tar` npm dep — Node has zlib, and tar is a simple
// fixed-block format.
/**
 * @param {string} tarballPath
 * @param {string[]} names
 * @returns {Promise<Record<string, Buffer>>}
 */
async function extractBinariesFromTarball(tarballPath, names) {
	const stream = createReadStream(tarballPath).pipe(createGunzip());
	/** @type {Buffer[]} */
	const chunks = [];
	for await (const chunk of stream) chunks.push(chunk);
	const buf = Buffer.concat(chunks);

	const wanted = new Set(names);
	const wantedExe = new Set(names.map((n) => `${n}.exe`));
	/** @type {Record<string, Buffer>} */
	const found = {};

	let off = 0;
	while (off + 512 <= buf.length) {
		const header = buf.subarray(off, off + 512);
		if (header[0] === 0) break; // end-of-archive zero block
		const rawName = header.subarray(0, 100);
		const nameEnd = rawName.indexOf(0);
		const name = rawName.subarray(0, nameEnd === -1 ? 100 : nameEnd).toString("utf8");
		const sizeStr = header.subarray(124, 136).toString("utf8").trim().replace(/\0/g, "");
		const size = parseInt(sizeStr, 8) || 0;
		const typeflag = String.fromCharCode(header[156] || 0x30);
		off += 512;
		if (typeflag === "0" || typeflag === "\0") {
			const base = basename(name);
			if (wanted.has(base) || wantedExe.has(base)) {
				found[base] = buf.subarray(off, off + size);
			}
		}
		off += Math.ceil(size / 512) * 512;
	}
	return found;
}

/**
 * @param {Matrix} matrix
 * @param {Target} target
 * @param {string} version
 * @param {BuildOpts} opts
 * @returns {Promise<string | null>}
 */
async function buildPlatformPackage(matrix, target, version, opts) {
	const pkgName = `${matrix.scope}/${target.pkg}`;
	const dest = join(npmDir, "dist", target.pkg);
	await mkdir(join(dest, "bin"), { recursive: true });

	const downloadsDir = opts.downloads
		? resolve(opts.downloads)
		: join(npmDir, "downloads");
	const tag = version.startsWith("v") ? version : `v${version}`;
	const tarball = join(downloadsDir, `runner-${tag}-${target.rust}.tar.gz`);

	// Tier 3 entries (FreeBSD arm64, NetBSD, OpenBSD) run with
	// continue-on-error: true in release.yml — their tarballs may legitimately
	// be absent. --skip-missing extends this leniency to all tiers (manual
	// backfill).
	const isOptional = target.tier === 3 || opts.skipMissing;

	let binaries;
	try {
		binaries = await extractBinariesFromTarball(tarball, matrix.binaries);
	} catch (err) {
		if (isOptional) {
			console.warn(`skipping ${pkgName}: ${errCode(err) || errMessage(err)}`);
			await rm(dest, { recursive: true, force: true });
			return null;
		}
		throw new Error(`failed to read ${tarball}: ${errMessage(err)}`);
	}

	const isWin = target.os.includes("win32");
	for (const name of matrix.binaries) {
		const file = isWin ? `${name}.exe` : name;
		const data = binaries[file];
		if (!data) {
			if (isOptional) {
				console.warn(`skipping ${pkgName}: missing ${file} in archive`);
				await rm(dest, { recursive: true, force: true });
				return null;
			}
			throw new Error(`missing ${file} in ${tarball}`);
		}
		await writeFile(join(dest, "bin", file), data, { mode: 0o755 });
	}

	const pkg = {
		name: pkgName,
		version,
		description: `${target.pkg} prebuilt binaries for runner-run`,
		license: "MIT",
		author: "Kaj Kowalski <info+runner@kajkowalski.nl>",
		homepage: "https://github.com/kjanat/runner#readme",
		repository: { type: "git", url: "git+https://github.com/kjanat/runner.git" },
		bugs: { url: "https://github.com/kjanat/runner/issues" },
		os: target.os,
		cpu: target.cpu,
		...(target.libc ? { libc: target.libc } : {}),
		engines: { node: ">=18" },
		files: ["bin/"],
	};
	await writeFile(join(dest, "package.json"), `${JSON.stringify(pkg, null, 2)}\n`);
	await cp(join(repoDir, "LICENSE"), join(dest, "LICENSE"));

	const readme = `# ${pkgName}\n\n`
		+ `Prebuilt \`runner\` and \`run\` binaries for \`${target.pkg}\` `
		+ `(rustc target: \`${target.rust}\`).\n\n`
		+ `This package is an internal implementation detail of [\`runner-run\`](https://www.npmjs.com/package/runner-run). `
		+ `Don't depend on it directly — install \`runner-run\` and let npm pick the right sub-package for your platform.\n`;
	await writeFile(join(dest, "README.md"), readme);

	console.log(`built ${pkgName}@${version}`);
	return pkgName;
}

async function main() {
	const opts = readOpts();
	const matrix = await readTargets();
	await clean();
	const filter = opts.only ? new Set(opts.only) : null;
	const built = [];
	for (const target of matrix.targets) {
		if (filter && !filter.has(target.pkg)) continue;
		const result = await buildPlatformPackage(matrix, target, opts.version, opts);
		if (result) built.push(target);
	}
	if (built.length === 0) {
		throw new Error("no platform packages were built — refusing to publish a façade with empty optionalDependencies");
	}
	await buildFacade(matrix, opts.version, built);
}

if (argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
	main().catch((err) => {
		const trace = err instanceof Error ? (err.stack || err.message) : String(err);
		stderr.write(`build-packages: ${trace}\n`);
		exit(1);
	});
}
