// @ts-check
import { spawnSync } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { appendFileSync, chmodSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { EOL } from "node:os";
import { join } from "node:path";
import { arch, env, exit, platform, stdout } from "node:process";

const REGISTRY = env.RUNNER_NPM_REGISTRY || "https://registry.npmjs.org";

/**
 * @param {"GITHUB_PATH" | "GITHUB_OUTPUT"} name
 * @param {string} block
 */
function fileCommand(name, block) {
	const file = env[name];
	if (!file) throw new Error(`${name} is not set, not running inside a GitHub Action?`);
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
		throw new Error("output delimiter collision (astronomically unlikely, retry)");
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
 * @returns {import("node:child_process").SpawnSyncReturns<string>}
 */
function run(file, args, stdio) {
	const res = spawnSync(file, args, { encoding: "utf8", stdio });
	if (res.error) throw res.error;
	if (res.status !== 0) {
		throw new Error(`\`${file} ${args.join(" ")}\` exited with ${res.status ?? "signal"}`);
	}
	return res;
}

/**
 * @template T
 * @param {() => Promise<T>} fn
 * @param {number[]} backoffsMs
 * @returns {Promise<T>}
 */
async function withRetry(fn, backoffsMs) {
	for (let attempt = 0;; attempt++) {
		try {
			return await fn();
		} catch (err) {
			if (attempt >= backoffsMs.length) throw err;
			const wait = backoffsMs[attempt];
			const msg = err instanceof Error ? err.message : String(err);
			debug(`attempt ${attempt + 1} failed (${msg}), retrying in ${wait}ms`);
			await new Promise((resolve) => setTimeout(resolve, wait));
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
		warn(`'${requested}' is not a semver pin or 'latest', falling back to 'latest'`);
		return "latest";
	}
	console.log(`version: ${m[1]} (from '${requested}')`);
	return m[1];
}

/**
 * Resolve the `@runner-run/<pkg>` platform package matching this runner.
 * Returns null on any unexpected, unmapped platform, unreadable manifest, or
 * undetectable libc.
 * @returns {{ scope: string, pkg: string } | null}
 */
function resolvePlatformTarget() {
	/** @type {{ scope: string, targets: { pkg: string, os: string[], cpu: string[], libc?: string[] | null }[] }} */
	let manifest;
	try {
		manifest = JSON.parse(readFileSync(join(import.meta.dirname, "npm", "targets.json"), "utf8"));
	} catch (err) {
		debug(`could not read npm/targets.json (${err instanceof Error ? err.message : String(err)})`);
		return null;
	}

	/** @type {"glibc" | "musl" | undefined} */
	let libc;
	if (platform === "linux") {
		try {
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
		debug(`no npm/targets.json entry for ${platform}/${arch}${libc ? `/${libc}` : ""}`);
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
 * @param {string} a
 * @param {string} b
 * @returns {number}
 */
function compareVersions(a, b) {
	const pa = a.split(/[.+-]/, 3).map(Number);
	const pb = b.split(/[.+-]/, 3).map(Number);
	for (let i = 0; i < 3; i++) {
		if (pa[i] !== pb[i]) return (pa[i] || 0) - (pb[i] || 0);
	}
	const preA = a.includes("-");
	const preB = b.includes("-");
	if (preA !== preB) return preA ? -1 : 1;
	return a < b ? -1 : a > b ? 1 : 0;
}

/**
 * @param {string} url
 * @returns {Promise<unknown>}
 */
async function getJson(url) {
	const res = await fetch(url);
	if (!res.ok) throw new Error(`GET ${url} responded ${res.status}`);
	return res.json();
}

/**
 * Resolve a version spec to the concrete npm dist for the platform package.
 * @param {string} pkgName
 * @param {string} spec
 * @returns {Promise<{ version: string, tarball: string, integrity: string | undefined }>}
 */
async function resolveDist(pkgName, spec) {
	if (spec === "latest" || isExactPin(spec)) {
		const manifest = /** @type {{ version: string, dist: { tarball: string, integrity?: string } }} */ (
			await withRetry(() => getJson(`${REGISTRY}/${pkgName}/${encodeURIComponent(spec)}`), [1000, 3000])
		);
		return { version: manifest.version, tarball: manifest.dist.tarball, integrity: manifest.dist.integrity };
	}
	const doc = /** @type {{ versions: Record<string, { dist: { tarball: string, integrity?: string } }> }} */ (
		await withRetry(() => getJson(`${REGISTRY}/${pkgName}`), [1000, 3000])
	);
	const prefix = `${spec}.`;
	const matches = Object.keys(doc.versions ?? {}).filter((v) => v === spec || v.startsWith(prefix));
	if (matches.length === 0) throw new Error(`no ${pkgName} version matching '${spec}'`);
	matches.sort(compareVersions);
	const version = matches[matches.length - 1];
	const dist = doc.versions[version].dist;
	return { version, tarball: dist.tarball, integrity: dist.integrity };
}

/**
 * @param {Buffer} buf
 * @param {string | undefined} integrity
 * @param {string} label
 */
function verifyIntegrity(buf, integrity, label) {
	if (!integrity) {
		warn(`no integrity metadata for ${label}, skipping checksum`);
		return;
	}
	const dash = integrity.indexOf("-");
	const algo = integrity.slice(0, dash);
	const expected = integrity.slice(dash + 1);
	const actual = createHash(algo).update(buf).digest("base64");
	if (actual !== expected) {
		throw new Error(`integrity mismatch for ${label}: expected ${algo}-${expected}, got ${algo}-${actual}`);
	}
}

/**
 * @param {string} tarball
 * @param {string | undefined} integrity
 * @param {string} label
 * @param {string} binDir
 */
async function downloadExtract(tarball, integrity, label, binDir) {
	startGroup(`download ${tarball}`);
	try {
		const buf = await withRetry(async () => {
			const res = await fetch(tarball);
			if (!res.ok) throw new Error(`GET ${tarball} responded ${res.status}`);
			return Buffer.from(await res.arrayBuffer());
		}, [1000, 3000]);
		verifyIntegrity(buf, integrity, label);
		mkdirSync(binDir, { recursive: true });
		const tgz = join(binDir, ".pkg.tgz");
		writeFileSync(tgz, buf);
		run("tar", ["-xzf", tgz, "-C", binDir, "--strip-components=2", "package/bin"], "inherit");
		rmSync(tgz, { force: true });
		if (platform !== "win32") {
			for (const b of ["runner", "run"]) chmodSync(join(binDir, b), 0o755);
		}
	} finally {
		endGroup();
	}
}

/**
 * @param {string} binDir
 * @returns {string}
 */
function verifyVersion(binDir) {
	const runner = join(binDir, platform === "win32" ? "runner.exe" : "runner");
	const res = run(runner, ["--version"], "pipe");
	const out = `${res.stdout ?? ""}${res.stderr ?? ""}`;
	stdout.write(out.endsWith("\n") ? out : `${out}${EOL}`);
	const m = /\b(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)\b/.exec(out);
	if (!m) throw new Error(`could not parse version from \`${runner} --version\`: ${out.trim()}`);
	return m[1];
}

/** @returns {string} */
function toolCacheRoot() {
	const toolCache = env.RUNNER_TOOL_CACHE;
	if (!toolCache) throw new Error("RUNNER_TOOL_CACHE is not set, not running inside a GitHub Action?");
	return toolCache;
}

/**
 * @param {string} binDir
 * @param {boolean} cacheHit
 * @param {string} label
 * @param {string} spec
 */
function finish(binDir, cacheHit, label, spec) {
	const verified = verifyVersion(binDir);
	if (isExactPin(spec) && verified !== spec) {
		throw new Error(`requested ${label}@${spec} but runner --version reported ${verified}`);
	}
	const suffix = platform === "win32" ? ".exe" : "";
	const runnerBin = join(binDir, `runner${suffix}`);
	const runBin = join(binDir, `run${suffix}`);
	addPath(binDir);
	console.log(`version: ${verified}`);
	console.log(`bin-dir: ${binDir}`);
	console.log(`runner-bin: ${runnerBin}`);
	console.log(`run-bin: ${runBin}`);
	console.log(`cache-hit: ${cacheHit}`);
	setOutput("version", verified);
	setOutput("bin-dir", binDir);
	setOutput("runner-bin", runnerBin);
	setOutput("run-bin", runBin);
	setOutput("cache-hit", String(cacheHit));
}

async function main() {
	const spec = resolveSpec();
	const target = resolvePlatformTarget();
	if (!target) throw new Error(`no prebuilt runner binary for ${platform}/${arch}`);
	const label = `${target.scope}/${target.pkg}`;
	const cache = (env.INPUT_CACHE ?? "true") !== "false";
	const exe = platform === "win32" ? "runner.exe" : "runner";
	const root = toolCacheRoot();

	// Exact pin already in the tool cache: skip the registry entirely.
	if (cache && isExactPin(spec)) {
		const binDir = join(root, "runner-cli", spec, target.pkg);
		if (existsSync(join(binDir, exe))) {
			console.log(`cache hit: ${label}@${spec}`);
			finish(binDir, true, label, spec);
			return;
		}
	}

	const dist = await resolveDist(label, spec);
	const binDir = join(root, "runner-cli", dist.version, target.pkg);
	let cacheHit = false;
	if (cache && existsSync(join(binDir, exe))) {
		console.log(`cache hit: ${label}@${dist.version}`);
		cacheHit = true;
	} else {
		await downloadExtract(dist.tarball, dist.integrity, `${label}@${dist.version}`, binDir);
	}
	finish(binDir, cacheHit, label, spec);
}

try {
	await main();
} catch (err) {
	const msg = err instanceof Error ? err.message : String(err);
	stdout.write(`::error::${escapeData(msg)}${EOL}`);
	exit(1);
}
