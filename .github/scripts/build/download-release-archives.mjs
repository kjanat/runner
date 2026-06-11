// @ts-check

import { mkdir, readdir, rm } from "node:fs/promises";
import { env } from "node:process";

/** @typedef {import('@actions/github-script').AsyncFunctionArguments['core']} Core */

const DOWNLOADS_DIR = "npm/downloads";

/**
 * Download every `runner-<tag>-<target>.tar.gz` + `.sha256` pair from the
 * (still draft) GitHub release into `npm/downloads`.
 *
 * Uses the `gh` CLI rather than the REST API because the release is a draft
 * at this point in the pipeline — `getReleaseByTag` can't see drafts, while
 * `gh release download` resolves them fine with a `contents: write` token.
 *
 * Required env:
 * - `RELEASE_TAG` — git tag, e.g. `v0.6.0`.
 * - `GH_TOKEN` — token with `contents: write` on this repo.
 *
 * @param {Pick<import('@actions/github-script').AsyncFunctionArguments, 'core' | 'context' | 'exec'>} args
 */
export default async function downloadReleaseArchives({ core, context, exec }) {
	const releaseTag = env.RELEASE_TAG;
	if (!releaseTag) {
		fail(core, "RELEASE_TAG env var is required");
		return;
	}

	// Scrub before fetch: stale .tar.gz/.sha256 from a previous tag would pass
	// verify-checksum.mjs (which walks every file in this dir) but be
	// wrong-version for the current RELEASE_TAG. Hosted GHA runners get fresh
	// workspaces, but self-hosted runners and local invocations don't.
	await rm(DOWNLOADS_DIR, { recursive: true, force: true });
	await mkdir(DOWNLOADS_DIR, { recursive: true });

	await exec.exec("gh", [
		"release",
		"download",
		releaseTag,
		"--repo",
		`${context.repo.owner}/${context.repo.repo}`,
		"--pattern",
		"runner-*-*.tar.gz",
		"--pattern",
		"runner-*-*.sha256",
		"--dir",
		DOWNLOADS_DIR,
	]);

	const files = await readdir(DOWNLOADS_DIR);
	core.info(`Downloaded ${files.length} files:\n${files.sort().join("\n")}`);
}

/**
 * @param {Core} core
 * @param {string} message
 */
function fail(core, message) {
	core.error(message, { file: ".github/workflows/release.yml", title: "Release archive download failed" });
	core.setFailed(message);
}
