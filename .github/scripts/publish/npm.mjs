// @ts-check

import { access, readdir, readFile } from "node:fs/promises";
import path from "node:path";

const TIMEOUT = "120s";
const CONFLICT_RE = /EPUBLISHCONFLICT|cannot publish over the previously published versions/i;
const TAG_RE = /^v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;
const DIST_TAG_RE = /^[A-Za-z][A-Za-z0-9._-]*$/;

/** @typedef {import('@actions/github-script').AsyncFunctionArguments['core']} Core */
/** @typedef {import('@actions/github-script').AsyncFunctionArguments['context']} Context */
/** @typedef {import('@actions/github-script').AsyncFunctionArguments['github']} GitHub */
/** @typedef {import('@actions/github-script').AsyncFunctionArguments['exec']} Exec */
/** @typedef {{ pkg: string, experimental?: boolean }} Target */
/** @typedef {{ facade: string, scope: string, targets: Target[] }} TargetsConfig */
/** @typedef {{ name: string, status: string }} PublishSummary */
/** @typedef {{ ok: true, summary: PublishSummary } | { ok: false }} PublishResult */
/** @typedef {{ name?: string, version?: string, publishConfig?: unknown, optionalDependencies?: Record<string, string> }} PackageJson */
/** @typedef {{ core: Core, exec: Exec, dir: string, distTag: string, dryRun: boolean, expectedName: string, expectedVersion: string, facade: string, registry: string, required: boolean, requiredPlatforms: string[], optionalPlatforms: string[], scope: string }} PublishOptions */

/**
 * @param {Pick<import('@actions/github-script').AsyncFunctionArguments, 'core' | 'github' | 'context'>} args
 */
export async function resolveReleaseRun({ core, github, context }) {
	const tag = context.payload.release?.tag_name ?? context.payload.inputs?.tag ?? "";
	if (!TAG_RE.test(tag)) {
		core.error(`invalid tag: ${tag}`, {
			file: ".github/workflows/npm-release.yml",
			title: "Invalid release tag",
		});
		core.setFailed("invalid release tag");
		return;
	}

	const runId = await resolveRunId({ core, context, github, tag });
	if (runId === undefined) return;
	const ok = await waitForReleaseRun({ core, context, github, runId });
	if (!ok) return;

	const value = String(runId);
	core.setOutput("run-id", value);
	await core.summary
		.addHeading("Release artifact ready")
		.addRaw(`Using release workflow run ${runId} for ${tag}.`)
		.write();
}

/**
 * @param {Pick<import('@actions/github-script').AsyncFunctionArguments, 'core' | 'context'>} args
 */
export async function deriveNpmPublishSettings({ core, context }) {
	const tag = context.payload.release?.tag_name ?? context.payload.inputs?.tag ?? "";
	if (!TAG_RE.test(tag)) {
		core.error(`invalid tag: ${tag}`, {
			file: ".github/workflows/npm-release.yml",
			title: "Invalid release tag",
		});
		core.setFailed("invalid release tag");
		return;
	}

	const distTag = context.payload.inputs?.["dist-tag"] ?? (tag.includes("-") ? "next" : "latest");
	const dryRun = String(context.payload.inputs?.["dry-run"] ?? "false").toLowerCase();
	if (!DIST_TAG_RE.test(distTag)) {
		core.error(`invalid npm dist-tag: ${distTag}`, {
			file: ".github/workflows/npm-release.yml",
			title: "Invalid npm dist-tag",
		});
		core.setFailed("invalid npm dist-tag");
		return;
	}
	if (dryRun !== "true" && dryRun !== "false") {
		core.error(`dry-run must be true or false: ${dryRun}`, {
			file: ".github/workflows/npm-release.yml",
			title: "Invalid dry-run input",
		});
		core.setFailed("invalid dry-run input");
		return;
	}

	core.setOutput("dist-tag", distTag);
	core.setOutput("dry-run", dryRun);
	await core.summary
		.addHeading("npm publish settings")
		.addList([`dist-tag: ${distTag}`, `dry-run: ${dryRun}`])
		.write();
}

