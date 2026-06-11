// @ts-check

import { createHash } from "node:crypto";
import { chmod, copyFile, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { env } from "node:process";

/** @typedef {import('@actions/github-script').AsyncFunctionArguments['core']} Core */

/**
 * Package built `runner` and `run` binaries into a release tarball matching
 * the layout `taiki-e/upload-rust-binary-action` produces, then upload the
 * archive and its `.sha256` companion to the GitHub release for the current
 * tag.
 *
 * Used by release.yml for build paths that can't go through
 * `taiki-e/upload-rust-binary-action`:
 *
 * - tier-3 BSD targets requiring `-Z build-std` (the action has no way to
 *   inject that flag), and
 * - VM-built targets such as OpenBSD (the action runs on the outer Linux
 *   host and never sees the VM's filesystem).
 *
 * Required env:
 * - `RELEASE_TAG` — git tag, e.g. `v0.6.0`. Same value the matrix consumes.
 * - `TARGET` — Rust target triple, e.g. `aarch64-unknown-freebsd`.
 * - `BIN_DIR` — directory containing the freshly built `runner` and `run`
 *   binaries (no `.exe` — BSDs don't use it).
 * - `GH_TOKEN` — token with `contents: write` on this repo.
 *
 * @param {Pick<import('@actions/github-script').AsyncFunctionArguments, 'core' | 'exec'>} args
 */
export default async function packageReleaseAsset({ core, exec }) {
	const releaseTag = env.RELEASE_TAG;
	const target = env.TARGET;
	const binDir = env.BIN_DIR;
	if (!releaseTag || !target || !binDir) {
		fail(core, "RELEASE_TAG, TARGET, and BIN_DIR env vars are required");
		return;
	}

	// Defensive: this script doesn't handle .exe binaries. The release.yml
	// matrix only routes BSDs through cargo-build-std today, but a future
	// config could route a Windows target here and silently produce a
	// broken archive. Bail loudly instead.
	if (target.includes("windows")) {
		fail(core, "package-release-asset.mjs does not handle Windows targets (.exe naming)");
		return;
	}

	const archiveBasename = `runner-${releaseTag}-${target}`;
	const archive = `${archiveBasename}.tar.gz`;
	// `<basename>.sha256`, NOT `<basename>.tar.gz.sha256`. Matches the
	// convention `taiki-e/upload-rust-binary-action` uses, which is what
	// verify-checksum.mjs enforces.
	const checksum = `${archiveBasename}.sha256`;

	const staging = await mkdtemp(join(tmpdir(), "package-release-asset-"));
	try {
		// Lay out the contents the way upload-rust-binary-action does with
		// `leading_dir: false` and `include: README.md,LICENSE`: every file at
		// the tarball root, no wrapper directory. build-packages.ts only
		// matches by basename, but verify-checksum.mjs and any user inspecting
		// the archive expect this exact layout.
		for (const bin of ["runner", "run"]) {
			const src = join(binDir, bin);
			try {
				await copyFile(src, join(staging, bin));
			} catch {
				fail(core, `${src} not found — build step did not produce ${bin}`);
				return;
			}
			await chmod(join(staging, bin), 0o755);
		}
		await copyFile("README.md", join(staging, "README.md"));
		await copyFile("LICENSE", join(staging, "LICENSE"));

		await exec.exec("tar", ["-C", staging, "-czf", archive, "runner", "run", "README.md", "LICENSE"]);

		// `<hex>  <basename>` — the exact line `sha256sum` emits when invoked
		// from the archive's directory, which verify-checksum.mjs requires.
		const digest = createHash("sha256").update(await readFile(archive)).digest("hex");
		await writeFile(checksum, `${digest}  ${archive}\n`, "utf8");

		await exec.exec("gh", ["release", "upload", releaseTag, archive, checksum, "--clobber"]);
	} finally {
		await rm(staging, { recursive: true, force: true });
	}

	await core.summary
		.addHeading("Release asset packaged")
		.addRaw(`Uploaded ${archive} and ${checksum} to ${releaseTag}.`)
		.write();
}

/**
 * @param {Core} core
 * @param {string} message
 */
function fail(core, message) {
	core.error(message, { file: ".github/workflows/release.yml", title: "Release asset packaging failed" });
	core.setFailed(message);
}
