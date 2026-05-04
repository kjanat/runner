#!/usr/bin/env node
/**
 * Builds npm package trees in `npm/dist/` for:
 *
 * - the facade package
 * - every per-platform package listed in `npm/targets.json`
 *
 * Native binary tarballs are read from `npm/downloads/` by default. CI usually
 * populates that directory with `gh release download`.
 *
 * Usage:
 *
 *   node npm/scripts/build-packages.ts                                  # version + meta from Cargo.toml
 *   node npm/scripts/build-packages.ts --only=linux-x64-gnu
 *   node npm/scripts/build-packages.ts --version 0.0.0-dev              # override the Cargo version
 *   node npm/scripts/build-packages.ts --downloads=/tmp/artifacts
 */
import { spawnSync } from "node:child_process";
import { cp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, posix, resolve } from "node:path";
import { argv, exit, stderr, stdout } from "node:process";
import { fileURLToPath } from "node:url";
import { parseArgs, promisify } from "node:util";
import { gunzip } from "node:zlib";

const gunzipAsync = promisify(gunzip);

const scriptPath = fileURLToPath(import.meta.url);
const here = dirname(scriptPath);
const npmDir = resolve(here, "..");
const repoDir = resolve(npmDir, "..");
const distDir = join(npmDir, "dist");

const BLOCK_SIZE = 512;

const FACADE_BIN_FILES = ["runner.cjs", "run.cjs"] as const;
const FACADE_LIB_FILES = ["resolve.cjs", "launch.cjs"] as const;

// npm allows multiple shapes for several `package.json` fields. Cargo's
// `package.metadata` is user-defined freeform JSON — the user could put any
// of these shapes (or a typo, or a number) under `metadata.npm.repository`
// and Cargo wouldn't care. We narrow at parse time so a malformed manifest
// fails the build with a useful pointer instead of producing a garbage
// package.json that fails later at `npm publish`.
type RepositoryField = string | { type?: string; url: string; directory?: string };
type BugsField = string | { url: string; email?: string };
type AuthorField = string | { name: string; email?: string; url?: string };
type EnginesField = Record<string, string>;

interface CargoManifest {
	name: string;
	version: string;
	license?: string;
	// Top-level Cargo fields. Both are always strings when Cargo emits them.
	homepage?: string;
	repository?: string;
	// Untyped on purpose — every access goes through a narrowing helper.
	metadata?: unknown;
}

function isObject(v: unknown): v is Record<string, unknown> {
	return typeof v === "object" && v !== null && !Array.isArray(v);
}

/** Accepts either a URL string or an object with a `url: string`. Other
 * optional fields (`type`, `directory`) are forwarded only when they're
 * strings. Anything else throws.
 */
function narrowRepository(v: unknown, where: string): RepositoryField | undefined {
	if (v === undefined) return undefined;
	if (typeof v === "string") return v;
	if (isObject(v) && typeof v.url === "string") {
		const out: { type?: string; url: string; directory?: string } = { url: v.url };
		if (typeof v.type === "string") out.type = v.type;
		if (typeof v.directory === "string") out.directory = v.directory;
		return out;
	}
	throw new Error(`${where} must be a URL string or { url: string, … }, got ${JSON.stringify(v)}`);
}

/** Accepts either a URL string or an object with a `url: string` and an
 * optional `email: string`. Anything else throws.
 */
function narrowBugs(v: unknown, where: string): BugsField | undefined {
	if (v === undefined) return undefined;
	if (typeof v === "string") return v;
	if (isObject(v) && typeof v.url === "string") {
		const out: { url: string; email?: string } = { url: v.url };
		if (typeof v.email === "string") out.email = v.email;
		return out;
	}
	throw new Error(`${where} must be a URL string or { url: string, … }, got ${JSON.stringify(v)}`);
}

/** Validates each value is a version-range string. Cargo's freeform metadata
 * could let a non-string slip in (e.g. `engines.node = 22`) and npm would
 * then reject the whole publish, so check here.
 */
