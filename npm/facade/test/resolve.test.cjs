/// <reference types="node" />
"use strict";

const { test } = require("node:test");
const assert = require("node:assert/strict");
const { mkdirSync, mkdtempSync, writeFileSync } = require("node:fs");
const { tmpdir } = require("node:os");
const { join } = require("node:path");

const { resolveBinary, detectLibc } = require("#resolve");

/** @typedef {{ pkg: string, os: string[], cpu: string[], libc?: string[] }} Target */
const { scope, binaries, targets } = /** @type {{ scope: string, binaries: string[], targets: Target[] }} */ (
	require("../../targets.json")
);

/** Every platform package the facade declares, in generated manifest order. */
const declared = targets.map((target) => `${scope}/${target.pkg}`);

/** @param {Target} target */
const libcOf = (target) => target.libc?.[0] ?? null;

/**
 * The same-arch package built for the other libc, when the matrix ships one.
 * `linux-armv7-gnueabihf` has none, and must not have one invented for it.
 *
 * @param {Target} target
 * @returns {Target | undefined}
 */
const siblingOf = (target) =>
	targets.find((other) =>
		other !== target
		&& other.os[0] === target.os[0]
		&& other.cpu[0] === target.cpu[0]
		&& libcOf(other) === (libcOf(target) === "musl" ? "glibc" : "musl")
	);

/**
 * Materialize `packages` as a `node_modules`-shaped tree and return a resolver
 * over it that misses the way Node's does.
 *
 * @param {readonly string[]} packages
 * @param {readonly string[]} files - Binary names to create under each `bin/`.
 * @returns {{ root: string, resolvePackageJson: (pkg: string) => string }}
 */
function installFixture(packages, files) {
	const root = mkdtempSync(join(tmpdir(), "runner-resolve-"));
	for (const pkg of packages) {
		mkdirSync(join(root, pkg, "bin"), { recursive: true });
		writeFileSync(join(root, pkg, "package.json"), JSON.stringify({ name: pkg }));
		for (const file of files) writeFileSync(join(root, pkg, "bin", file), "");
	}
	return {
		root,
		resolvePackageJson: (pkg) => {
			if (packages.includes(pkg)) return join(root, pkg, "package.json");
			throw Object.assign(new Error(`Cannot find module '${pkg}'`), { code: "MODULE_NOT_FOUND" });
		},
	};
}

/**
 * The real detector with every signal stubbed, so no property of the machine
 * running the tests leaks into the result.
 *
 * @param {import("#resolve").LibcSignals} [signals]
 */
const detectWith = (signals = {}) => () =>
	detectLibc({ env: {}, glibcVersion: () => null, fileExists: () => false, readDir: () => [], ...signals });

/** @type {Record<string, () => import("#resolve").LibcDetection>} */
const HOSTS = {
	glibc: detectWith({ glibcVersion: () => "2.39" }),
	musl: detectWith({ fileExists: (path) => path === "/etc/alpine-release" }),
	undecided: detectWith(),
};

/** @param {() => unknown} fn */
function captureStderr(fn) {
	const original = console.error;
	/** @type {string[]} */
	const lines = [];
	console.error = (...args) => void lines.push(args.map(String).join(" "));
	try {
		fn();
		return { error: undefined, output: lines.join("\n") };
	} catch (error) {
		return { error, output: lines.join("\n") };
	} finally {
		console.error = original;
	}
}

