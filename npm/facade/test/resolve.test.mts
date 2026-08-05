// tsconfig pins `typeRoots`, so `@types/bun`'s own reference is not followed.
/// <reference types="bun" />
import { afterAll, expect, spyOn, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { strip } from "ansispeck";

import { detectLibc, resolveBinary } from "#resolve";
import type { LibcDetection, LibcSignals } from "#resolve";
import type { Matrix, Target } from "../../scripts/build-packages.ts";
import matrix from "../../targets.json" with { type: "json" };

const { scope, binaries, targets } = matrix as Matrix;
const fixtureRoots = new Set<string>();

afterAll(() => {
	for (const root of fixtureRoots) rmSync(root, { recursive: true, force: true });
	fixtureRoots.clear();
});

/** Every platform package the facade declares, in generated manifest order. */
const declared = targets.map((target) => `${scope}/${target.pkg}`);

const libcOf = (target: Target) => target.libc?.[0] ?? null;

/**
 * The same-arch package built for the other libc, when the matrix ships one.
 * `linux-armv7-gnueabihf` has none, and must not have one invented for it.
 */
const siblingOf = (target: Target): Target | undefined =>
	targets.find((other) =>
		other !== target
		&& other.os[0] === target.os[0]
		&& other.cpu[0] === target.cpu[0]
		&& libcOf(other) === (libcOf(target) === "musl" ? "glibc" : "musl")
	);

/**
 * Materialize `packages` as a `node_modules`-shaped tree and return a resolver
 * over it that misses the way Node's does.
 */
function installFixture(packages: readonly string[], files: readonly string[]) {
	const root = mkdtempSync(join(tmpdir(), "runner-resolve-"));
	fixtureRoots.add(root);
	for (const pkg of packages) {
		mkdirSync(join(root, pkg, "bin"), { recursive: true });
		writeFileSync(join(root, pkg, "package.json"), JSON.stringify({ name: pkg }));
		for (const file of files) writeFileSync(join(root, pkg, "bin", file), "");
	}
	return {
		root,
		resolvePackageJson: (pkg: string): string => {
			if (packages.includes(pkg)) return join(root, pkg, "package.json");
			throw Object.assign(new Error(`Cannot find module '${pkg}'`), { code: "MODULE_NOT_FOUND" });
		},
	};
}

/**
 * The real detector with every signal stubbed, so no property of the machine
 * running the tests leaks into the result.
 */
const detectWith = (signals: LibcSignals = {}) => (): LibcDetection =>
	detectLibc({ env: {}, glibcVersion: () => null, fileExists: () => false, readDir: () => [], ...signals });

const HOSTS: Record<string, () => LibcDetection> = {
	glibc: detectWith({ glibcVersion: () => "2.39" }),
	musl: detectWith({ fileExists: (path) => path === "/etc/alpine-release" }),
	undecided: detectWith(),
};

function captureStderr(fn: () => unknown): { error: unknown; output: string } {
	const spy = spyOn(console, "error").mockImplementation(() => {});
	const output = () => strip(spy.mock.calls.map((args) => args.map(String).join(" ")).join("\n"));
	try {
		fn();
		return { error: undefined, output: output() };
	} catch (error) {
		return { error, output: output() };
	} finally {
		spy.mockRestore();
	}
}

// Every target in the matrix, against a host that matches it and an install
// carrying its libc sibling where one exists — the tree Bun and Deno produce.
// Asserting the exact package means no case can pass on manifest order.
test.each(targets.map((target) => [target.pkg, target] as const))(
	"%s resolves the package built for its own platform",
	(_pkg, target) => {
		const sibling = siblingOf(target);
		const files = binaries.map((name) => (target.os[0] === "win32" ? `${name}.exe` : name));
		const installed = [target, ...(sibling ? [sibling] : [])].map((each) => `${scope}/${each.pkg}`);
		const { root, resolvePackageJson } = installFixture(installed, files);
		const context = {
			platform: target.os[0],
			arch: target.cpu[0],
			packages: declared,
			detect: HOSTS[libcOf(target) ?? "undecided"],
			resolvePackageJson,
		};

		for (const [index, name] of binaries.entries()) {
			expect(resolveBinary(name, context)).toBe(join(root, `${scope}/${target.pkg}`, "bin", files[index]!));
		}
	},
);

test("only the GNU sibling installed on musl fails without spawning it", () => {
	const { resolvePackageJson } = installFixture([`${scope}/linux-x64-gnu`], binaries);
	const context = { platform: "linux", arch: "x64", packages: declared, detect: HOSTS.musl, resolvePackageJson };

	const { error, output } = captureStderr(() => resolveBinary("run", context));

	expect(String(error)).toMatch(/No usable musl binary for linux-x64/);
	expect(output).toContain(`Expected package: ${scope}/linux-x64-musl — not installed`);
});

test("a glibc host falls back to the static musl package", () => {
	const musl = `${scope}/linux-x64-musl`;
	const { root, resolvePackageJson } = installFixture([musl], binaries);
	const context = { platform: "linux", arch: "x64", packages: declared, detect: HOSTS.glibc, resolvePackageJson };

	expect(resolveBinary("run", context)).toBe(join(root, musl, "bin", "run"));
});

test("the matching package installed without its bin reports that, not a missing install", () => {
	const musl = installFixture([`${scope}/linux-x64-musl`], []);
	const gnu = installFixture([`${scope}/linux-x64-gnu`], binaries);
	const resolvePackageJson = (pkg: string) =>
		pkg.endsWith("-musl") ? musl.resolvePackageJson(pkg) : gnu.resolvePackageJson(pkg);
	const context = { platform: "linux", arch: "x64", packages: declared, detect: HOSTS.musl, resolvePackageJson };

	const { error, output } = captureStderr(() => resolveBinary("run", context));

	expect(String(error)).toMatch(/refusing to spawn the glibc build/);
	expect(output).toContain("package present but bin missing at");
	expect(output).not.toContain("not installed");
});

test("an undecided libc keeps the declared order and says so", () => {
	const installed = [`${scope}/linux-x64-gnu`, `${scope}/linux-x64-musl`];
	const { root, resolvePackageJson } = installFixture(installed, binaries);
	const context = { platform: "linux", arch: "x64", packages: declared, detect: HOSTS.undecided, resolvePackageJson };

	const { output } = captureStderr(() =>
		expect(resolveBinary("run", context)).toBe(join(root, `${scope}/linux-x64-gnu`, "bin", "run"))
	);

	expect(output).toContain("could not detect this host's libc");
});

// Each layer is a positive proof; none may be inferred from another's absence.
const LAYERS: [string, LibcSignals, LibcDetection["libc"]][] = [
	["RUNNER_LIBC outranks every signal", { env: { RUNNER_LIBC: "musl" }, glibcVersion: () => "2.39" }, "musl"],
	["gnu is accepted for glibc", { env: { RUNNER_LIBC: "gnu" } }, "glibc"],
	["an unknown override falls through", { env: { RUNNER_LIBC: "uclibc" }, glibcVersion: () => "2.39" }, "glibc"],
	["process.report reports glibc", { glibcVersion: () => "2.39" }, "glibc"],
	["alpine marks musl", { fileExists: (p) => p === "/etc/alpine-release" }, "musl"],
	["the musl loader marks musl", { fileExists: (p) => p === "/lib/ld-musl-x86_64.so.1" }, "musl"],
	["a scanned musl loader marks musl", { readDir: (d) => (d === "/lib" ? ["ld-musl-x86_64.so.1"] : []) }, "musl"],
	["the glibc loader marks glibc", { fileExists: (p) => p === "/lib64/ld-linux-x86-64.so.2" }, "glibc"],
	["no signal stays undecided", {}, null],
];

test.each(LAYERS)("libc detection: %s", (_name, signals, expected) => {
	expect(detectWith({ arch: "x64", ...signals })().libc).toBe(expected);
});
