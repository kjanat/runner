// @ts-check
import { delimiter } from "node:path";
import { env } from "node:process";

/**
 * Setup runner — composite action body.
 *
 * Two phases, dispatched by the `PHASE` env var set in `action.yml`:
 *
 *   - `resolve`: pick version tag + rust triple, append the (not-yet-
 *                populated) install dir to `$GITHUB_PATH`, and return the
 *                resolved `ResolveMeta`.
 *   - `install`: download cargo-binstall then
 *                `cargo-binstall runner-run@<ver>` into the install dir.
 *
 * Each phase is a separate `actions/github-script@v9` step in `action.yml`,
 * which is what lets the `actions/cache` step sit between `resolve` and
 * `install`. The PATH append lives in `resolve` because `$GITHUB_PATH` only
 * affects *later* steps and tolerates a dir that doesn't exist yet — so the
 * `install` step can be skipped wholesale (`if: cache-hit != 'true'`) when
 * `actions/cache` already restored the binaries.
 *
 * @param {import('@actions/github-script').AsyncFunctionArguments} args
 */
export default async function run(args) {
	const phase = env.PHASE ?? "";
	switch (phase) {
		case "resolve":
			return resolve(args);
		case "install":
			return install(args);
		default:
			throw new Error(`unknown PHASE: ${phase || "(unset)"}`);
	}
}

/**
 * Everything `resolve` hands to `install`: returned from the `resolve`
 * script so github-script JSON-encodes it into `steps.resolve.outputs.result`.
 * `tag`/`triple`/`dest` are also read in `action.yml` via `fromJSON()`
 * (cache key, action outputs); `cbhome`/`ext` are JS-only.
 *
 * @typedef {object} ResolveMeta
 * @property {string} tag      Release tag, e.g. `v0.10.0`.
 * @property {string} triple   Rust target triple.
 * @property {string} dest     Install dir (also the cache path + `bin-dir`).
 * @property {string} cbhome   cargo-binstall's `CARGO_HOME`.
 * @property {string} ext      Executable suffix (`.exe` on Windows, else ``).
 * @property {string} cacheKey `actions/cache` key (JS-built, YAML-consumed).
 */

/**
 * The action's `inputs`, marshalled as one `INPUTS` env var via
 * `${{ toJSON(inputs) }}`. Composite actions get no automatic
 * `INPUT_<NAME>` vars, so this is how `resolve` reads them. Keys are the
 * input ids verbatim. Only `version`/`target` are consumed in JS;
 * `cache`/`github-token` are handled in YAML but listed for fidelity.
 *
 * @typedef {object} ActionInputs
 * @property {string} version
 * @property {string} target
 * @property {string} cache
 * @property {string} github-token
 */

/**
 * Parse the `INPUTS` env var (the JSON `${{ toJSON(inputs) }}` blob).
 *
 * @returns {ActionInputs}
 */
function actionInputs() {
	/** @type {ActionInputs} */
	const parsed = JSON.parse(required("INPUTS"));
	return parsed;
}

/* ────────────────────────────── resolve ──────────────────────────────── */

/**
 * Resolve the release tag + rust target triple and export them as step
 * outputs. Also pre-computes derived paths (`dest`, `cbhome`, `ext`) that
 * downstream steps need so the YAML never has to recompute them.
 *
 * Version sourcing order:
 *   1. `inputs.version` (from the `INPUTS` JSON) if it looks like semver.
 *   2. `github.action_ref` (passed via `ACTION_REF`) if it looks like semver
 *      — this is what makes `uses: kjanat/runner@v0.10.0` pin v0.10.0.
 *   3. Otherwise, the latest release via the REST API (no scraping, no
 *      pipe-to-curl, no SIGPIPE class of bug).
 *
 * Target sourcing order:
 *   1. `inputs.target` (from the `INPUTS` JSON) if non-empty.
 *   2. First match in `npm/targets.json` for the current `RUNNER_OS` /
 *      `RUNNER_ARCH` (preferring glibc when libc is set). Single source of
 *      truth shared with the release matrix.
 *
 * @param {import('@actions/github-script').AsyncFunctionArguments} args
 */
function resolve({ github, core }) {
	return core.group("runner-run | resolve version + target", async () => {
		const inputs = actionInputs();
		const tag = await resolveTag(github, core, inputs.version);
		const triple = await resolveTriple(core, inputs.target);

		const runnerOs = env.RUNNER_OS ?? "";
		const ext = runnerOs === "Windows" ? ".exe" : "";

		const toolCache = required("RUNNER_TOOL_CACHE");
		const runnerTemp = required("RUNNER_TEMP");
		const dest = `${toolCache}/runner-run/${tag}/${triple}`;
		const cbhome = `${runnerTemp}/cargo-binstall`;

		core.info(`tag: ${tag}`);
		core.info(`triple: ${triple}`);
		core.info(`dest: ${dest}`);

		// Safe here even though `dest` is empty until the cache restore /
		// install: `$GITHUB_PATH` only prepends for *subsequent* steps and a
		// nonexistent PATH entry is inert. Doing it here lets `install` be
		// skipped entirely on a cache hit.
		core.addPath(dest);

		// Returned to github-script, which JSON-encodes it into
		// `steps.resolve.outputs.result` (default result-encoding: json).
		/** @type {ResolveMeta} */
		const meta = { tag, triple, dest, cbhome, ext, cacheKey: `runner-run-${tag}-${triple}` };
		return meta;
	});
}

/**
 * Pick the release tag. Returns a string like `v0.10.0`.
 *
 * @param {import('@actions/github-script').AsyncFunctionArguments['github']} github
 * @param {import('@actions/github-script').AsyncFunctionArguments['core']} core
 * @param {string} version `inputs.version`.
 * @returns {Promise<string>}
 */
