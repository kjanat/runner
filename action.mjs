// @ts-check
import { env } from "node:process";

/**
 * Setup runner — composite action body. A single `actions/github-script@v9`
 * step (no cache step ⇒ nothing to split around):
 *
 *   1. Resolve the npm version spec — `inputs.version` / `github.action_ref`
 *      if it looks like semver pins that exact version; anything else
 *      (`@master`, a branch/SHA ref, empty input) uses the `latest`
 *      dist-tag.
 *   2. `npm install --global runner-run@<spec>`. npm selects the correct
 *      prebuilt platform package (incl. musl — verified) via the facade's
 *      optionalDependencies; the binaries are static-pie so libc selection
 *      is robust regardless.
 *   3. Append npm's global bin dir to `$GITHUB_PATH` so `runner` / `run`
 *      are on PATH for every subsequent step in the caller's job — the
 *      whole point of a setup action (hence not npx/bunx, which are
 *      ephemeral).
 *
 * No GitHub API in the path ⇒ no token, no rate limit, no cargo-binstall,
 * no rust-triple mapping.
 *
 * @param {import('@actions/github-script').AsyncFunctionArguments} args
 */
export default async function run({ core, exec }) {
	return core.group("runner-run | setup", async () => {
		const spec = resolveSpec(core);

		await core.group(`npm install --global runner-run@${spec}`, async () => {
			await exec.exec(npm(), ["install", "--global", `runner-run@${spec}`]);
		});

		const version = await installedVersion(exec);
		const binDir = await npmGlobalBin(exec);

		core.addPath(binDir);
		core.info(`version: ${version}`);
		core.info(`bin-dir: ${binDir}`);

		core.setOutput("version", version);
		core.setOutput("bin-dir", binDir);
	});
}

/**
 * `npm` is `npm.cmd` on Windows runners — `@actions/exec` does not append
 * the `.cmd` itself.
 *
 * @returns {string}
 */
function npm() {
	return env.RUNNER_OS === "Windows" ? "npm.cmd" : "npm";
}

/**
 * npm version spec. Semver from `INPUT_VERSION` (the `version` input;
 * composite actions get no automatic `INPUT_*`, so `action.yml` maps it) or
 * `ACTION_REF` (`github.action_ref`, what makes `uses: kjanat/runner@v0.10.0`
 * pin) → that exact version, leading `v` stripped (npm wants `0.10.0`).
 * Anything else → the `latest` dist-tag.
 *
 * @param {import('@actions/github-script').AsyncFunctionArguments['core']} core
 * @returns {string}
 */
function resolveSpec(core) {
	const semver = /^v?\d+\.\d+\.\d+/;
	const requested = env.INPUT_VERSION || env.ACTION_REF || "";

	if (semver.test(requested)) {
		const v = requested.replace(/^v/, "");
		core.info(`version: ${v} (pinned)`);
		return v;
	}

	core.info(`'${requested || "(empty)"}' is not semver — using 'latest' dist-tag`);
	return "latest";
}

/**
 * The concrete installed version, queried post-install so the `version`
 * output is the real number even when the spec was `latest`.
 *
 * @param {import('@actions/github-script').AsyncFunctionArguments['exec']} exec
 * @returns {Promise<string>}
 */
async function installedVersion(exec) {
	const { stdout } = await exec.getExecOutput(
		npm(),
		["ls", "--global", "--depth=0", "--json", "runner-run"],
		{ silent: true },
	);
	/** @type {{ dependencies?: Record<string, { version?: string }> }} */
	const tree = JSON.parse(stdout);
	const v = tree.dependencies?.["runner-run"]?.version;
	if (!v) throw new Error("could not determine installed runner-run version from `npm ls`");
	return v;
}

/**
 * npm's global bin directory, cross-platform. On Windows npm links bins at
 * the prefix root; elsewhere at `<prefix>/bin`.
 *
 * @param {import('@actions/github-script').AsyncFunctionArguments['exec']} exec
 * @returns {Promise<string>}
 */
async function npmGlobalBin(exec) {
	const { stdout } = await exec.getExecOutput(
		npm(),
		["prefix", "--global"],
		{ silent: true },
	);
	const prefix = stdout.trim();
	if (!prefix) throw new Error("`npm prefix --global` returned empty");
	return env.RUNNER_OS === "Windows" ? prefix : `${prefix}/bin`;
}
