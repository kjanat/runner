/// <reference types="node" />
"use strict";

const { test } = require("node:test");
const assert = require("node:assert/strict");
const { mkdirSync, mkdtempSync, writeFileSync } = require("node:fs");
const { tmpdir } = require("node:os");
const { join } = require("node:path");

const { resolveBinary, detectLibc, planCandidates } = require("../lib/resolve.cjs");

const SCOPE = "@runner-run";

/**
 * The declared platform packages, in the order `build-packages.ts` generates
 * them from `npm/targets.json`. GNU deliberately precedes musl here: a test
 * that passes only because of this order is a test of the bug, not the fix.
 */
const DECLARED = [
	`${SCOPE}/linux-x64-gnu`,
	`${SCOPE}/linux-x64-musl`,
	`${SCOPE}/linux-arm64-gnu`,
	`${SCOPE}/linux-arm64-musl`,
	`${SCOPE}/android-arm64`,
	`${SCOPE}/linux-armv7-gnueabihf`,
	`${SCOPE}/darwin-x64`,
	`${SCOPE}/darwin-arm64`,
	`${SCOPE}/win32-x64-msvc`,
	`${SCOPE}/win32-arm64-msvc`,
	`${SCOPE}/freebsd-x64`,
];

/**
 * Build a `node_modules`-shaped fixture containing exactly `packages`, each
 * with a real `package.json` and real `bin/` entries, and return a resolver
 * over it that fails the way Node's does for anything else.
 *
 * @param {readonly string[]} packages - Packages to materialize on disk.
 * @param {readonly string[]} [binaries] - Binary file names to create in each package.
 * @returns {{ root: string, resolvePackageJson: (pkg: string) => string }}
 */
function installFixture(packages, binaries = ["run", "runner"]) {
	const root = mkdtempSync(join(tmpdir(), "runner-resolve-"));
	for (const pkg of packages) {
		const dir = join(root, pkg);
		mkdirSync(join(dir, "bin"), { recursive: true });
		writeFileSync(join(dir, "package.json"), JSON.stringify({ name: pkg, version: "0.0.0-test" }));
		for (const binary of binaries) writeFileSync(join(dir, "bin", binary), "#!/bin/sh\n", { mode: 0o755 });
	}

	return {
		root,
		resolvePackageJson(pkg) {
			if (!packages.includes(pkg)) {
				throw Object.assign(new Error(`Cannot find module '${pkg}/package.json'`), { code: "MODULE_NOT_FOUND" });
			}
			return join(root, pkg, "package.json");
		},
	};
}

/**
 * A `detect` implementation backed by the real layered detector, with every
 * signal stubbed out so nothing about the machine running the tests leaks in.
 *
 * @param {import("../lib/resolve.cjs").LibcSignals} [signals] - Signals to override.
 * @returns {() => import("../lib/resolve.cjs").LibcDetection}
 */
function detectWith(signals = {}) {
	return () =>
		detectLibc({ env: {}, glibcVersion: () => null, fileExists: () => false, readDir: () => [], ...signals });
}

/** A host whose only libc evidence is Alpine's release file. */
const MUSL_HOST = detectWith({ fileExists: (path) => path === "/etc/alpine-release" });

/** A host that reports a runtime glibc version, the way Node on glibc does. */
const GLIBC_HOST = detectWith({ glibcVersion: () => "2.39" });

