// @ts-check

import { access } from "node:fs/promises";

const TAG_RE = /^v(?<version>\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)$/;

/**
 * @param {Pick<import('@actions/github-script').AsyncFunctionArguments, 'core' | 'context' | 'exec'>} args
 */
export default async function buildNpmPackages({ core, context, exec }) {
	const tag = context.ref.replace(/^refs\/tags\//, "");
	const version = TAG_RE.exec(tag)?.groups?.version;
	if (version === undefined) {
		core.error(`invalid tag: ${tag}`, {
			file: ".github/workflows/release.yml",
			title: "Invalid release tag",
		});
		core.setFailed("invalid release tag");
		return;
	}

	const args = ["npm/scripts/build-packages.ts", "--version", version];
	if (await exists("man")) args.push("--man-dir", "man");
	await exec.exec("node", args);
	await core.summary
		.addHeading("npm package build")
		.addRaw(`Built npm packages for ${tag}.`)
		.write();
}

/**
 * @param {string} path
 * @returns {Promise<boolean>}
 */
async function exists(path) {
	try {
		await access(path);
		return true;
	} catch {
		return false;
	}
}
