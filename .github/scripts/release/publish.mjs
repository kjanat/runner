// @ts-check

const TAG_RE = /^v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;

/** @typedef {import('@actions/github-script').AsyncFunctionArguments['core']} Core */
/** @typedef {import('@actions/github-script').AsyncFunctionArguments['github']} GitHub */
/** @typedef {import('@actions/github-script').AsyncFunctionArguments['context']} Context */

/**
 * @param {Pick<import('@actions/github-script').AsyncFunctionArguments, 'core' | 'github' | 'context'>} args
 */
export async function publishGithubRelease({ core, github, context }) {
	const tag = tagFromRef(core, context);
	if (tag === undefined) return;

	const release = await github.rest.repos.getReleaseByTag({
		...context.repo,
		tag,
	});
	await github.rest.repos.updateRelease({
		...context.repo,
		release_id: release.data.id,
		draft: false,
	});
	core.notice(`Published GitHub release ${tag}`, { title: "Release published" });
}

/**
 * @param {Pick<import('@actions/github-script').AsyncFunctionArguments, 'core' | 'github' | 'context'>} args
 */
export async function dispatchPackagePublishes({ core, github, context }) {
	const tag = tagFromRef(core, context);
	if (tag === undefined) return;

	await github.rest.actions.createWorkflowDispatch({
		...context.repo,
		workflow_id: "npm-release.yml",
		ref: "master",
		inputs: {
			tag,
			"run-id": String(context.runId),
			"dry-run": "false",
		},
	});
	await github.rest.actions.createWorkflowDispatch({
		...context.repo,
		workflow_id: "aur-release.yml",
		ref: "master",
		inputs: {
			tag,
			"dry-run": "false",
		},
	});
	core.notice(`Dispatched npm and AUR publish workflows for ${tag}`, {
		title: "Package publishes dispatched",
	});
	await core.summary
		.addHeading("Package publishes dispatched")
		.addList([`npm-release.yml for ${tag}`, `aur-release.yml for ${tag}`])
		.write();
}

/**
 * @param {Core} core
 * @param {Context} context
 * @returns {string | undefined}
 */
function tagFromRef(core, context) {
	const tag = context.ref.replace(/^refs\/tags\//, "");
	if (TAG_RE.test(tag)) return tag;
	core.error(`invalid tag: ${tag}`, {
		file: ".github/workflows/release.yml",
		title: "Invalid release tag",
	});
	core.setFailed("invalid release tag");
	return undefined;
}
