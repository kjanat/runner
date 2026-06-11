// @ts-check

import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { env } from "node:process";

/** @typedef {import('@actions/github-script').AsyncFunctionArguments['core']} Core */

/**
 * Archive the generated `man/` directory and upload the tarball plus its
 * `.sha256` companion to the GitHub release for the current tag.
 *
 * Required env:
 * - `RELEASE_TAG` — git tag, e.g. `v0.6.0`.
 * - `GH_TOKEN` — token with `contents: write` on this repo.
 *
 * @param {Pick<import('@actions/github-script').AsyncFunctionArguments, 'core' | 'exec'>} args
 */
export default async function archiveManPages({ core, exec }) {
	const releaseTag = env.RELEASE_TAG;
	if (!releaseTag) {
		fail(core, "RELEASE_TAG env var is required");
		return;
	}

	const archive = `runner-${releaseTag}-man.tar.gz`;
	const checksum = `runner-${releaseTag}-man.sha256`;

	await exec.exec("tar", ["-C", "man", "-czf", archive, "."]);

	// `<hex>  <basename>`, the format `sha256sum` emits — aur.mjs and any
	// downstream consumer parse the first whitespace-separated field.
	const digest = createHash("sha256").update(await readFile(archive)).digest("hex");
	await writeFile(checksum, `${digest}  ${archive}\n`, "utf8");

	await exec.exec("gh", ["release", "upload", releaseTag, archive, checksum, "--clobber"]);
}

/**
 * @param {Core} core
 * @param {string} message
 */
function fail(core, message) {
	core.error(message, { file: ".github/workflows/release.yml", title: "Man page archive failed" });
	core.setFailed(message);
}
