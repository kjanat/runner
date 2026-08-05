/// <reference types="node" />
"use strict";

const { optionalDependencies, name: pkgName } = require("#pkg");
const { platform, arch, env, report } = require("node:process");
const { dirname, join } = require("node:path");
const { existsSync, readdirSync } = require("node:fs");

const repo = "https://github.com/kjanat/runner";
const subPackages = Object.keys(optionalDependencies || {});

// ansispeck handles color and OSC 8 hyperlink capability detection (NO_COLOR,
// TTY, terminal support). It is ESM-only and require(esm) needs Node >= 20.19,
// while this package's engines contract is node >= 18: on Node 18 and Node 20
// before 20.19 (or when the install skipped the dependency) the require throws
// and diagnostics degrade to plain text without colors or OSC 8 links.
const { red, yellow, cyan, link, space } = (() => {
	try {
		return require("ansispeck");
	} catch {
		/** @param {unknown} value */
		const plain = (value) => String(value);
		return { red: plain, yellow: plain, cyan: plain, link: plain, space: () => "  " };
	}
})();

/**
 * A libc implementation, named the way `npm/targets.json` names it.
 *
 * @typedef {"glibc" | "musl"} Libc
 */

/**
 * Node `arch` → the arch token musl puts in `/lib/ld-musl-<token>.so.1`.
 *
 * @type {Record<string, string>}
 */
const MUSL_LOADER_ARCH = {
	arm: "armhf",
	arm64: "aarch64",
	ia32: "i386",
	ppc64: "powerpc64le",
	riscv64: "riscv64",
	s390x: "s390x",
	x64: "x86_64",
};

/**
 * Node `arch` → loader/libc paths that only exist on a glibc root filesystem.
 *
 * @type {Record<string, string[]>}
 */
const GLIBC_ARCH_PATHS = {
	arm: ["/lib/ld-linux-armhf.so.3", "/lib/arm-linux-gnueabihf/libc.so.6"],
	arm64: ["/lib/ld-linux-aarch64.so.1", "/lib/aarch64-linux-gnu/libc.so.6"],
	ia32: ["/lib/ld-linux.so.2", "/lib/i386-linux-gnu/libc.so.6"],
	x64: ["/lib64/ld-linux-x86-64.so.2", "/lib/x86_64-linux-gnu/libc.so.6"],
};

/** Arch-independent glibc markers, checked after the arch-specific ones. */
const GLIBC_PATHS = ["/lib/libc.so.6", "/lib64/libc.so.6", "/usr/lib/libc.so.6"];

/** Directories scanned for a musl loader when the arch token is unknown. */
const LIB_DIRS = ["/lib", "/usr/lib"];

/**
 * `existsSync` that never throws; a locked-down root can raise on `/lib64`.
 *
 * @param {string} path
 * @returns {boolean}
 */
const exists = (path) => {
	try {
		return existsSync(path);
	} catch {
		return false;
	}
};

/**
 * `readdirSync` that reports an unreadable or absent directory as empty.
 *
 * @param {string} dir
 * @returns {string[]}
 */
const listDir = (dir) => {
	try {
		return readdirSync(dir);
	} catch {
		return [];
	}
};

/**
 * Name of the platform package holding the `libc` build for this architecture.
 *
 * The scope and the `<os>-<arch>-<libc>` convention are the contract; the name
 * is derived rather than looked up so the resolver never depends on the order
 * `optionalDependencies` happens to be generated in.
 *
 * @param {string} scope - npm scope of the platform packages, e.g. `@runner-run`.
 * @param {string} arch - Node `arch` string, e.g. `x64`.
 * @param {Libc} libc
 * @returns {string}
 */
const linuxPackage = (scope, arch, libc) => `${scope}/linux-${arch}-${libc === "musl" ? "musl" : "gnu"}`;

/**
 * The scope shared by the declared platform packages.
 *
 * @param {readonly string[]} packages
 * @returns {string | null} The scope including its `@`, or `null` when the
 *   packages are unscoped and no name can be derived.
 */
function scopeOf(packages) {
	for (const pkg of packages) {
		const slash = pkg.indexOf("/");
		if (pkg.startsWith("@") && slash > 1) return pkg.slice(0, slash);
	}
	return null;
}

