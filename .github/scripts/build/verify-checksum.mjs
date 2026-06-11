// @ts-check

import { createHash } from "node:crypto";
import { readdir, readFile } from "node:fs/promises";
import { join } from "node:path";
import { cwd, env } from "node:process";

/** @typedef {import('@actions/github-script').AsyncFunctionArguments['core']} Core */

// `<hex>  <name>` as written by `sha256sum`; the optional `*` marks
// binary mode and is not part of the filename.
const SUM_LINE_RE = /^(?<hex>[0-9a-f]{64})\s+\*?(?<name>\S+)$/;

/**
 * Verify every downloaded release tarball against its `.sha256` companion.
 *
 * Required env:
 * - `RELEASE_TAG` — git tag, used in error messages only.
 *
 * @param {Pick<import('@actions/github-script').AsyncFunctionArguments, 'core'>} args
 */
export default async function verifyChecksums({ core }) {
	const releaseTag = env.RELEASE_TAG ?? "(unknown tag)";
	const dir = join(env.GITHUB_WORKSPACE ?? cwd(), "npm", "downloads");

	const files = await readdir(dir);
	const tarballs = files.filter((name) => name.endsWith(".tar.gz"));
	const sums = files.filter((name) => name.endsWith(".sha256"));

	// Refuse to proceed unless every tarball has a matching .sha256 and
	// every .sha256 has a matching tarball — otherwise an unchecksummed
	// binary could slip through to publish.
	if (tarballs.length === 0) {
		fail(core, `no tarballs downloaded for ${releaseTag}`);
		return;
	}
	for (const tarball of tarballs) {
		const expected = tarball.replace(/\.tar\.gz$/, ".sha256");
		if (!sums.includes(expected)) {
			fail(core, `tarball ${tarball} has no matching ${expected}`);
			return;
		}
	}

	for (const sum of sums) {
		const expected = sum.replace(/\.sha256$/, ".tar.gz");
		if (!tarballs.includes(expected)) {
			fail(core, `checksum file ${sum} has no matching ${expected}`);
			return;
		}

		const line = (await readFile(join(dir, sum), "utf8")).trim();
		const parsed = SUM_LINE_RE.exec(line)?.groups;
		if (parsed === undefined) {
			fail(core, `malformed checksum file ${sum}: '${line}'`);
			return;
		}

		// Verify each .sha256 references a tarball whose name matches its
		// own basename — defends against a release where foo.sha256 was
		// swapped to reference bar.tar.gz, which would leave foo.tar.gz
		// unchecked while a plain `sha256sum -c` silently re-verifies bar.
		if (parsed.name !== expected) {
			fail(core, `${sum} references '${parsed.name}', expected '${expected}'`);
			return;
		}

		const digest = createHash("sha256").update(await readFile(join(dir, expected))).digest("hex");
		if (digest !== parsed.hex) {
			fail(core, `checksum mismatch for ${expected}: expected ${parsed.hex}, got ${digest}`);
			return;
		}
	}

	core.info(`Verified ${tarballs.length} tarballs against their checksums.`);
}

/**
 * @param {Core} core
 * @param {string} message
 */
function fail(core, message) {
	core.error(message, { file: ".github/workflows/release.yml", title: "Checksum verification failed" });
	core.setFailed(message);
}
