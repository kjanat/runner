// @ts-check
import { spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { appendFileSync, mkdirSync, readFileSync } from "node:fs";
import { EOL } from "node:os";
import { join } from "node:path";
import { arch, env, exit, platform, stdout } from "node:process";

/**
 * @param {"GITHUB_PATH" | "GITHUB_OUTPUT"} name
 * @param {string} block
 */
function fileCommand(name, block) {
	const file = env[name];
	if (!file) throw new Error(`${name} is not set — not running inside a GitHub Action?`);
	appendFileSync(file, `${block}${EOL}`);
}

/** @param {string} dir */
function addPath(dir) {
	fileCommand("GITHUB_PATH", dir);
}

/**
 * @param {string} name
 * @param {string} value
 */
function setOutput(name, value) {
	const delim = `ghadelimiter_${randomUUID()}`;
	if (name.includes(delim) || value.includes(delim)) {
		throw new Error("output delimiter collision (astronomically unlikely — retry)");
	}
	fileCommand("GITHUB_OUTPUT", `${name}<<${delim}${EOL}${value}${EOL}${delim}`);
}

/**
 * @param {string} s
 * @returns {string}
 */
function escapeData(s) {
	return s.replace(/%/g, "%25").replace(/\r/g, "%0D").replace(/\n/g, "%0A");
}

/** @param {string} title */
function startGroup(title) {
	stdout.write(`::group::${escapeData(title)}${EOL}`);
}

function endGroup() {
	stdout.write(`::endgroup::${EOL}`);
}

/** @param {string} msg */
function warn(msg) {
	stdout.write(`::warning::${escapeData(msg)}${EOL}`);
}

/** @param {string} msg */
function debug(msg) {
	stdout.write(`::debug::${escapeData(msg)}${EOL}`);
}

/**
 * @param {string} file
 * @param {string[]} args
 * @param {"inherit" | "pipe"} stdio
 * @param {boolean} [shell]
 * @returns {import("node:child_process").SpawnSyncReturns<string>}
 */
function run(file, args, stdio, shell = false) {
	const res = spawnSync(file, args, { encoding: "utf8", shell, stdio });
	if (res.error) throw res.error;
	if (res.status !== 0) {
		throw new Error(`\`${file} ${args.join(" ")}\` exited with ${res.status ?? "signal"}`);
	}
	return res;
}

/** @param {number} ms */
function sleep(ms) {
	Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

/**
 * @param {() => void} fn
 * @param {number[]} backoffsMs
 */
function withRetry(fn, backoffsMs) {
	for (let attempt = 0;; attempt++) {
		try {
			fn();
			return;
		} catch (err) {
			if (attempt >= backoffsMs.length) throw err;
			const wait = backoffsMs[attempt];
			const msg = err instanceof Error ? err.message : String(err);
			debug(`attempt ${attempt + 1} failed (${msg}) — retrying in ${wait}ms`);
			sleep(wait);
		}
	}
}

/** @returns {string} */
function resolveSpec() {
	const requested = env.INPUT_VERSION || "";
	if (requested === "" || requested === "latest") {
		console.log("version: latest");
		return "latest";
	}
	const m = /^v?(\d{1,9}(?:\.\d{1,9}){0,2}(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)$/
		.exec(requested);
	if (!m) {
		warn(`'${requested}' is not a semver pin or 'latest' — falling back to 'latest'`);
		return "latest";
	}
	console.log(`version: ${m[1]} (from '${requested}')`);
	return m[1];
}

/** @returns {string} */
function installPrefix() {
	const toolCache = env.RUNNER_TOOL_CACHE;
	if (!toolCache) throw new Error("RUNNER_TOOL_CACHE is not set — not running inside a GitHub Action?");
	const prefix = join(toolCache, "runner-cli");
	mkdirSync(prefix, { recursive: true });
	return prefix;
}

/**
 * Resolve the `@runner-run/<pkg>` platform package matching this runner,
 * so install can skip the facade's optionalDependencies resolution
 * (13 extra registry metadata fetches for packages that'll never be used).
 * Returns null on anything unexpected — unmapped platform, unreadable
 * manifest, undetectable libc — so the caller falls back to the facade,
 * which covers every platform npm's own optionalDependencies resolution
 * covers.
 * @returns {{ scope: string, pkg: string } | null}
 */
function resolvePlatformTarget() {
	/** @type {{ scope: string, targets: { pkg: string, os: string[], cpu: string[], libc?: string[] | null }[] }} */
	let manifest;
	try {
		manifest = JSON.parse(readFileSync(join(import.meta.dirname, "npm", "targets.json"), "utf8"));
	} catch (err) {
		debug(`could not read npm/targets.json (${err instanceof Error ? err.message : String(err)}) — using facade`);
		return null;
	}

	/** @type {"glibc" | "musl" | undefined} */
	let libc;
	if (platform === "linux") {
		try {
			// Node's own signal for glibc vs musl — the same mechanism npm's
			// optionalDependencies resolution relies on for the `libc` field.
			const report = /** @type {{ header?: { glibcVersionRuntime?: string } }} */ (process.report?.getReport?.());
			libc = report?.header?.glibcVersionRuntime ? "glibc" : "musl";
		} catch {
			libc = undefined;
		}
	}

	const match = manifest.targets.find((t) =>
		t.os.includes(platform)
		&& t.cpu.includes(arch)
		&& (t.libc == null || (libc !== undefined && t.libc.includes(libc)))
	);
	if (!match) {
		debug(`no npm/targets.json entry for ${platform}/${arch}${libc ? `/${libc}` : ""} — using facade`);
		return null;
	}
	return { scope: manifest.scope, pkg: match.pkg };
}

/**
 * @param {string} spec
 * @returns {boolean}
 */
function isExactPin(spec) {
	return /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(spec);
}

/**
 * @param {string} binDir
 * @returns {string}
 */
function verifyVersion(binDir) {
	const runner = platform === "win32" ? join(binDir, "runner.cmd") : join(binDir, "runner");
	const res = run(runner, ["--version"], "pipe", platform === "win32");
	const out = `${res.stdout ?? ""}${res.stderr ?? ""}`;
	stdout.write(out.endsWith("\n") ? out : `${out}${EOL}`);
	const m = /\b(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)\b/.exec(out);
	if (!m) throw new Error(`could not parse version from \`${runner} --version\`: ${out.trim()}`);
	return m[1];
}

/**
 * @param {string} pkgName
 * @param {string} spec
 * @param {string} prefix
 */
function installPackage(pkgName, spec, prefix) {
	startGroup(`npm install --global --ignore-scripts --prefix ${prefix} ${pkgName}@${spec}`);
	try {
		withRetry(
			() =>
				run(
					"npm",
					["install", "--global", "--ignore-scripts", "--prefix", prefix, `${pkgName}@${spec}`],
					"inherit",
					platform === "win32",
				),
			[2000, 4000],
		);
	} finally {
		endGroup();
	}
}

try {
	const spec = resolveSpec();
	const prefix = installPrefix();
	const binDir = platform === "win32" ? prefix : join(prefix, "bin");
	const facade = "runner-run";

	// Installing the platform package directly skips the facade's
	// optionalDependencies resolution (a registry metadata fetch per
	// sibling platform package that will never be used). Any failure —
	// including a genuinely unpublished experimental-platform version —
	// falls back to the facade, which is what every platform used before.
	let installedPkg = facade;
	const target = resolvePlatformTarget();
	if (target) {
		const fastPkg = `${target.scope}/${target.pkg}`;
		try {
			installPackage(fastPkg, spec, prefix);
			installedPkg = fastPkg;
		} catch (err) {
			const msg = err instanceof Error ? err.message : String(err);
			warn(`fast install of ${fastPkg}@${spec} failed (${msg}) — falling back to ${facade}`);
			installPackage(facade, spec, prefix);
		}
	} else {
		installPackage(facade, spec, prefix);
	}

	const version = verifyVersion(binDir);
	if (isExactPin(spec) && version !== spec) {
		throw new Error(`requested ${installedPkg}@${spec} but runner --version reported ${version}`);
	}

	addPath(binDir);
	console.log(`version: ${version}`);
	console.log(`bin-dir: ${binDir}`);

	setOutput("version", version);
	setOutput("bin-dir", binDir);
} catch (err) {
	const msg = err instanceof Error ? err.message : String(err);
	stdout.write(`::error::${escapeData(msg)}${EOL}`);
	exit(1);
}
