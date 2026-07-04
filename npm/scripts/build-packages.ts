#!/usr/bin/env node
/**
 * Builds npm package trees in `npm/dist/` for:
 *
 * - the facade package
 * - every per-platform package listed in `npm/targets.json`
 *
 * Native binary tarballs are read from `npm/downloads/` by default. CI usually
 * populates that directory with `gh release download`. Outside CI (no
 * `GITHUB_ACTIONS=true`), a dev machine only ever has native binaries for its
 * own host, so a bare local run: builds the host's own tarball with
 * `cargo bbr` if it's missing, and treats every other target's missing
 * tarball as skippable (same as `--skip-missing`) instead of failing — a
 * plain `build-packages` "just works" for whatever platform you're on.
 *
 * Usage:
 *
 *   node npm/scripts/build-packages.ts                                  # version + meta from Cargo.toml
 *   node npm/scripts/build-packages.ts --only=linux-x64-gnu
 *   node npm/scripts/build-packages.ts --version 0.0.0-dev              # override the Cargo version
 *   node npm/scripts/build-packages.ts --downloads=/tmp/artifacts
 */
import { spawnSync } from "node:child_process";
import { access, cp, mkdir, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, posix, resolve } from "node:path";
import { argv, env, exit, stderr, stdout } from "node:process";
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
/**
 * Package-relative launcher shipped inside every platform package so each
 * `@runner-run/*` package is a functional CLI on its own (`npx
 * @runner-run/<platform> …`). Exactly ONE bin entry on purpose: npx can
 * auto-select a package's sole bin regardless of its name, whereas two
 * entries would force users to always name the command (the 0.16.1 "could
 * not determine executable" failure shape).
 */
const PLATFORM_SHIM = "runner.cjs";

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

/**
 * Determine whether a value is a non-null, non-array object.
 *
 * Acts as a type guard that narrows the input to `Record<string, unknown>`.
 *
 * @param v - The value to test
 * @returns `true` if `v` is an object (not `null` and not an array), `false` otherwise.
 */
function isObject(v: unknown): v is Record<string, unknown> {
	return typeof v === "object" && v !== null && !Array.isArray(v);
}

/**
 * Validate and normalize a repository field from Cargo metadata into an npm-compatible repository descriptor.
 *
 * Accepts `undefined`, a URL string, or an object with a required `url` string and optional `type` and `directory` strings.
 *
 * @param v - The raw repository value to validate (may be `undefined`, a string URL, or an object).
 * @param where - Context string used in the thrown error message when validation fails.
 * @returns The original URL string, or an object `{ url: string, type?: string, directory?: string }`, or `undefined` when `v` is `undefined`.
 * @throws Error if `v` is neither `undefined`, a string, nor an object with a string `url` (or if optional fields are present but not strings).
 */
function narrowRepository(v: unknown, where: string): RepositoryField | undefined {
	if (v === undefined) return undefined;
	if (typeof v === "string") return v;
	if (isObject(v) && typeof v.url === "string") {
		const out: { type?: string; url: string; directory?: string } = { url: v.url };
		if (v.type !== undefined) {
			if (typeof v.type !== "string") {
				throw new Error(`${where}.type must be a string when present, got ${JSON.stringify(v.type)}`);
			}
			out.type = v.type;
		}
		if (v.directory !== undefined) {
			if (typeof v.directory !== "string") {
				throw new Error(`${where}.directory must be a string when present, got ${JSON.stringify(v.directory)}`);
			}
			out.directory = v.directory;
		}
		return out;
	}
	throw new Error(`${where} must be a URL string or { url: string, … }, got ${JSON.stringify(v)}`);
}

/**
 * Validate and normalize a `bugs` field into an allowed npm shape.
 *
 * @param v - The raw value to validate; may be `undefined`, a URL string, or an object.
 * @param where - Context used in the error message when `v` has an invalid shape.
 * @returns `undefined` if `v` is `undefined`; the original URL string if `v` is a string; otherwise an object with a required `url` string and an optional `email` string.
 * @throws If `v` is not `undefined`, not a string, and not an object with a string `url`.
 */
function narrowBugs(v: unknown, where: string): BugsField | undefined {
	if (v === undefined) return undefined;
	if (typeof v === "string") return v;
	if (isObject(v) && typeof v.url === "string") {
		const out: { url: string; email?: string } = { url: v.url };
		if (v.email !== undefined) {
			if (typeof v.email !== "string") {
				throw new Error(`${where}.email must be a string when present, got ${JSON.stringify(v.email)}`);
			}
			out.email = v.email;
		}
		return out;
	}
	throw new Error(`${where} must be a URL string or { url: string, … }, got ${JSON.stringify(v)}`);
}