/**
 * Read the runtime glibc version out of `process.report`, when that API exists
 * and is complete. Absent on musl builds of Node, and on runtimes whose Node
 * compatibility layer does not implement diagnostic reports at all.
 *
 * `getReport()` otherwise enumerates network interfaces and can issue reverse
 * DNS lookups for them, which is unbounded latency on the launch path for a
 * field we do not read. `excludeNetwork` suppresses that; it is restored only
 * when it was a boolean to begin with, because the setter rejects `undefined`
 * and a blind restore would throw wherever the property does not exist.
 *
 * @returns {string | null}
 */
function glibcVersionFromReport() {
	// `excludeNetwork` predates its @types/node declaration and is missing
	// entirely on runtimes that only approximate `process.report`.
	const api = /** @type {{ excludeNetwork?: boolean, getReport?: () => unknown } | undefined} */ (report);
	const previous = api?.excludeNetwork;
	try {
		if (api && typeof previous === "boolean") api.excludeNetwork = true;
		const raw = api?.getReport?.();
		const header = /** @type {{ header?: { glibcVersionRuntime?: unknown } } | undefined} */ (raw)?.header;
		const version = header?.glibcVersionRuntime;
		return typeof version === "string" && version.length > 0 ? version : null;
	} catch {
		return null;
	} finally {
		if (api && typeof previous === "boolean") api.excludeNetwork = previous;
	}
}

/**
 * Normalize a user-supplied libc name.
 *
 * @param {string | undefined} value
 * @returns {Libc | null}
 */
function normalizeLibc(value) {
	switch (String(value ?? "").trim().toLowerCase()) {
		case "musl":
			return "musl";
		case "gnu":
		case "glibc":
			return "glibc";
		default:
			return null;
	}
}

/**
 * First path in `paths` that exists.
 *
 * @param {readonly string[]} paths
 * @param {(path: string) => boolean} fileExists
 * @returns {string | null}
 */
function firstExisting(paths, fileExists) {
	for (const path of paths) {
		if (fileExists(path)) return path;
	}
	return null;
}

/**
 * Locate a musl loader by scanning the standard library directories, for the
 * architectures `MUSL_LOADER_ARCH` does not name.
 *
 * @param {(dir: string) => string[]} readDir
 * @returns {string | null}
 */
function findMuslLoader(readDir) {
	for (const dir of LIB_DIRS) {
		for (const entry of readDir(dir)) {
			if (entry.startsWith("ld-musl-") || entry.startsWith("libc.musl-")) return `${dir}/${entry}`;
		}
	}
	return null;
}

/**
 * The signal that identified a libc, or `null` when none was conclusive.
 *
 * @typedef {object} LibcDetection
 * @property {Libc | null} libc - Detected libc; `null` when undecided.
 * @property {string} source - The deciding signal, quoted in diagnostics.
 */

/**
 * Injection points for {@link detectLibc}; each defaults to the real host.
 *
 * @typedef {object} LibcSignals
 * @property {string} [arch] - Node `arch` string.
 * @property {Record<string, string | undefined>} [env] - Environment to read `RUNNER_LIBC` from.
 * @property {() => string | null} [glibcVersion] - Runtime glibc version, if the runtime reports one.
 * @property {(path: string) => boolean} [fileExists]
 * @property {(dir: string) => string[]} [readDir]
 */

/**
 * Detect the host libc from layered signals, most authoritative first.
 *
 * No single signal is trustworthy on its own: `process.report` is missing or
 * incomplete outside Node, and the filesystem markers only appear for the libc
 * that is actually installed. Each layer is therefore a positive proof, never
 * an inference from another layer's absence.
 *
 * 1. `RUNNER_LIBC`, the escape hatch for hosts that carry both libcs.
 * 2. `process.report`'s `glibcVersionRuntime`, present only on a glibc build.
 * 3. musl markers: `/etc/alpine-release`, then the musl loader under `/lib`.
 * 4. glibc markers: the arch's ELF interpreter, then `libc.so.6`.
 *
 * musl is checked before glibc because a musl host that adds a glibc shim can
 * run both, while a glibc host never grows a musl loader by accident.
 *
 * @param {LibcSignals} [signals]
 * @returns {LibcDetection}
 */