/** SGR colors and OSC 8 hyperlinks, which ansispeck emits on a capable terminal. */
const ANSI = /\u001B\][^\u0007\u001B]*(?:\u0007|\u001B\\)|\u001B\[[0-9;]*[A-Za-z]/g;

/**
 * Run `fn` with `console.error` captured instead of printed. Escape sequences
 * are stripped so assertions read the message, not the terminal capabilities
 * of whoever ran the tests.
 *
 * @template T
 * @param {() => T} fn
 * @returns {{ result: T | undefined, error: unknown, output: string }}
 */
function captureStderr(fn) {
	const original = console.error;
	/** @type {string[]} */
	const lines = [];
	console.error = (...args) => void lines.push(args.map(String).join(" "));
	const output = () => lines.join("\n").replace(ANSI, "");
	try {
		return { result: fn(), error: undefined, output: output() };
	} catch (error) {
		return { result: undefined, error, output: output() };
	} finally {
		console.error = original;
	}
}

test("musl host prefers the musl package even when GNU is installed and declared first", () => {
	const { root, resolvePackageJson } = installFixture([`${SCOPE}/linux-x64-gnu`, `${SCOPE}/linux-x64-musl`]);
	const context = { platform: "linux", arch: "x64", packages: DECLARED, detect: MUSL_HOST, resolvePackageJson };

	for (const name of ["run", "runner"]) {
		assert.equal(resolveBinary(name, context), join(root, `${SCOPE}/linux-x64-musl`, "bin", name));
	}
});

test("musl host still prefers musl when the manifest declares musl first", () => {
	const { root, resolvePackageJson } = installFixture([`${SCOPE}/linux-x64-gnu`, `${SCOPE}/linux-x64-musl`]);
	const reversed = [...DECLARED].reverse();
	const context = { platform: "linux", arch: "x64", packages: reversed, detect: MUSL_HOST, resolvePackageJson };

	assert.equal(resolveBinary("run", context), join(root, `${SCOPE}/linux-x64-musl`, "bin", "run"));
});

test("glibc arm64 host prefers the GNU package when both are installed", () => {
	const { root, resolvePackageJson } = installFixture([`${SCOPE}/linux-arm64-gnu`, `${SCOPE}/linux-arm64-musl`]);
	const context = { platform: "linux", arch: "arm64", packages: DECLARED, detect: GLIBC_HOST, resolvePackageJson };

	for (const name of ["run", "runner"]) {
		assert.equal(resolveBinary(name, context), join(root, `${SCOPE}/linux-arm64-gnu`, "bin", name));
	}
});

test("glibc arm64 host prefers GNU even when musl is declared first", () => {
	const { root, resolvePackageJson } = installFixture([`${SCOPE}/linux-arm64-gnu`, `${SCOPE}/linux-arm64-musl`]);
	const muslFirst = [`${SCOPE}/linux-arm64-musl`, ...DECLARED.filter((pkg) => pkg !== `${SCOPE}/linux-arm64-musl`)];
	const context = { platform: "linux", arch: "arm64", packages: muslFirst, detect: GLIBC_HOST, resolvePackageJson };

	assert.equal(resolveBinary("run", context), join(root, `${SCOPE}/linux-arm64-gnu`, "bin", "run"));
});

test("musl host with only the GNU sibling installed fails with a libc diagnostic", () => {
	const { resolvePackageJson } = installFixture([`${SCOPE}/linux-x64-gnu`]);
	const context = { platform: "linux", arch: "x64", packages: DECLARED, detect: MUSL_HOST, resolvePackageJson };

	const { error, output } = captureStderr(() => resolveBinary("run", context));

	assert.ok(error instanceof Error, "expected resolveBinary to throw");
	assert.match(error.message, /No musl binary installed for linux-x64/);
	assert.match(error.message, /linux-x64-gnu/);
	assert.match(output, /Detected libc: musl \(\/etc\/alpine-release\)/);
	assert.match(output, new RegExp(`Expected package: ${SCOPE}/linux-x64-musl`));
	assert.doesNotMatch(output, /no prebuilt binary found/);
});

test("glibc host with only the musl sibling installed fails with a libc diagnostic", () => {
	const { resolvePackageJson } = installFixture([`${SCOPE}/linux-arm64-musl`]);
	const context = { platform: "linux", arch: "arm64", packages: DECLARED, detect: GLIBC_HOST, resolvePackageJson };

	const { error } = captureStderr(() => resolveBinary("runner", context));

	assert.ok(error instanceof Error, "expected resolveBinary to throw");
	assert.match(error.message, /No glibc binary installed for linux-arm64/);
	assert.match(error.message, /linux-arm64-musl/);
});

test("a matching package whose bin is missing does not fall through to the sibling", () => {
	// Half-finished install: the musl package is there, its bins are not.
	const { resolvePackageJson } = installFixture([`${SCOPE}/linux-x64-musl`], []);
	const gnu = installFixture([`${SCOPE}/linux-x64-gnu`]);
	/** @param {string} pkg */
	const resolve = (pkg) => (pkg.endsWith("-musl") ? resolvePackageJson(pkg) : gnu.resolvePackageJson(pkg));
	const context = {
		platform: "linux",
		arch: "x64",
		packages: DECLARED,
		detect: MUSL_HOST,
		resolvePackageJson: resolve,
	};

	const { error } = captureStderr(() => resolveBinary("run", context));

	assert.ok(error instanceof Error, "expected resolveBinary to throw");
	assert.match(error.message, /refusing to spawn the glibc build/);
});

test("neither variant installed keeps the generic optionalDependencies diagnostic", () => {
	const { resolvePackageJson } = installFixture([]);
	const context = { platform: "linux", arch: "x64", packages: DECLARED, detect: MUSL_HOST, resolvePackageJson };

	const { error, output } = captureStderr(() => resolveBinary("run", context));

	assert.ok(error instanceof Error, "expected resolveBinary to throw");
	assert.match(error.message, /No prebuilt binary found/);
	assert.match(output, /optionalDependencies/);
});

test("an undecided libc falls back to declared order and says so", () => {
	const { root, resolvePackageJson } = installFixture([`${SCOPE}/linux-x64-gnu`, `${SCOPE}/linux-x64-musl`]);
	const context = { platform: "linux", arch: "x64", packages: DECLARED, detect: detectWith(), resolvePackageJson };

	const { result, output } = captureStderr(() => resolveBinary("run", context));

	assert.equal(result, join(root, `${SCOPE}/linux-x64-gnu`, "bin", "run"));
	assert.match(output, /could not detect this host's libc/);
	assert.match(output, /RUNNER_LIBC/);
});

test("RUNNER_LIBC overrides detection", () => {
	const { root, resolvePackageJson } = installFixture([`${SCOPE}/linux-x64-gnu`, `${SCOPE}/linux-x64-musl`]);
	const forced = detectWith({ env: { RUNNER_LIBC: "musl" }, glibcVersion: () => "2.39" });
	const context = { platform: "linux", arch: "x64", packages: DECLARED, detect: forced, resolvePackageJson };

	assert.equal(resolveBinary("run", context), join(root, `${SCOPE}/linux-x64-musl`, "bin", "run"));
});

test("linux-armv7-gnueabihf resolves without synthesizing a musl sibling", () => {
	const { root, resolvePackageJson } = installFixture([`${SCOPE}/linux-armv7-gnueabihf`]);
	const context = { platform: "linux", arch: "arm", packages: DECLARED, detect: MUSL_HOST, resolvePackageJson };

	assert.equal(resolveBinary("run", context), join(root, `${SCOPE}/linux-armv7-gnueabihf`, "bin", "run"));

	const plan = planCandidates({ platform: "linux", arch: "arm", packages: DECLARED, detect: MUSL_HOST });
	assert.deepEqual(plan.pair, []);
	assert.deepEqual(plan.rejected, []);
	assert.deepEqual(plan.order, DECLARED);
});

test("non-Linux platforms keep the declared order untouched", () => {
	for (const [platform, arch, pkg] of [["darwin", "arm64", "darwin-arm64"], ["freebsd", "x64", "freebsd-x64"]]) {
		const { root, resolvePackageJson } = installFixture([`${SCOPE}/${pkg}`]);
		const context = { platform, arch, packages: DECLARED, detect: MUSL_HOST, resolvePackageJson };

		assert.equal(resolveBinary("run", context), join(root, `${SCOPE}/${pkg}`, "bin", "run"));

		const plan = planCandidates({ platform, arch, packages: DECLARED, detect: MUSL_HOST });
		assert.deepEqual(plan.order, DECLARED);
		assert.equal(plan.expected, null);
	}
});

test("win32 still resolves the .exe suffix", () => {
	const { root, resolvePackageJson } = installFixture([`${SCOPE}/win32-x64-msvc`], ["run.exe", "runner.exe"]);
	const context = { platform: "win32", arch: "x64", packages: DECLARED, detect: MUSL_HOST, resolvePackageJson };

	assert.equal(resolveBinary("run", context), join(root, `${SCOPE}/win32-x64-msvc`, "bin", "run.exe"));
});

test("unscoped platform packages are left in declared order", () => {
	const packages = ["linux-x64-gnu", "linux-x64-musl"];
	const plan = planCandidates({ platform: "linux", arch: "x64", packages, detect: MUSL_HOST });

	assert.deepEqual(plan.order, packages);
	assert.equal(plan.expected, null);
});

test("detectLibc reads a usable process.report glibc version first", () => {
	const detected = detectLibc({
		arch: "x64",
		env: {},
		glibcVersion: () => "2.39",
		fileExists: () => false,
		readDir: () => [],
	});

	assert.equal(detected.libc, "glibc");
	assert.match(detected.source, /glibcVersionRuntime 2\.39/);
});

test("detectLibc falls back to the filesystem when process.report is unusable", () => {
	// Bun/Deno (no diagnostic report) and musl builds of Node (report without a
	// glibcVersionRuntime field) both land here.
	const alpine = detectLibc({
		arch: "x64",
		env: {},
		glibcVersion: () => null,
		fileExists: (path) => path === "/etc/alpine-release",
		readDir: () => [],
	});
	assert.deepEqual(alpine, { libc: "musl", source: "/etc/alpine-release" });

	const loader = detectLibc({
		arch: "x64",
		env: {},
		glibcVersion: () => null,
		fileExists: (path) => path === "/lib/ld-musl-x86_64.so.1",
		readDir: () => [],
	});
	assert.deepEqual(loader, { libc: "musl", source: "/lib/ld-musl-x86_64.so.1" });

	const glibc = detectLibc({
		arch: "arm64",
		env: {},
		glibcVersion: () => null,
		fileExists: (path) => path === "/lib/ld-linux-aarch64.so.1",
		readDir: () => [],
	});
	assert.deepEqual(glibc, { libc: "glibc", source: "/lib/ld-linux-aarch64.so.1" });
});

test("detectLibc scans lib directories for architectures it has no loader name for", () => {
	const detected = detectLibc({
		arch: "loongarch64",
		env: {},
		glibcVersion: () => null,
		fileExists: () => false,
		readDir: (dir) => (dir === "/lib" ? ["libz.so.1", "ld-musl-loongarch64.so.1"] : []),
	});

	assert.deepEqual(detected, { libc: "musl", source: "/lib/ld-musl-loongarch64.so.1" });
});

test("detectLibc reports undecided rather than guessing", () => {
	const detected = detectLibc({
		arch: "x64",
		env: {},
		glibcVersion: () => null,
		fileExists: () => false,
		readDir: () => [],
	});

	assert.equal(detected.libc, null);
});

test("detectLibc honours RUNNER_LIBC over every other signal", () => {
	for (const [value, expected] of [["musl", "musl"], ["glibc", "glibc"], ["gnu", "glibc"], [" MUSL ", "musl"]]) {
		const detected = detectLibc({
			arch: "x64",
			env: { RUNNER_LIBC: value },
			glibcVersion: () => "2.39",
			fileExists: () => true,
			readDir: () => [],
		});
		assert.equal(detected.libc, expected, `RUNNER_LIBC=${value}`);
	}

	const ignored = detectLibc({
		arch: "x64",
		env: { RUNNER_LIBC: "uclibc" },
		glibcVersion: () => "2.39",
		fileExists: () => false,
		readDir: () => [],
	});
	assert.equal(ignored.libc, "glibc", "an unknown RUNNER_LIBC value falls through to detection");
});