/**
 * Validate and return an object mapping engine names to version-range strings.
 *
 * Accepts a freeform value and ensures it is an object whose property values are strings.
 *
 * @param v - The value to validate; may be `undefined`.
 * @param where - A contextual path used in thrown error messages.
 * @returns The validated `EnginesField` object, or `undefined` if `v` is `undefined`.
 * @throws If `v` is not an object or any property value is not a string.
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

/**
 * Normalize an author value from Cargo metadata into a canonical author representation.
 *
 * Accepts the common Cargo shapes and returns a value suitable for npm `author` metadata.
 *
 * @param v - The raw author value: `undefined`, a string like `"Name <email>"`,
 *   or an object with at least a `name` string and optional `email` and `url` properties.
 * @param where - A descriptive location used in the error message when validation fails.
 * @returns The normalized author value: the original string or an object with
 *   `name` and optional `email` and `url`, or `undefined` when `v` is `undefined`.
 * @throws If `v` is present but is neither a string nor an object containing a `name` string.
 */
function narrowAuthor(v: unknown, where: string): AuthorField | undefined {
	if (v === undefined) return undefined;
	if (typeof v === "string") return v;
	if (isObject(v) && typeof v.name === "string") {
		const out: { name: string; email?: string; url?: string } = { name: v.name };
		if (v.email !== undefined) {
			if (typeof v.email !== "string") {
				throw new Error(`${where}.email must be a string when present, got ${JSON.stringify(v.email)}`);
			}
			out.email = v.email;
		}
		if (v.url !== undefined) {
			if (typeof v.url !== "string") {
				throw new Error(`${where}.url must be a string when present, got ${JSON.stringify(v.url)}`);
			}
			out.url = v.url;
		}
		return out;
	}
	throw new Error(`${where} must be a string or { name: string, … }, got ${JSON.stringify(v)}`);
}

/**
 * Read the crate's Cargo manifest and return its parsed JSON; `metadata` is left unvalidated.
 *
 * Uses `cargo metadata --no-deps` (the `cargo read-manifest` replacement) and
 * picks the workspace's default member.
 *
 * @returns The parsed Cargo manifest object; the `metadata` property is preserved
 *   as-is and must be narrowed before use.
 * @throws If `cargo metadata` fails (the error message includes the command's stderr).
 * @throws If the workspace's default package is missing or lacks required `name`/`version` string fields.
 */
function readCargoManifest(): CargoManifest {
	const result = spawnSync("cargo", ["metadata", "--no-deps", "--format-version", "1"], {
		cwd: repoDir,
		encoding: "utf8",
		// metadata output for a workspace can dwarf the 1 MiB Node default.
		maxBuffer: 64 * 1024 * 1024,
	});
	if (result.status !== 0) {
		const err = (result.stderr || "").trim();
		throw new Error(`cargo metadata failed${err ? `: ${err}` : ""}`);
	}
	const envelope = JSON.parse(result.stdout) as {
		packages?: unknown;
		workspace_default_members?: unknown;
	};
	if (!Array.isArray(envelope.packages) || envelope.packages.length === 0) {
		throw new Error(`cargo metadata produced unexpected shape (no packages)`);
	}
	const defaults = Array.isArray(envelope.workspace_default_members)
		? envelope.workspace_default_members.filter((id): id is string => typeof id === "string")
		: [];
	// Single workspace member: that's the package. Multi-member workspace:
	// match the first default member by id (Cargo's own "what does a bare
	// `cargo build` build" answer) — falls back to the first package so a
	// non-virtual workspace without explicit defaults still resolves.
	const pickById = defaults[0]
		? envelope.packages.find((p): p is Record<string, unknown> => isObject(p) && p.id === defaults[0])
		: undefined;
	const pkg = pickById ?? (isObject(envelope.packages[0]) ? envelope.packages[0] : undefined);
	if (!pkg) {
		throw new Error(`cargo metadata produced unexpected shape (no usable package entry)`);
	}
	if (typeof pkg.name !== "string" || typeof pkg.version !== "string") {
		throw new Error(`cargo metadata produced unexpected shape (missing name/version)`);
	}
	return {
		name: pkg.name,
		version: pkg.version,
		license: typeof pkg.license === "string" ? pkg.license : undefined,
		homepage: typeof pkg.homepage === "string" ? pkg.homepage : undefined,
		repository: typeof pkg.repository === "string" ? pkg.repository : undefined,
		metadata: pkg.metadata,
	};
}