function detectLibc(signals = {}) {
	const {
		arch: hostArch = arch,
		env: hostEnv = env,
		glibcVersion = glibcVersionFromReport,
		fileExists = exists,
		readDir = listDir,
	} = signals;

	const forced = normalizeLibc(hostEnv.RUNNER_LIBC);
	if (forced) return { libc: forced, source: `RUNNER_LIBC=${hostEnv.RUNNER_LIBC}` };

	const glibc = glibcVersion();
	if (glibc) return { libc: "glibc", source: `process.report glibcVersionRuntime ${glibc}` };

	const muslArch = MUSL_LOADER_ARCH[hostArch];
	const muslPaths = ["/etc/alpine-release", ...(muslArch ? [`/lib/ld-musl-${muslArch}.so.1`] : [])];
	const musl = firstExisting(muslPaths, fileExists) ?? findMuslLoader(readDir);
	if (musl) return { libc: "musl", source: musl };

	const glibcPath = firstExisting([...(GLIBC_ARCH_PATHS[hostArch] ?? []), ...GLIBC_PATHS], fileExists);
	if (glibcPath) return { libc: "glibc", source: glibcPath };

	return { libc: null, source: "no conclusive libc signal" };
}

/**
 * Which platform packages to try, in which order, and which were ruled out.
 *
 * @typedef {object} Plan
 * @property {string[]} order - Packages to probe, best candidate first.
 * @property {string[]} rejected - Declared packages skipped as libc-incompatible.
 * @property {string[]} pair - Both libc variants declared for this host; empty when unpaired.
 * @property {string | null} expected - The libc-matched package name, when one could be derived.
 * @property {Libc | null} libc - Detected libc; `null` when undecided.
 * @property {string} libcSource - The deciding signal, quoted in diagnostics.
 */

/**
 * Order the declared platform packages for the current host.
 *
 * Only Linux targets that declare a GNU/musl pair for this architecture are
 * reordered, and only when both names are present in the manifest. A musl host
 * rejects the dynamically linked GNU sibling; a glibc host prefers GNU but
 * keeps the static musl build as a compatible fallback. Everything else —
 * macOS, Windows, Android, and single-variant Linux targets such as
 * `linux-armv7-gnueabihf` — keeps the declared order, so a package name is
 * never synthesized for a target that has no libc sibling.
 *
 * @param {object} context
 * @param {string} context.platform - Node `platform` string.
 * @param {string} context.arch - Node `arch` string.
 * @param {readonly string[]} context.packages - Declared platform packages, in manifest order.
 * @param {(signals?: LibcSignals) => LibcDetection} context.detect
 * @returns {Plan}
 */
function planCandidates({ platform, arch, packages, detect }) {
	const order = [...packages];
	/** @type {Plan} */
	const unpaired = { order, rejected: [], pair: [], expected: null, libc: null, libcSource: "" };

	const scope = scopeOf(order);
	if (platform !== "linux" || scope === null) return unpaired;

	const gnu = linuxPackage(scope, arch, "glibc");
	const musl = linuxPackage(scope, arch, "musl");
	if (!order.includes(gnu) || !order.includes(musl)) return unpaired;

	const pair = [gnu, musl];
	const { libc, source } = detect();
	// An undecided libc keeps the declared order rather than guessing; the
	// caller warns if that fallback is what ends up selecting one of the pair.
	if (libc === null) return { ...unpaired, pair, libcSource: source };

	const expected = libc === "musl" ? musl : gnu;
	const sibling = libc === "musl" ? gnu : musl;
	return {
		order: [expected, ...(libc === "glibc" ? [sibling] : []), ...order.filter((pkg) => !pair.includes(pkg))],
		rejected: libc === "musl" ? [sibling] : [],
		pair,
		expected,
		libc,
		libcSource: source,
	};
}

/**
 * Resolve `<pkg>/package.json` through Node's resolver.
 *
 * @param {string} pkg
 * @returns {string}
 */
const resolvePackageJson = (pkg) => require.resolve(`${pkg}/package.json`);

/**
 * Whether a package is present in the dependency tree at all.
 *
 * @param {string} pkg
 * @param {(pkg: string) => string} resolve
 * @returns {boolean}
 */
function isInstalled(pkg, resolve) {
	try {
		resolve(pkg);
		return true;
	} catch {
		return false;
	}
}

/**
 * Whether an error is the expected "this platform package was not installed"
 * miss rather than something worth reporting.
 *
 * @param {unknown} err
 * @returns {boolean}
 */