function narrowEngines(v: unknown, where: string): EnginesField | undefined {
	if (v === undefined) return undefined;
	if (!isObject(v)) {
		throw new Error(`${where} must be an object of engine → version-range strings, got ${JSON.stringify(v)}`);
	}
	const out: EnginesField = {};
	for (const [k, val] of Object.entries(v)) {
		if (typeof val !== "string") {
			throw new Error(`${where}.${k} must be a version-range string, got ${JSON.stringify(val)}`);
		}
		out[k] = val;
	}
	return out;
}

/** Accepts a string ("Name <email>") or an object with at least `name`. */
function narrowAuthor(v: unknown, where: string): AuthorField | undefined {
	if (v === undefined) return undefined;
	if (typeof v === "string") return v;
	if (isObject(v) && typeof v.name === "string") {
		const out: { name: string; email?: string; url?: string } = { name: v.name };
		if (typeof v.email === "string") out.email = v.email;
		if (typeof v.url === "string") out.url = v.url;
		return out;
	}
	throw new Error(`${where} must be a string or { name: string, … }, got ${JSON.stringify(v)}`);
}

/** Reads the crate manifest as JSON. `cargo read-manifest` walks up to
 * find Cargo.toml itself, but we pin `cwd` so the script works from any cwd.
 * Returns the manifest with `metadata` left untyped — narrow before use.
 */
function readCargoManifest(): CargoManifest {
	const result = spawnSync("cargo", ["read-manifest"], {
		cwd: repoDir,
		encoding: "utf8",
	});
	if (result.status !== 0) {
		const err = (result.stderr || "").trim();
		throw new Error(`cargo read-manifest failed${err ? `: ${err}` : ""}`);
	}
	const parsed = JSON.parse(result.stdout) as {
		name?: unknown;
		version?: unknown;
		license?: unknown;
		homepage?: unknown;
		repository?: unknown;
		metadata?: unknown;
	};
	if (typeof parsed.name !== "string" || typeof parsed.version !== "string") {
		throw new Error(`cargo read-manifest produced unexpected shape (missing name/version)`);
	}
	return {
		name: parsed.name,
		version: parsed.version,
		license: typeof parsed.license === "string" ? parsed.license : undefined,
		homepage: typeof parsed.homepage === "string" ? parsed.homepage : undefined,
		repository: typeof parsed.repository === "string" ? parsed.repository : undefined,
		metadata: parsed.metadata,
	};
}

/** Returns the npm-canonical first author. Strings pass through as-is;
 * `{name, email}` objects collapse to `"Name <email>"` for consistency with
 * the previously-hardcoded value.
 */
function formatFirstAuthor(authorsRaw: unknown): string | undefined {
	if (authorsRaw === undefined) return undefined;
	if (!Array.isArray(authorsRaw)) {
		throw new Error(`metadata.authors must be an array, got ${JSON.stringify(authorsRaw)}`);
	}
	if (authorsRaw.length === 0) return undefined;
	const first = narrowAuthor(authorsRaw[0], "metadata.authors[0]");
	if (first === undefined) return undefined;
	if (typeof first === "string") return first;
	return first.email ? `${first.name} <${first.email}>` : first.name;
}

/** Derives the per-package npm metadata (license, author, homepage, repo,
 * bugs, engines) from the Cargo manifest. Each field is optional — missing
 * entries are simply omitted from the output package.json. Each present
 * entry is shape-validated before passing through.
 */
function packageMetadata(manifest: CargoManifest): Record<string, unknown> {
	const root = isObject(manifest.metadata) ? manifest.metadata : {};
	const npm = isObject(root.npm) ? root.npm : {};

	const out: Record<string, unknown> = {};
	if (manifest.license) out.license = manifest.license;

	const author = formatFirstAuthor(root.authors);
	if (author !== undefined) out.author = author;

	// `metadata.npm.*` overrides the top-level Cargo field when both are set.
	// Lets the npm artifact diverge from the Cargo crate's URLs (e.g. crates.io
	// vs npm landing pages) without duplicating data when they're the same.
	const homepage = typeof npm.homepage === "string" ? npm.homepage : manifest.homepage;
	if (homepage) out.homepage = homepage;

	const repo = narrowRepository(npm.repository, "metadata.npm.repository") ?? manifest.repository;
	if (repo !== undefined) out.repository = repo;

	const bugs = narrowBugs(npm.bugs, "metadata.npm.bugs");
	if (bugs !== undefined) out.bugs = bugs;

	const engines = narrowEngines(npm.engines, "metadata.npm.engines");
	if (engines !== undefined) out.engines = engines;

	return out;
}