/**
 * Normalize the first entry of Cargo `metadata.authors` into an npm-compatible author string.
 *
 * @param authorsRaw - The raw `metadata.authors` value from a Cargo manifest;
 *   may be undefined or an array.
 * @returns The formatted first author: the original string if the entry is a
 *   string, `"Name <email>"` when the entry is an object with `name` and `email`,
 *   `name` when the entry is an object without `email`, or `undefined` if no
 *   author is present.
 * @throws If `authorsRaw` is present but not an array.
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

/**
 * Build a partial npm package.json object from a Cargo manifest's metadata.
 *
 * Produces an object containing any of: `license`, `author`, `homepage`,
 * `repository`, `bugs`, and `engines` when those values are present and valid.
 * Fields under `metadata.npm` override top-level Cargo values when provided.
 *
 * @param manifest - The Cargo manifest object (as returned by `readCargoManifest`)
 * @returns A plain object with npm package fields to merge into `package.json`
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
	build: "cargo" | "cross" | "cargo-cross-toolchain" | "cargo-build-std" | "vm";
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
	/** Dir of `*.1` man pages to ship in the facade; `null` to skip. */
	manDir: string | null;
	/**
	 * `true` outside CI: a missing tarball for a *non-host* target is treated
	 * as skippable (a dev machine can't produce it without cross-compiling),
	 * and a missing tarball for the *host* target is built on demand instead
	 * of failing. `false` in CI, where every target's tarball is expected to
	 * already exist via `gh release download` and a miss is a real problem.
	 */
	local: boolean;
	/** This machine's Rust target triple (`rustc --print host-tuple`), or `null` if `rustc` isn't on `PATH`. */
	hostTriple: string | null;
}

interface TarEntry {
	name: string;
	size: number;
	type: string;
	bodyOffset: number;
}

/**
 * Produce a human-readable message from an arbitrary thrown value.
 *
 * @param error - The thrown value or error-like object to extract a message from
 * @returns The `message` property when `error` is an `Error`, otherwise `String(error)`
 */