const isModuleNotFound = (err) =>
	typeof err === "object" && err !== null && "code" in err && err.code === "MODULE_NOT_FOUND";

/**
 * Report an installed GNU sibling that cannot run on a musl host, then throw.
 *
 * @param {object} info
 * @param {string} info.arch - Node `arch` string.
 * @param {string} info.libcSource - The signal that detected it.
 * @param {string} info.expected - The package that should have been installed.
 * @param {string[]} info.installed - Incompatible siblings found instead.
 * @param {string[]} info.errors - Why the expected package was unusable, when it resolved at all.
 * @returns {never}
 * @throws {Error} Always.
 */
function failMuslMismatch({ arch, libcSource, expected, installed, errors }) {
	const libc = "musl";
	const other = "glibc";
	const indent = space(2);
	// The expected package is usually absent, but it can also be present and
	// broken — a half-finished install leaves the package.json without the bin.
	// Saying "not installed" there sends people off to reinstall what they
	// already have, so report the reason the loop actually recorded.
	const broken = errors.length > 0;
	const why = broken ? `\n${indent}- ${errors.join(`\n${indent}- `)}` : ` — not installed`;
	// Reinstalling is the fix for a present-but-broken package; installing it
	// is the fix for an absent one. Leading with the wrong one wastes a round.
	const repair = broken
		? `reinstall so the platform packages are unpacked in full`
		: `install the matching package: ${cyan(`npm install ${expected}`)}`;

	console.error(`${red(pkgName)}: no usable ${yellow(libc)} binary for ${yellow(`linux-${arch}`)}.

Detected libc: ${yellow(libc)} (${libcSource})
Expected package: ${cyan(expected)}${why}
Installed instead: ${cyan(installed.join(", "))} — built for ${yellow(other)}

The ${yellow(other)} package was skipped deliberately: its ELF interpreter does not exist
on a ${yellow(libc)} host, so spawning it would fail with a bare ${cyan("ENOENT")} from inside
${cyan("child_process")} instead of the message you are reading now.

Package managers that honour the ${cyan("libc")} field (npm, pnpm, Yarn) install only the
matching variant. Bun and Deno currently install both, and a ${cyan("node_modules")} tree
copied from another machine can carry either one.

Workarounds:
${indent}- ${repair}
${indent}- prebuilt release binary: ${cyan("cargo binstall runner-run")}
${indent}- build from source: ${cyan("cargo install runner-run --locked")}
${indent}- if this host can genuinely run ${yellow(other)} binaries: ${cyan(`RUNNER_LIBC=${other}`)}
${indent}- file an issue: ${link(`${repo}/issues`)}
`);

	throw new Error(
		`No usable ${libc} binary for linux-${arch}; refusing to spawn the ${other} build (${installed.join(", ")}).`,
	);
}

/**
 * Report that nothing usable was installed, then throw.
 *
 * @param {object} info
 * @param {string} info.platform - Node `platform` string.
 * @param {string} info.arch - Node `arch` string.
 * @param {string[]} info.missing - Packages absent from the dependency tree.
 * @param {string[]} info.errors - Per-package resolution failures.
 * @returns {never}
 * @throws {Error} Always.
 */
function failUnresolved({ platform, arch, missing, errors }) {
	const details = [...errors];
	if (missing.length > 0) {
		details.push(`not installed (${missing.length}): ${missing.join(", ")}`);
	}
	const detail = details.length > 0
		? "\n\nDetails of attempted resolutions:\n  - " + details.join("\n  - ")
		: "";

	const indent = space(2);

	console.error(`${red(pkgName)}: no prebuilt binary found for ${yellow(`${platform}-${arch}`)}.

This usually means your package manager skipped ${cyan("optionalDependencies")}
(common with ${cyan("--no-optional")}, ${cyan("--omit=optional")}, or some Docker/CI setups).

Workarounds:
${indent}- reinstall without: ${cyan("--no-optional")} / ${cyan("--omit=optional")}
${indent}- bun + ${cyan("minimumReleaseAge")}: add the ${cyan("@runner-run/*")} platform packages (not just ${
		cyan(pkgName)
	}) to ${cyan("minimumReleaseAgeExcludes")}; a fresh release is otherwise age-gated
${indent}- prebuilt release binary: ${cyan("cargo binstall runner-run")}
${indent}- build from source: ${cyan("cargo install runner-run --locked")}
${indent}- file an issue if your platform is unsupported: ${link(`${repo}/issues`)}${detail}
`);

	throw new Error("No prebuilt binary found for the current platform and architecture.");
}