// Every target in the matrix, against a host that matches it and an install
// carrying its libc sibling where one exists — the tree Bun and Deno produce.
// Asserting the exact package means no case can pass on manifest order.
for (const target of targets) {
	const sibling = siblingOf(target);
	const host = HOSTS[libcOf(target) ?? "undecided"];
	const files = binaries.map((name) => (target.os[0] === "win32" ? `${name}.exe` : name));

	test(`${target.pkg} resolves itself${sibling ? ` over ${sibling.pkg}` : ""}`, () => {
		const installed = [target, ...(sibling ? [sibling] : [])].map((each) => `${scope}/${each.pkg}`);
		const { root, resolvePackageJson } = installFixture(installed, files);
		const context = {
			platform: target.os[0],
			arch: target.cpu[0],
			packages: declared,
			detect: host,
			resolvePackageJson,
		};

		for (const [index, name] of binaries.entries()) {
			assert.equal(resolveBinary(name, context), join(root, `${scope}/${target.pkg}`, "bin", files[index]));
		}
	});
}

test("only the wrong-libc sibling installed fails without spawning it", () => {
	const { resolvePackageJson } = installFixture([`${scope}/linux-x64-gnu`], binaries);
	const context = { platform: "linux", arch: "x64", packages: declared, detect: HOSTS.musl, resolvePackageJson };

	const { error, output } = captureStderr(() => resolveBinary("run", context));

	assert.match(String(error), /No usable musl binary for linux-x64/);
	assert.match(output, new RegExp(`Expected package: ${scope}/linux-x64-musl — not installed`));
});

test("the matching package installed without its bin reports that, not a missing install", () => {
	const musl = installFixture([`${scope}/linux-x64-musl`], []);
	const gnu = installFixture([`${scope}/linux-x64-gnu`], binaries);
	const resolvePackageJson = (/** @type {string} */ pkg) =>
		pkg.endsWith("-musl") ? musl.resolvePackageJson(pkg) : gnu.resolvePackageJson(pkg);
	const context = { platform: "linux", arch: "x64", packages: declared, detect: HOSTS.musl, resolvePackageJson };

	const { error, output } = captureStderr(() => resolveBinary("run", context));

	assert.match(String(error), /refusing to spawn the glibc build/);
	assert.match(output, /package present but bin missing at/);
	assert.doesNotMatch(output, /not installed/);
});

test("an undecided libc keeps the declared order and says so", () => {
	const installed = [`${scope}/linux-x64-gnu`, `${scope}/linux-x64-musl`];
	const { root, resolvePackageJson } = installFixture(installed, binaries);
	const context = { platform: "linux", arch: "x64", packages: declared, detect: HOSTS.undecided, resolvePackageJson };

	const { output } = captureStderr(() =>
		assert.equal(resolveBinary("run", context), join(root, `${scope}/linux-x64-gnu`, "bin", "run"))
	);

	assert.match(output, /could not detect this host's libc/);
});

// Each layer is a positive proof; none may be inferred from another's absence.
test("libc detection layers", () => {
	/** @type {[string, import("#resolve").LibcSignals, string | null][]} */
	const cases = [
		["RUNNER_LIBC wins over everything", { env: { RUNNER_LIBC: "musl" }, glibcVersion: () => "2.39" }, "musl"],
		["gnu is accepted for glibc", { env: { RUNNER_LIBC: "gnu" } }, "glibc"],
		["an unknown override falls through", { env: { RUNNER_LIBC: "uclibc" }, glibcVersion: () => "2.39" }, "glibc"],
		["process.report reports glibc", { glibcVersion: () => "2.39" }, "glibc"],
		["alpine marks musl", { fileExists: (p) => p === "/etc/alpine-release" }, "musl"],
		["the musl loader marks musl", { fileExists: (p) => p === "/lib/ld-musl-x86_64.so.1" }, "musl"],
		["a scanned musl loader marks musl", { readDir: (d) => (d === "/lib" ? ["ld-musl-x86_64.so.1"] : []) }, "musl"],
		["the glibc loader marks glibc", { fileExists: (p) => p === "/lib64/ld-linux-x86-64.so.2" }, "glibc"],
		["no signal stays undecided", {}, null],
	];

	for (const [name, signals, expected] of cases) {
		assert.equal(detectWith({ arch: "x64", ...signals })().libc, expected, name);
	}
});