const ansi = stdout.isTTY
	? {
		blue: "\x1b[34m",
		purple: "\x1b[35m",
		green: "\x1b[32m",
		underline: "\x1b[4m",
		italic: "\x1b[3m",
		reset: "\x1b[0m",
	}
	: {
		blue: "",
		purple: "",
		green: "",
		underline: "",
		italic: "",
		reset: "",
	};

type Libc = "glibc" | "musl";

interface Target {
	pkg: string;
	rust: string;
	os: NodeJS.Platform[];
	cpu: NodeJS.Architecture[];
	libc?: Libc[];
	runner: string;
	build: "cargo" | "cross";
	tier: 1 | 2 | 3;
	experimental?: boolean;
}

interface Matrix {
	facade: string;
	scope: string;
	binaries: string[];
	targets: Target[];
}

interface BuildOptions {
	version: string;
	only: Set<string> | null;
	skipMissing: boolean;
	downloadsDir: string;
}

interface TarEntry {
	name: string;
	size: number;
	type: string;
	bodyOffset: number;
}

function errorMessage(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

function errorCode(error: unknown): string | undefined {
	if (error instanceof Error && "code" in error && typeof error.code === "string") {
		return error.code;
	}

	return undefined;
}

function readOptions(defaultVersion: string): BuildOptions {
	const { values } = parseArgs({
		args: argv.slice(2),
		strict: true,
		options: {
			version: { type: "string" },
			only: { type: "string" },
			"skip-missing": { type: "boolean", default: false },
			downloads: { type: "string" },
		},
	});

	return {
		version: values.version || defaultVersion,
		only: parseOnlyList(values.only),
		skipMissing: values["skip-missing"] ?? false,
		downloadsDir: values.downloads ? resolve(values.downloads) : join(npmDir, "downloads"),
	};
}

function parseOnlyList(value: string | undefined): Set<string> | null {
	if (!value) return null;

	const packages = value
		.split(",")
		.map((item) => item.trim())
		.filter(Boolean);

	return packages.length > 0 ? new Set(packages) : null;
}

async function readMatrix(): Promise<Matrix> {
	const path = join(npmDir, "targets.json");
	return JSON.parse(await readFile(path, "utf8")) as Matrix;
}

async function cleanDist(): Promise<void> {
	await rm(distDir, { recursive: true, force: true });
	await mkdir(distDir, { recursive: true });
}

async function buildFacade(
	matrix: Matrix,
	version: string,
	builtTargets: Target[],
	meta: Record<string, unknown>,
): Promise<void> {
	const templatePath = join(npmDir, "facade", "package.json");
	const template = JSON.parse(await readFile(templatePath, "utf8")) as Record<string, unknown>;

	// Cargo metadata wins over the template for fields it owns (license,
	// author, homepage, repository, bugs, engines). Keeps the facade and
	// sub-packages in lockstep on engines/runtime contract — drop those
	// fields from the template so there's only one source of truth.
	const packageJson = {
		...template,
		...meta,
		version,
		optionalDependencies: Object.fromEntries(
			builtTargets.map((target) => [`${matrix.scope}/${target.pkg}`, version]),
		),
	};

	const dest = join(distDir, matrix.facade);

	await mkdir(join(dest, "bin"), { recursive: true });
	await mkdir(join(dest, "lib"), { recursive: true });

	await writeJson(join(dest, "package.json"), packageJson);
	await cp(join(npmDir, "facade", "README.md"), join(dest, "README.md"));
	await cp(join(repoDir, "LICENSE"), join(dest, "LICENSE"));

	await copyFiles(join(npmDir, "facade", "bin"), join(dest, "bin"), FACADE_BIN_FILES);
	await copyFiles(join(npmDir, "facade", "lib"), join(dest, "lib"), FACADE_LIB_FILES);

	console.log(`built ${formatPackage(undefined, matrix.facade, version)}`);
}

async function buildPlatformPackage(
	matrix: Matrix,
	target: Target,
	opts: BuildOptions,
	meta: Record<string, unknown>,
): Promise<Target | null> {
	const packageName = `${matrix.scope}/${target.pkg}`;
	const dest = join(distDir, target.pkg);
	const tarball = tarballPath(opts.downloadsDir, opts.version, target);
	const maySkip = opts.skipMissing || target.tier === 3;

	await mkdir(join(dest, "bin"), { recursive: true });

	let binaries: Map<string, Buffer>;

	try {
		binaries = await extractBinariesFromTarball(tarball, matrix.binaries);
	} catch (error) {
		if (maySkip) {
			console.warn(`skipping ${packageName}: ${errorCode(error) ?? errorMessage(error)}`);
			await removePartialPackage(dest);
			return null;
		}

		throw new Error(`failed to read ${tarball}: ${errorMessage(error)}`);
	}

	const missing = await writePlatformBinaries(dest, matrix.binaries, target, binaries);

	if (missing) {
		if (maySkip) {
			console.warn(`skipping ${packageName}: missing ${missing} in archive`);
			await removePartialPackage(dest);
			return null;
		}

		throw new Error(`missing ${missing} in ${tarball}`);
	}

	const pkg = platformPackageJson(matrix, target, opts.version, meta);
	await writeJson(join(dest, "package.json"), pkg);
	console.debug(pkg);

	const readme = platformReadme(matrix, target);
	await writeFile(join(dest, "README.md"), readme);
	console.debug(readme);

	const license = await readFile(join(repoDir, "LICENSE"), "utf8");
	await writeFile(join(dest, "LICENSE"), license);

	console.log(`built ${formatPackage(matrix.scope, target.pkg, opts.version)}`);
	return target;
}

async function writePlatformBinaries(
	dest: string,
	binaryNames: string[],
	target: Target,
	binaries: Map<string, Buffer>,
): Promise<string | null> {
	for (const binaryName of binaryNames) {
		const fileName = target.os.includes("win32") ? `${binaryName}.exe` : binaryName;
		const data = binaries.get(fileName);

		if (!data) return fileName;

		await writeFile(join(dest, "bin", fileName), data, { mode: 0o755 });
	}

	return null;
}

function platformPackageJson(
	matrix: Matrix,
	target: Target,
	version: string,
	meta: Record<string, unknown>,
): Record<string, unknown> {
	return {
		name: `${matrix.scope}/${target.pkg}`,
		version,
		description: `${target.pkg} prebuilt binaries for ${matrix.facade}`,
		...meta,
		os: target.os,
		cpu: target.cpu,
		...(target.libc ? { libc: target.libc } : {}),
		exports: { "./package.json": "./package.json" },
		directories: { bin: "./bin" },
		files: ["bin/"],
	};
}

function platformReadme(matrix: Matrix, target: Target): string {
	const packageName = `${matrix.scope}/${target.pkg}`;
	const binaries = matrix.binaries.map((name) => `\`${name}\``).join(" and ");

	return `# ${packageName}

Prebuilt ${binaries} binaries for \`${target.pkg}\`.\\
(rustc target: \`${target.rust}\`).

This package is an internal implementation detail of [\`${matrix.facade}\`](https://npm.im/${matrix.facade} "View on npm").

Do not depend on it directly.\\
Install \`${matrix.facade}\` and let npm select the right package for your platform.
`;
}

async function extractBinariesFromTarball(
	tarballPath: string,
	binaryNames: string[],
): Promise<Map<string, Buffer>> {
	const compressed = await readFile(tarballPath);
	const tar = await gunzipAsync(compressed);

	const wanted = new Set([
		...binaryNames,
		...binaryNames.map((name) => `${name}.exe`),
	]);

	const found = new Map<string, Buffer>();

	for (const entry of readTarEntries(tar)) {
		if (!isRegularTarFile(entry.type)) continue;

		const fileName = posix.basename(entry.name);
		if (!wanted.has(fileName)) continue;

		found.set(fileName, Buffer.from(tar.subarray(entry.bodyOffset, entry.bodyOffset + entry.size)));
	}

	return found;
}

function readTarEntries(tar: Buffer): TarEntry[] {
	const entries: TarEntry[] = [];

	let offset = 0;

	while (offset + BLOCK_SIZE <= tar.length) {
		const header = tar.subarray(offset, offset + BLOCK_SIZE);

		if (isZeroBlock(header)) break;

		const name = readTarPath(header);
		const size = readTarSize(header);
		const type = readTarString(header, 156, 1) || "0";
		const bodyOffset = offset + BLOCK_SIZE;
		const nextOffset = bodyOffset + alignToBlock(size);

		if (nextOffset > tar.length) {
			throw new Error(`malformed tar archive: ${name} extends past end of file`);
		}

		entries.push({ name, size, type, bodyOffset });
		offset = nextOffset;
	}

	return entries;
}

function readTarPath(header: Buffer): string {
	const name = readTarString(header, 0, 100);
	const prefix = readTarString(header, 345, 155);

	return prefix ? `${prefix}/${name}` : name;
}

function readTarSize(header: Buffer): number {
	const raw = readTarString(header, 124, 12).trim();

	if (!raw) return 0;

	const size = Number.parseInt(raw, 8);

	if (!Number.isFinite(size)) {
		throw new Error(`malformed tar archive: invalid size ${JSON.stringify(raw)}`);
	}

	return size;
}

function readTarString(buffer: Buffer, start: number, length: number): string {
	const bytes = buffer.subarray(start, start + length);
	const end = bytes.indexOf(0);
	const slice = end === -1 ? bytes : bytes.subarray(0, end);

	return slice.toString("utf8");
}

function isZeroBlock(block: Buffer): boolean {
	return block.every((byte) => byte === 0);
}

function isRegularTarFile(type: string): boolean {
	return type === "0" || type === "\0";
}

function alignToBlock(size: number): number {
	return Math.ceil(size / BLOCK_SIZE) * BLOCK_SIZE;
}

function tarballPath(downloadsDir: string, version: string, target: Target): string {
	const tag = version.startsWith("v") ? version : `v${version}`;
	return join(downloadsDir, `runner-${tag}-${target.rust}.tar.gz`);
}

async function copyFiles(
	fromDir: string,
	toDir: string,
	files: readonly string[],
): Promise<void> {
	for (const file of files) {
		await cp(join(fromDir, file), join(toDir, file));
	}
}

async function writeJson(path: string, value: unknown): Promise<void> {
	await writeFile(path, `${JSON.stringify(value, null, 2)}\n`);
}

async function removePartialPackage(path: string): Promise<void> {
	await rm(path, { recursive: true, force: true });
}

function formatPackage(scope: string | undefined, name: string, version: string): string {
	const packageName = scope ? `${scope}/${name}` : name;
	const formattedName = `${ansi.blue}${ansi.underline}${packageName}${ansi.reset}`;
	const formattedVersion = `${ansi.green}@${ansi.reset}${ansi.purple}${ansi.italic}${version}${ansi.reset}`;

	return `${formattedName}${formattedVersion}`;
}

async function main(): Promise<void> {
	const manifest = readCargoManifest();
	const opts = readOptions(manifest.version);
	const meta = packageMetadata(manifest);
	const matrix = await readMatrix();

	await cleanDist();

	const builtTargets: Target[] = [];

	for (const target of matrix.targets) {
		if (opts.only && !opts.only.has(target.pkg)) continue;

		const built = await buildPlatformPackage(matrix, target, opts, meta);
		if (built) builtTargets.push(built);
	}

	if (builtTargets.length === 0) {
		throw new Error(
			"no platform packages were built; refusing to publish a facade with empty optionalDependencies",
		);
	}

	await buildFacade(matrix, opts.version, builtTargets, meta);
}

if (import.meta.main) {
	main().catch((error) => {
		const trace = error instanceof Error ? error.stack ?? error.message : String(error);
		stderr.write(`build-packages: ${trace}\n`);
		exit(1);
	});
}