async function resolveTag(github, core, version) {
	const semver = /^v?\d+\.\d+\.\d+/;
	const requested = version || env.ACTION_REF || "";

	if (semver.test(requested)) {
		return `v${requested.replace(/^v/, "")}`;
	}

	core.info(`'${requested || "(empty)"}' is not semver — fetching latest release`);
	const { data } = await github.rest.repos.getLatestRelease({
		owner: "kjanat",
		repo: "runner",
	});

	if (!/^v\d/.test(data.tag_name)) {
		throw new Error(`failed to resolve latest runner-run release (got '${data.tag_name}')`);
	}
	return data.tag_name;
}

/**
 * Pick the rust target triple. Returns a string like
 * `x86_64-unknown-linux-gnu`.
 *
 * @param {import('@actions/github-script').AsyncFunctionArguments['core']} core
 * @param {string} target `inputs.target`.
 * @returns {Promise<string>}
 */
async function resolveTriple(core, target) {
	if (target) return target;

	const runnerOs = required("RUNNER_OS");
	const runnerArch = required("RUNNER_ARCH");
	const actionPath = required("GITHUB_ACTION_PATH");

	/** @type {Record<string, string>} */
	const osMap = { Linux: "linux", macOS: "darwin", Windows: "win32" };
	/** @type {Record<string, string>} */
	const archMap = { X64: "x64", ARM64: "arm64", ARM: "arm", X86: "ia32" };

	const nodeOs = osMap[runnerOs] ?? "";
	const nodeCpu = archMap[runnerArch] ?? "";

	const fs = await import("node:fs/promises");
	const raw = await fs.readFile(`${actionPath}/npm/targets.json`, "utf8");
	/** @type {{ targets: Array<{ os: string[]; cpu: string[]; libc?: string[] | null; rust: string }> }} */
	const data = JSON.parse(raw);

	const match = data.targets.find((t) =>
		t.os.includes(nodeOs)
		&& t.cpu.includes(nodeCpu)
		&& (t.libc == null || t.libc.includes("glibc"))
	);

	if (!match) {
		core.setFailed(
			`No prebuilt target in npm/targets.json for ${runnerOs}-${runnerArch}; pass an explicit \`target\` input.`,
		);
		throw new Error("target resolution failed");
	}
	return match.rust;
}

/* ────────────────────────────── install ──────────────────────────────── */

/**
 * Install cargo-binstall (installer pinned by commit SHA via `BINSTALL_SHA`;
 * the release it downloads pinned by `BINSTALL_VERSION`, which the upstream
 * script reads from the inherited env), then use it to install
 * `runner-run@<tag>` into `meta.dest`. Only runs on a cache miss — the
 * step-level `if:` in `action.yml` skips it on a hit; PATH was already
 * appended by `resolve`.
 *
 * @param {import('@actions/github-script').AsyncFunctionArguments} args
 */
async function install({ core, exec }) {
	/** @type {ResolveMeta} */
	const meta = JSON.parse(required("META"));
	const { tag, triple, dest, cbhome, ext } = meta;

	const binstallSha = required("BINSTALL_SHA");
	// `github-script` is a JS action, so its `with: github-token:` lands as
	// the step input `github-token` — read it here rather than restating
	// `${{ inputs.github-token }}` as a separate env var in action.yml.
	const token = core.getInput("github-token");

	await core.group("runner-run | install cargo-binstall", async () => {
		// Pre-add CARGO_HOME/bin to PATH for the spawned bash so the upstream
		// installer detects it as already-present and skips writing to
		// $GITHUB_PATH. We invoke cargo-binstall by absolute path next; it
		// does not need to be on the consumer's PATH.
		const newPath = `${cbhome}/bin${delimiter}${env.PATH ?? ""}`;
		const url =
			`https://raw.githubusercontent.com/cargo-bins/cargo-binstall/${binstallSha}/install-from-binstall-release.sh`;

		const res = await fetch(url);
		if (!res.ok) {
			throw new Error(`failed to fetch cargo-binstall installer: ${res.status} ${res.statusText}`);
		}
		const script = await res.text();

		await exec.exec("bash", ["-s"], {
			input: Buffer.from(script),
			env: { ...env, CARGO_HOME: cbhome, PATH: newPath },
		});
	});

	await core.group(`runner-run | cargo-binstall runner-run@${tag.replace(/^v/, "")}`, async () => {
		const cbin = `${cbhome}/bin/cargo-binstall${ext}`;
		await exec.exec(cbin, [`runner-run@${tag.replace(/^v/, "")}`, "--install-path", dest], {
			env: {
				...env,
				// cargo-binstall reads GITHUB_TOKEN for GitHub API auth
				// (release lookups); without it CI hits the 60 req/hr
				// anonymous limit. Omit when empty so it isn't masked as a
				// "no token" being explicitly set.
				...(token ? { GITHUB_TOKEN: token } : {}),
				CARGO_BUILD_TARGET: triple,
				BINSTALL_NO_CONFIRM: "true",
				// Fail fast in CI; never silently build from source.
				BINSTALL_DISABLE_STRATEGIES: "compile",
			},
		});
	});
}

/* ────────────────────────────── helpers ──────────────────────────────── */

/**
 * Read a required env var or throw. Mirrors `${VAR:?}` from the bash
 * originals.
 *
 * @param {string} name
 * @returns {string}
 */
function required(name) {
	const v = env[name];
	if (!v) throw new Error(`required env var not set: ${name}`);
	return v;
}