/**
 * Injection points for {@link resolveBinary}; each defaults to the real host.
 *
 * @typedef {object} ResolveContext
 * @property {string} [platform] - Node `platform` string.
 * @property {string} [arch] - Node `arch` string.
 * @property {readonly string[]} [packages] - Declared platform packages, in manifest order.
 * @property {(signals?: LibcSignals) => LibcDetection} [detect]
 * @property {(pkg: string) => string} [resolvePackageJson]
 * @property {(path: string) => boolean} [fileExists]
 */

/**
 * Locate the prebuilt executable matching the current platform, architecture,
 * and libc.
 *
 * Package presence is not proof of compatibility. On Linux targets that ship
 * both a GNU and a musl build, the package name for the detected libc is
 * derived and resolved directly. A musl host drops the incompatible GNU
 * sibling; a glibc host retains the static musl build as a fallback. If only
 * GNU is installed on musl, this fails here with a libc diagnostic rather than
 * letting `spawnSync` fail later with an unexplained `ENOENT`.
 *
 * @param {string} name - Base name of the executable (without platform-specific extension).
 * @param {ResolveContext} [context] - Host details to resolve against; defaults to this process.
 * @returns {string} The filesystem path to the resolved executable.
 * @throws {Error} If no compatible binary is installed for the current platform, architecture, and libc.
 */
function resolveBinary(name, context = {}) {
	const {
		platform: hostPlatform = platform,
		arch: hostArch = arch,
		packages = subPackages,
		detect = detectLibc,
		resolvePackageJson: resolve = resolvePackageJson,
		fileExists = exists,
	} = context;

	const exe = hostPlatform === "win32" ? `${name}.exe` : name;
	const plan = planCandidates({ platform: hostPlatform, arch: hostArch, packages, detect });

	/** @type {string[]} */
	const missing = [];
	/** @type {string[]} */
	const errors = [];
	for (const subPkg of plan.order) {
		let pkgJsonPath;
		try {
			pkgJsonPath = resolve(subPkg);
		} catch (err) {
			// MODULE_NOT_FOUND is the expected miss for every platform package
			// npm skipped; a require stack per package is pure noise. Only an
			// unexpected error deserves its message, and only the first line.
			if (isModuleNotFound(err)) {
				missing.push(subPkg);
			} else {
				errors.push(`${subPkg}: ${String(err instanceof Error ? err.message : err).split("\n")[0]}`);
			}
			continue;
		}
		const binPath = join(dirname(pkgJsonPath), "bin", exe);
		// `require.resolve` proves the package.json exists, not the binary.
		// Could mismatch if a user manually deletes the bin, or a partial
		// install half-succeeded. Prefer a clear error here over an opaque
		// `ENOENT` from `spawnSync` later in `launch.cjs`.
		if (!fileExists(binPath)) {
			errors.push(`${subPkg}: package present but bin missing at ${binPath}`);
			continue;
		}
		// Both libc variants are declared and nothing identified this host's.
		// Manifest order picked one; say so rather than let a coin flip pass
		// for a decision.
		if (plan.libc === null && plan.pair.includes(subPkg)) {
			console.error(
				`${yellow(pkgName)}: could not detect this host's libc (${plan.libcSource}); using ${cyan(subPkg)}. Set ${
					cyan("RUNNER_LIBC=glibc")
				} or ${cyan("RUNNER_LIBC=musl")} to choose.`,
			);
		}
		return binPath;
	}

	// The libc-matched package is unusable. If its sibling is sitting right
	// there, that mismatch is the real failure, not a skipped install.
	if (plan.expected !== null && plan.libc === "musl") {
		const installed = plan.rejected.filter((subPkg) => isInstalled(subPkg, resolve));
		if (installed.length > 0) {
			failMuslMismatch({
				arch: hostArch,
				libcSource: plan.libcSource,
				expected: plan.expected,
				installed,
				errors,
			});
		}
	}

	failUnresolved({ platform: hostPlatform, arch: hostArch, missing, errors });
}

module.exports = { resolveBinary, detectLibc, planCandidates };