function errorMessage(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

/**
 * Extracts the `code` property from an Error-like value when that property is a string.
 *
 * @param error - The value to inspect; typically an Error or Error-like object.
 * @returns The `code` string if present on `error`, `undefined` otherwise.
 */
function errorCode(error: unknown): string | undefined {
	if (error instanceof Error && "code" in error && typeof error.code === "string") {
		return error.code;
	}

	return undefined;
}

/**
 * Whether this run is executing under GitHub Actions (`GITHUB_ACTIONS=true`).
 * CI is the only environment where every platform's tarball is expected to
 * already exist (via `gh release download`), so it's also the flag used to
 * decide whether a missing tarball is a hard failure or something a local
 * dev run can build or skip.
 */
function isCi(): boolean {
	return env.GITHUB_ACTIONS === "true";
}

/**
 * Run `fn` inside a GitHub Actions log group when executing under Actions;
 * otherwise just run `fn` with no extra output.
 *
 * Emits `::group::<title>` before and `::endgroup::` after, even on throw,
 * so each per-package build collapses cleanly in the workflow log without
 * altering local-dev output.
 *
 * @param title - Group title shown in the collapsed Actions log row
 * @param fn - Async work to execute inside the group
 * @returns Whatever `fn` resolves to
 */
async function withLogGroup<T>(title: string, fn: () => Promise<T>): Promise<T> {
	const inActions = isCi();
	if (inActions) stdout.write(`::group::${title}\n`);
	try {
		return await fn();
	} finally {
		if (inActions) stdout.write("::endgroup::\n");
	}
}

/**
 * Parse CLI flags and return normalized build options for the packaging run.
 *
 * @param defaultVersion - Version string to use when `--version` is not provided
 * @returns An object with resolved options:
 *  - `version`: the effective version string
 *  - `only`: a `Set` of package names to build, or `null` when no `--only` was supplied
 *  - `skipMissing`: `true` when missing platform tarballs should be skipped
 *  - `downloadsDir`: absolute path to the downloads directory to read tarballs from
 */
function readOptions(defaultVersion: string): BuildOptions {
	const { values } = parseArgs({
		args: argv.slice(2),
		strict: true,
		options: {
			version: { type: "string" },
			only: { type: "string" },
			"skip-missing": { type: "boolean", default: false },
			downloads: { type: "string" },
			"man-dir": { type: "string" },
		},
	});

	return {
		version: values.version || defaultVersion,
		only: parseOnlyList(values.only),
		skipMissing: values["skip-missing"] ?? false,
		downloadsDir: values.downloads ? resolve(values.downloads) : join(npmDir, "downloads"),
		manDir: values["man-dir"] ? resolve(values["man-dir"]) : null,
		local: !isCi(),
		hostTriple: hostRustTriple(),
	};
}

/**
 * The current machine's Rust target triple, via `rustc --print host-tuple`.
 * Used to tell which `npm/targets.json` entry is buildable locally without
 * cross-compiling.
 *
 * @returns The host triple, or `null` if `rustc` isn't on `PATH` or fails.
 */
function hostRustTriple(): string | null {
	const result = spawnSync("rustc", ["--print", "host-tuple"], { encoding: "utf8" });
	return result.status === 0 ? result.stdout.trim() : null;
}

/**
 * Parses a comma-separated string into a set of trimmed package names.
 *
 * @param value - A comma-separated list of package names (may include whitespace);
 *   if empty or undefined, nothing will be parsed.
 * @returns A `Set` containing each trimmed package name when one or more names
 *   are present, `null` otherwise.
 */
function parseOnlyList(value: string | undefined): Set<string> | null {
	if (!value) return null;

	const packages = value
		.split(",")
		.map((item) => item.trim())
		.filter(Boolean);

	return packages.length > 0 ? new Set(packages) : null;
}

/**
 * Load and parse the build matrix from npm/targets.json.
 *
 * @returns The parsed `Matrix` containing the facade package name, npm scope,
 * and the list of per-target package definitions.
 */
async function readMatrix(): Promise<Matrix> {
	const path = join(npmDir, "targets.json");
	return JSON.parse(await readFile(path, "utf8")) as Matrix;
}

/**
 * Remove the distribution directory and recreate an empty one.
 *
 * Deletes the directory referenced by `distDir` if it exists, then recreates
 * it so the dist directory is empty and ready for new outputs.
 */
async function cleanDist(): Promise<void> {
	await rm(distDir, { recursive: true, force: true });
	await mkdir(distDir, { recursive: true });
}

/**
 * Builds the facade npm package in the distribution directory from the provided
 * template and metadata.
 *
 * @param matrix - Build matrix describing the facade name and scope and
 *   available binaries.
 * @param version - The package version to write into the facade's package.json.
 * @param builtTargets - List of successfully built platform targets used to
 *   populate `optionalDependencies`.
 * @param meta - Npm metadata derived from the Cargo manifest (e.g. `license`,
 *   `author`, `homepage`, `repository`, `bugs`, `engines`) to merge into the
 *   generated package.json.
 */
async function buildFacade(
	matrix: Matrix,
	version: string,
	builtTargets: Target[],
	meta: Record<string, unknown>,
	manDir: string | null,
): Promise<void> {
	const templatePath = join(npmDir, "facade", "package.json");
	const template = JSON.parse(await readFile(templatePath, "utf8")) as Record<string, unknown>;

	const dest = join(distDir, matrix.facade);

	await mkdir(join(dest, "bin"), { recursive: true });
	await mkdir(join(dest, "lib"), { recursive: true });

	// npm symlinks these into share/man on global install.
	const manFiles = await stageManPages(dest, manDir);

	// Cargo metadata wins over the template for fields it owns (license,
	// author, homepage, repository, bugs, engines). Keeps the facade and
	// sub-packages in lockstep on engines/runtime contract — drop those
	// fields from the template so there's only one source of truth.
	const files = Array.isArray(template.files) ? [...template.files] : [];
	if (manFiles.length > 0 && !files.includes("man/")) files.push("man/");

	const packageJson = {
		...template,
		...meta,
		version,
		...(manFiles.length > 0 ? { man: manFiles, files } : {}),
		optionalDependencies: Object.fromEntries(
			builtTargets.map((target) => [`${matrix.scope}/${target.pkg}`, version]),
		),
	};

	await writeJson(join(dest, "package.json"), packageJson);
	await cp(join(npmDir, "facade", "README.md"), join(dest, "README.md"));
	await cp(join(repoDir, "LICENSE"), join(dest, "LICENSE"));

	await copyFiles(join(npmDir, "facade", "bin"), join(dest, "bin"), FACADE_BIN_FILES);
	await copyFiles(join(npmDir, "facade", "lib"), join(dest, "lib"), FACADE_LIB_FILES);

	console.log(`built ${formatPackage(undefined, matrix.facade, version)}`);
}

/** Copy `*.1` from `manDir` into `<dest>/man/`; return their `man/<file>` paths. */
async function stageManPages(dest: string, manDir: string | null): Promise<string[]> {
	if (!manDir) return [];

	const entries = (await readdir(manDir)).filter((name) => name.endsWith(".1")).sort();
	if (entries.length === 0) return [];

	await mkdir(join(dest, "man"), { recursive: true });
	const manFiles: string[] = [];
	for (const entry of entries) {
		await cp(join(manDir, entry), join(dest, "man", entry));
		manFiles.push(posix.join("man", entry));
	}

	return manFiles;
}

/**
 * Build the host's own release binaries with `cargo bbr` and pack them into
 * the tarball `buildPlatformPackage` expects, so a bare local `build-packages`
 * run works without a prior `gh release download` or `just test-release`.
 *
 * @param matrix - Build matrix; `matrix.binaries` names the expected binaries
 * @param target - The host-matching target to build a tarball for
 * @param version - Release version, used to name the produced tarball
 * @param downloadsDir - Directory the tarball is written into
 * @throws If `cargo bbr` fails, an expected release binary is missing afterward, or `tar` fails
 */
async function buildHostTarball(
	matrix: Matrix,
	target: Target,
	version: string,
	downloadsDir: string,
): Promise<void> {
	console.log(`→ no tarball for host target ${target.rust}; building it with \`cargo bbr\``);

	const build = spawnSync("cargo", ["bbr"], { cwd: repoDir, stdio: "inherit" });
	if (build.status !== 0) {
		throw new Error(`cargo bbr failed while building the host tarball for ${target.rust}`);
	}

	const releaseDir = join(repoDir, "target", "release");
	const fileNames = matrix.binaries.map((name) => (target.os.includes("win32") ? `${name}.exe` : name));
	for (const fileName of fileNames) {
		const path = join(releaseDir, fileName);
		await access(path).catch(() => {
			throw new Error(`expected ${path} to exist after cargo bbr`);
		});
	}

	await mkdir(downloadsDir, { recursive: true });
	const tarball = tarballPath(downloadsDir, version, target);
	const packed = spawnSync("tar", ["czf", tarball, "-C", releaseDir, ...fileNames]);
	if (packed.status !== 0) {
		throw new Error(
			`tar failed while packing the host tarball for ${target.rust}: ${(packed.stderr ?? "").toString().trim()}`,
		);
	}
}

/**
 * Builds a platform-specific npm package directory by extracting required
 * binaries and writing package files.
 *
 * Attempts to locate and extract the platform runner tarball for `target`,
 * writes the extracted binaries to
 * `npm/dist/<target.pkg>/bin/`, and creates `package.json`, `README.md`, and
 * `LICENSE` in that destination.
 *
 * @param matrix - Build matrix describing the facade and the list of binary names to include
 * @param target - Platform target definition used to name the package and determine file names and metadata
 * @param opts - Runtime build options (version, downloads directory, skipMissing behavior, host auto-build)
 * @param meta - Partial npm metadata derived from the Cargo manifest to be merged into the package.json
 * @returns The provided `target` when the package was built successfully, or
 *   `null` when the target was skipped due to a missing tarball or binaries (honoring `opts.skipMissing`, tier 3 targets, or a local non-host run)
 */
async function buildPlatformPackage(
	matrix: Matrix,
	target: Target,
	opts: BuildOptions,
	meta: Record<string, unknown>,
): Promise<Target | null> {
	const packageName = `${matrix.scope}/${target.pkg}`;
	const dest = join(distDir, target.pkg);
	const tarball = tarballPath(opts.downloadsDir, opts.version, target);
	const maySkip = opts.skipMissing || target.tier === 3 || opts.local;
	const isHost = opts.local && opts.hostTriple === target.rust;

	await mkdir(join(dest, "bin"), { recursive: true });

	let binaries: Map<string, Buffer>;

	try {
		binaries = await extractBinariesFromTarball(tarball, matrix.binaries);
	} catch (error) {
		if (isHost && errorCode(error) === "ENOENT") {
			await buildHostTarball(matrix, target, opts.version, opts.downloadsDir);
			binaries = await extractBinariesFromTarball(tarball, matrix.binaries);
		} else if (maySkip) {
			console.warn(`skipping ${packageName}: ${errorCode(error) ?? errorMessage(error)}`);
			await removePartialPackage(dest);
			return null;
		} else {
			throw new Error(`failed to read ${tarball}: ${errorMessage(error)}`);
		}
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

	await cp(join(npmDir, "platform", PLATFORM_SHIM), join(dest, PLATFORM_SHIM));

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

/**
 * Writes extracted binary buffers into the package's bin directory, using ".exe" suffixes for Windows targets.
 *
 * @param dest - Destination package directory; binaries are written under `dest/bin/`
 * @param binaryNames - Expected binary basenames to write
 * @param target - Target metadata; when `target.os` includes `"win32"`, filenames are suffixed with `.exe`
 * @param binaries - Map from filename (basename or with `.exe`) to its file contents
 * @returns The missing filename that was not found in `binaries`, or `null` if all binaries were written
 */
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

/**
 * Create the package.json object for a platform-specific prebuilt package.
 *
 * @param matrix - Build matrix providing the facade name and npm scope used to form the package name
 * @param target - Target descriptor whose `pkg`, `os`, `cpu`, and optional
 *   `libc` fields determine package identity and platform metadata
 * @param version - Version string to set on the package
 * @param meta - Additional npm fields (derived from Cargo metadata) to merge into the package.json
 * @returns The package.json object containing `name`, `version`, `description`,
 * merged `meta`, platform `os`/`cpu` (and optional `libc`), restricted `exports`, `directories.bin`, and `files`
 */
function platformPackageJson(
	matrix: Matrix,
	target: Target,
	version: string,
	meta: Record<string, unknown>,
): Record<string, unknown> {
	const keywords = [
		...new Set([
			matrix.facade,
			"prebuilt",
			"binary",
			"native",
			"cli",
			"task-runner",
			...target.os,
			...target.cpu,
			...(target.libc ?? []),
		]),
	];
	return {
		name: `${matrix.scope}/${target.pkg}`,
		version,
		description: `Prebuilt ${
			matrix.binaries.join(" + ")
		} ${target.rust} binaries for ${matrix.facade}; selected automatically by npm, also runnable standalone via npx.`,
		keywords,
		...meta,
		os: target.os,
		cpu: target.cpu,
		...(target.libc ? { libc: target.libc } : {}),
		// A single bin entry keeps `npx ${matrix.scope}/<pkg> …` working via
		// npx's only-bin auto-selection; see PLATFORM_SHIM. Named after the
		// primary binary, which is also the one the shim spawns.
		bin: { [matrix.binaries[0]]: PLATFORM_SHIM },
		exports: { "./package.json": "./package.json" },
		directories: { bin: "./bin" },
		files: ["bin/", PLATFORM_SHIM],
	};
}

/**
 * Generate a Markdown README for a platform package.
 *
 * Produces a README that names the package, lists the included binaries, shows the `rustc` target,
 * and explains that the package is an internal implementation detail of the facade package with
 * install guidance.
 *
 * @param matrix - Build matrix containing `facade`, `scope`, and `binaries` used in the README
 * @param target - Platform target containing `pkg` and `rust` values referenced in the README
 * @returns The generated README as a Markdown string
 */
function platformReadme(matrix: Matrix, target: Target): string {
	const packageName = `${matrix.scope}/${target.pkg}`;
	const binaries = matrix.binaries.map((name) => `\`${name}\``).join(" and ");
	const noun = matrix.binaries.length === 1 ? "binary" : "binaries";
	const platform = [...target.os, ...target.cpu, ...(target.libc ? [target.libc] : [])].join(" · ");

	const primary = matrix.binaries[0];

	return `# ${packageName}

Prebuilt ${binaries} ${noun} for **${platform}** (rustc target \`${target.rust}\`).
The platform-specific package of [\`${matrix.facade}\`](https://npm.im/${matrix.facade} "View on npm").

## Do I install this?

Usually not. Install the main package and npm picks the matching binary for your platform automatically:

\`\`\`sh
npm install ${matrix.facade}
\`\`\`

This package is listed in \`${matrix.facade}\`'s \`optionalDependencies\`. npm resolves the one
whose \`os\`/\`cpu\`${target.libc ? "/`libc`" : ""} matches your machine and skips the rest, so the wrapper
finds these binaries with no postinstall step. Depending on it directly pins you to a single
platform, so prefer \`${matrix.facade}\` for anything portable.

## Standalone use

The package is a working CLI in its own right — it ships a \`${primary}\` bin that launches
the bundled binary, so on a matching machine it runs without the facade:

\`\`\`sh
npx ${packageName} list
npx ${packageName} install -f build test
npx --package=${packageName} ${primary} run lint
\`\`\`

Useful when you want a platform-pinned install (e.g. a locked-down CI image) without
pulling the facade and its sibling platform packages into the resolution.

## Contents

- ${binaries}: prebuilt native ${noun} under \`bin/\`.
- \`${PLATFORM_SHIM}\`: package-relative launcher backing the \`${primary}\` bin.
- No dependencies, no install scripts, no network access.

## More

- Main package: <https://npm.im/${matrix.facade}>
- Documentation, source, and issue tracker: linked from the main package.

Released under the same license as \`${matrix.facade}\` (see \`LICENSE\`).
`;
}

/**
 * Extracts specified binary files from a gzip-compressed tarball.
 *
 * @param tarballPath - Filesystem path to the .tar.gz archive to read
 * @param binaryNames - Basename(s) of binaries to extract; both each name and the same name with `.exe` are considered
 * @returns A Map whose keys are the extracted filenames (e.g. `tool` or `tool.exe`)
 *   and whose values are the file contents as `Buffer`
 */
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

/**
 * Parses an uncompressed tar archive buffer and returns metadata for each regular entry.
 *
 * The returned entries describe each file's path, size, type flag, and the byte offset
 * where the file body begins inside the provided buffer. Parsing stops at the tar end
 * marker (a zero block).
 *
 * @param tar - A Buffer containing an uncompressed tar archive.
 * @returns An array of `TarEntry` objects describing each entry found in `tar`.
 * @throws Error if an entry's declared size extends past the end of the buffer.
 */
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

/**
 * Extracts the file path stored in a tar header's name and prefix fields.
 *
 * @param header - A 512-byte TAR header buffer containing the `name` and `prefix` fields.
 * @returns The file path formed as `prefix/name` when a prefix is present, otherwise `name`.
 */
function readTarPath(header: Buffer): string {
	const name = readTarString(header, 0, 100);
	const prefix = readTarString(header, 345, 155);

	return prefix ? `${prefix}/${name}` : name;
}

/**
 * Parse the file size from a tar header buffer.
 *
 * @param header - The 512-byte tar header buffer containing the size field at byte offset 124
 * @returns The file size in bytes as encoded in the header's octal size field
 * @throws If the size field is present but not a valid octal integer
 */
function readTarSize(header: Buffer): number {
	const raw = readTarString(header, 124, 12).trim();

	if (!raw) return 0;

	const size = Number.parseInt(raw, 8);

	if (!Number.isFinite(size)) {
		throw new Error(`malformed tar archive: invalid size ${JSON.stringify(raw)}`);
	}

	return size;
}

/**
 * Reads a null-terminated UTF-8 string from a buffer slice (commonly a tar header field).
 *
 * @param buffer - The buffer containing the bytes to read from.
 * @param start - The start offset (inclusive) within `buffer`.
 * @param length - The maximum number of bytes to read starting at `start`.
 * @returns The UTF-8 decoded string formed by bytes from `start` up to the first NUL byte or `start + length`.
 */
function readTarString(buffer: Buffer, start: number, length: number): string {
	const bytes = buffer.subarray(start, start + length);
	const end = bytes.indexOf(0);
	const slice = end === -1 ? bytes : bytes.subarray(0, end);

	return slice.toString("utf8");
}

/**
 * Determines whether a tar block consists entirely of zero bytes.
 *
 * @returns `true` if every byte in `block` is `0`, `false` otherwise.
 */
function isZeroBlock(block: Buffer): boolean {
	return block.every((byte) => byte === 0);
}

/**
 * Determines whether a tar header type flag represents a regular file.
 *
 * @param type - The tar header type flag (single-character string from the header)
 * @returns `true` if `type` is `"0"` or `"\0"`, `false` otherwise
 */
function isRegularTarFile(type: string): boolean {
	return type === "0" || type === "\0";
}

/**
 * Round a byte length up to the next 512-byte tar block boundary.
 *
 * @param size - The length in bytes to align
 * @returns The smallest multiple of `BLOCK_SIZE` (512) that is greater than or equal to `size`
 */
function alignToBlock(size: number): number {
	return Math.ceil(size / BLOCK_SIZE) * BLOCK_SIZE;
}

/**
 * Constructs the filesystem path to a runner tarball for a given release version and Rust target.
 *
 * @param downloadsDir - Base directory where tarballs are stored
 * @param version - Release version (may include a leading `v` or not)
 * @param target - Target descriptor whose `rust` triple is used in the filename
 * @returns The full path to the expected `runner-<tag>-<rust>.tar.gz` tarball
 */
function tarballPath(downloadsDir: string, version: string, target: Target): string {
	const tag = version.startsWith("v") ? version : `v${version}`;
	return join(downloadsDir, `runner-${tag}-${target.rust}.tar.gz`);
}

/**
 * Copy specified files from one directory to another, preserving their relative paths.
 *
 * @param fromDir - Source directory that each entry in `files` is relative to
 * @param toDir - Destination directory where each file will be written, retaining the same relative path
 * @param files - List of file paths (relative to `fromDir`) to copy into `toDir`
 */
async function copyFiles(
	fromDir: string,
	toDir: string,
	files: readonly string[],
): Promise<void> {
	for (const file of files) {
		await cp(join(fromDir, file), join(toDir, file));
	}
}

/**
 * Write a value to disk as pretty-printed JSON.
 *
 * The file is encoded with 2-space indentation and ends with a single trailing newline.
 */
async function writeJson(path: string, value: unknown): Promise<void> {
	await writeFile(path, `${JSON.stringify(value, null, 2)}\n`);
}

/**
 * Recursively removes the file or directory at the given filesystem path.
 *
 * Performs a forceful, recursive deletion and does nothing if the path does not exist.
 *
 * @param path - Filesystem path to remove (file or directory)
 */
async function removePartialPackage(path: string): Promise<void> {
	await rm(path, { recursive: true, force: true });
}

/**
 * Format an npm package identifier with ANSI colors and underline for terminal display.
 *
 * @param scope - Optional npm scope (include the leading `@`, or `undefined` for an unscoped package)
 * @param name - The package name
 * @param version - The package version string
 * @returns A single string containing the (optionally scoped) package name
 *   ollowed by `@version`, styled with ANSI color and underline codes for terminal output
 */
function formatPackage(scope: string | undefined, name: string, version: string): string {
	const packageName = scope ? `${scope}/${name}` : name;
	const formattedName = `${ansi.blue}${ansi.underline}${packageName}${ansi.reset}`;
	const formattedVersion = `${ansi.green}@${ansi.reset}${ansi.purple}${ansi.italic}${version}${ansi.reset}`;

	return `${formattedName}${formattedVersion}`;
}

/**
 * Builds per-platform npm packages and a facade package under `npm/dist/` using
 * the crate's Cargo manifest and the build matrix from `npm/targets.json`.
 *
 * Reads Cargo metadata, CLI options, and the target matrix; cleans the dist
 * directory; builds each requested platform package (respecting `--only` and
 * `--skip-missing` behavior); and finally writes the facade package that
 * references the successfully built platform packages.
 *
 * @throws Error if no platform packages were built (prevents publishing a facade with empty optionalDependencies)
 */
async function main(): Promise<void> {
	const manifest = readCargoManifest();
	const opts = readOptions(manifest.version);
	const meta = packageMetadata(manifest);
	const matrix = await readMatrix();

	await cleanDist();

	const builtTargets: Target[] = [];

	for (const target of matrix.targets) {
		if (opts.only && !opts.only.has(target.pkg)) continue;

		const built = await withLogGroup(
			`${matrix.scope}/${target.pkg}`,
			() => buildPlatformPackage(matrix, target, opts, meta),
		);
		if (built) builtTargets.push(built);
	}

	if (builtTargets.length === 0) {
		throw new Error(
			"no platform packages were built; refusing to publish a facade with empty optionalDependencies",
		);
	}

	await withLogGroup(
		matrix.facade,
		() => buildFacade(matrix, opts.version, builtTargets, meta, opts.manDir),
	);
}

if (import.meta.main) {
	main().catch((error) => {
		const trace = error instanceof Error ? error.stack ?? error.message : String(error);
		stderr.write(`build-packages: ${trace}\n`);
		exit(1);
	});
}