/**
 * @param {Pick<import('@actions/github-script').AsyncFunctionArguments, 'core' | 'exec'>} args
 */
export default async function publishNpmPackages({ core, exec }) {
	const derive = parseJsonInput(core, "derive_outputs");
	const workflowEnv = parseJsonInput(core, "workflow_env");
	if (derive === undefined || workflowEnv === undefined) return;

	const releaseTag = stringField(core, workflowEnv, "RELEASE_TAG");
	const registry = stringField(core, workflowEnv, "REGISTRY");
	const distTag = stringField(core, derive, "dist-tag");
	const dryRun = booleanStringField(core, derive, "dry-run");
	if (releaseTag === undefined || registry === undefined || distTag === undefined || dryRun === undefined) return;
	const expectedVersion = releaseTag.replace(/^v/, "");
	const workspace = process.env.GITHUB_WORKSPACE ?? process.cwd();
	const distRoot = path.join(workspace, "npm", "dist");
	/** @type {TargetsConfig} */
	const targetsConfig = JSON.parse(
		await readFile(path.join(workspace, "npm", "targets.json"), "utf8"),
	);
	const requiredPlatforms = targetsConfig.targets
		.filter((target) => !(target.experimental ?? false))
		.map((target) => target.pkg);
	const optionalPlatforms = targetsConfig.targets
		.filter((target) => target.experimental ?? false)
		.map((target) => target.pkg);
	const allowedPackages = new Set([
		targetsConfig.facade,
		...requiredPlatforms,
		...optionalPlatforms,
	]);
	/** @type {PublishSummary[]} */
	const results = [];

	for (const entry of await readdir(distRoot, { withFileTypes: true })) {
		if (entry.isDirectory() && !allowedPackages.has(entry.name)) {
			return fail(core, `artifact contains unexpected directory '${entry.name}'`);
		}
	}

	for (const platform of requiredPlatforms) {
		const result = await publishAllowed({
			core,
			exec,
			dir: path.join(distRoot, platform),
			distTag,
			dryRun,
			expectedName: `${targetsConfig.scope}/${platform}`,
			expectedVersion,
			facade: targetsConfig.facade,
			registry,
			required: true,
			requiredPlatforms,
			optionalPlatforms,
			scope: targetsConfig.scope,
		});
		if (!result.ok) return;
		results.push(result.summary);
	}
	for (const platform of optionalPlatforms) {
		const result = await publishAllowed({
			core,
			exec,
			dir: path.join(distRoot, platform),
			distTag,
			dryRun,
			expectedName: `${targetsConfig.scope}/${platform}`,
			expectedVersion,
			facade: targetsConfig.facade,
			registry,
			required: false,
			requiredPlatforms,
			optionalPlatforms,
			scope: targetsConfig.scope,
		});
		if (!result.ok) return;
		results.push(result.summary);
	}
	const facadeResult = await publishAllowed({
		core,
		exec,
		dir: path.join(distRoot, targetsConfig.facade),
		distTag,
		dryRun,
		expectedName: targetsConfig.facade,
		expectedVersion,
		facade: targetsConfig.facade,
		registry,
		required: true,
		requiredPlatforms,
		optionalPlatforms,
		scope: targetsConfig.scope,
	});
	if (!facadeResult.ok) return;
	results.push(facadeResult.summary);

	await core.summary
		.addHeading("npm publish")
		.addList(results.map(({ name, status }) => `${name}: ${status}`))
		.write();
}

/**
 * @param {PublishOptions} options
 * @returns {Promise<PublishResult>}
 */
async function publishAllowed(options) {
	const {
		core,
		exec,
		dir,
		distTag,
		dryRun,
		expectedName,
		expectedVersion,
		facade,
		registry,
		required,
		requiredPlatforms,
		optionalPlatforms,
		scope,
	} = options;
	if (!(await exists(dir))) {
		if (required) return fail(core, `required package ${expectedName} missing from artifact`);
		return { ok: true, summary: { name: expectedName, status: "skipped missing optional artifact" } };
	}

	const packagePath = path.join(dir, "package.json");
	if (!(await exists(packagePath))) return fail(core, `${packagePath} missing`);
	if (await exists(path.join(dir, ".npmrc"))) {
		return fail(core, `${dir}/.npmrc is forbidden because it could redirect publish`);
	}

	/** @type {PackageJson} */
	const pkg = JSON.parse(await readFile(packagePath, "utf8"));
	if (Object.prototype.hasOwnProperty.call(pkg, "publishConfig")) {
		return fail(core, `${packagePath} has publishConfig, which could redirect publish`);
	}
	if (pkg.name !== expectedName) {
		return fail(core, `${packagePath} declares name '${pkg.name}', expected '${expectedName}'`);
	}
	if (pkg.version !== expectedVersion) {
		return fail(
			core,
			`${packagePath} declares version '${pkg.version}', expected '${expectedVersion}'`,
		);
	}

	const optionalDependencies = pkg.optionalDependencies ?? {};
	if (expectedName === facade) {
		const expectedDeps = new Set([...requiredPlatforms, ...optionalPlatforms]);
		for (const [depName, depVersion] of Object.entries(optionalDependencies)) {
			if (!depName.startsWith(`${scope}/`)) {
				return fail(core, `facade optionalDependencies entry '${depName}' not under scope '${scope}'`);
			}
			const platform = depName.slice(scope.length + 1);
			if (!expectedDeps.has(platform)) {
				return fail(core, `facade optionalDependencies references unexpected package '${depName}'`);
			}
			if (depVersion !== expectedVersion) {
				return fail(core, `facade optionalDependencies['${depName}'] = '${depVersion}', expected '${expectedVersion}'`);
			}
		}
		for (const platform of requiredPlatforms) {
			if (!Object.prototype.hasOwnProperty.call(optionalDependencies, `${scope}/${platform}`)) {
				return fail(core, `facade optionalDependencies missing required package '${scope}/${platform}'`);
			}
		}
	} else if (Object.keys(optionalDependencies).length > 0) {
		return fail(core, `${packagePath} has optionalDependencies; only ${facade} may declare any`);
	}

	core.setOutput("package-url", `https://npm.im/${expectedName}`);
	const published = await exec.getExecOutput(
		"timeout",
		[TIMEOUT, "npm", "view", `${expectedName}@${expectedVersion}`, "--registry", registry, "version"],
		{ ignoreReturnCode: true, silent: true },
	);
	if (published.exitCode === 124) {
		return fail(core, `npm view ${expectedName}@${expectedVersion} timed out after ${TIMEOUT}`);
	}
	if (published.stdout.trim() === expectedVersion) {
		return { ok: true, summary: { name: expectedName, status: "already published" } };
	}

	const args = /* dprint-ignore */ [
		TIMEOUT,
		"npm", "publish",
		"--registry", registry,
		"--access", "public",
		"--tag", distTag,
		"--ignore-scripts",
		"--provenance",
	];
	if (dryRun) args.push("--dry-run");
	const publishedPackage = await exec.getExecOutput("timeout", args, {
		cwd: dir,
		ignoreReturnCode: true,
	});
	if (publishedPackage.exitCode === 124) {
		return fail(core, `npm publish for ${expectedName}@${expectedVersion} timed out after ${TIMEOUT}`);
	}
	if (publishedPackage.exitCode !== 0) {
		const output = `${publishedPackage.stdout}\n${publishedPackage.stderr}`;
		if (CONFLICT_RE.test(output)) {
			return { ok: true, summary: { name: expectedName, status: "already published" } };
		}
		return fail(core, `npm publish failed for ${expectedName}@${expectedVersion}`);
	}

	return { ok: true, summary: { name: expectedName, status: dryRun ? "dry-run ok" : "published" } };
}

/**
 * @param {string} filePath
 * @returns {Promise<boolean>}
 */
async function exists(filePath) {
	try {
		await access(filePath);
		return true;
	} catch {
		return false;
	}
}

/**
 * @param {{ core: Core, context: Context, github: GitHub, tag: string }} input
 * @returns {Promise<number | undefined>}
 */
async function resolveRunId({ core, context, github, tag }) {
	if (context.eventName === "workflow_dispatch") {
		const inputRunId = context.payload.inputs?.["run-id"] ?? "";
		if (!/^\d+$/.test(inputRunId)) {
			core.error(`run-id is not a positive integer: ${inputRunId}`, {
				file: ".github/workflows/npm-release.yml",
				title: "Invalid release run id",
			});
			core.setFailed("invalid release run id");
			return undefined;
		}
		return Number(inputRunId);
	}

	core.warning("release event did not provide run-id; resolving latest successful release run", {
		title: "Fallback run-id resolution",
	});
	const runs = await github.rest.actions.listWorkflowRuns({
		...context.repo,
		workflow_id: "release.yml",
		branch: tag,
		status: "success",
		per_page: 1,
	});
	const runId = runs.data.workflow_runs[0]?.id;
	if (runId === undefined) {
		core.error(`no run-id resolvable for ${tag}`, {
			file: ".github/workflows/npm-release.yml",
			title: "Missing release run id",
		});
		core.setFailed("missing release run id");
	}
	return runId;
}

/**
 * @param {{ core: Core, context: Context, github: GitHub, runId: number }} input
 * @returns {Promise<boolean>}
 */
async function waitForReleaseRun({ core, context, github, runId }) {
	core.startGroup(`Wait for release run ${runId}`);
	try {
		for (let attempt = 0; attempt < 120; attempt += 1) {
			const run = await github.rest.actions.getWorkflowRun({
				...context.repo,
				run_id: runId,
			});
			if (run.data.status === "completed") {
				if (run.data.conclusion === "success") return true;
				core.error(`release run ${runId} completed with ${run.data.conclusion}`, {
					file: ".github/workflows/npm-release.yml",
					title: "Release run failed",
				});
				core.setFailed("release run failed");
				return false;
			}
			await new Promise((resolve) => setTimeout(resolve, 5000));
		}
	} finally {
		core.endGroup();
	}
	core.error(`timed out waiting for release run ${runId}`, {
		file: ".github/workflows/npm-release.yml",
		title: "Release run timeout",
	});
	core.setFailed("release run timeout");
	return false;
}

/**
 * @param {Core} core
 * @param {string} name
 * @returns {Record<string, unknown> | undefined}
 */
function parseJsonInput(core, name) {
	const input = core.getInput(name, { required: true });
	try {
		const value = JSON.parse(input);
		if (value !== null && typeof value === "object" && !Array.isArray(value)) return value;
	} catch {
		// Fall through to one annotated failure path below.
	}
	fail(core, `${name} must be a JSON object`);
	return undefined;
}

/**
 * @param {Core} core
 * @param {Record<string, unknown>} record
 * @param {string} field
 * @returns {string | undefined}
 */
function stringField(core, record, field) {
	const value = record[field];
	if (typeof value === "string" && value !== "") return value;
	fail(core, `${field} must be a non-empty string`);
	return undefined;
}

/**
 * @param {Core} core
 * @param {Record<string, unknown>} record
 * @param {string} field
 * @returns {boolean | undefined}
 */
function booleanStringField(core, record, field) {
	const value = stringField(core, record, field);
	if (value === undefined) return undefined;
	if (value === "true") return true;
	if (value === "false") return false;
	fail(core, `${field} must be true or false`);
	return undefined;
}

/**
 * @param {Core} core
 * @param {string} message
 * @returns {{ ok: false }}
 */
function fail(core, message) {
	core.error(message, { file: ".github/workflows/npm-release.yml", title: "npm publish failed" });
	core.setFailed(message);
	return { ok: false };
}
