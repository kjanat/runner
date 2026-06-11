// @ts-check

import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { cwd, env } from "node:process";

const TAG_RE = /^v(?<version>\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)$/;
const SHA256_RE = /^[0-9a-f]{64}$/;
const BIN_TRIPLES = {
	x86_64: "x86_64-unknown-linux-gnu",
	aarch64: "aarch64-unknown-linux-gnu",
	armv7h: "armv7-unknown-linux-gnueabihf",
};

/** @typedef {import('@actions/github-script').AsyncFunctionArguments['core']} Core */
/** @typedef {import('@actions/github-script').AsyncFunctionArguments['github']} GitHub */
/** @typedef {import('@actions/github-script').AsyncFunctionArguments['context']} Context */
/** @typedef {{ name: string, id: number }} ReleaseAsset */

/**
 * @param {Pick<import('@actions/github-script').AsyncFunctionArguments, 'core' | 'github' | 'context'>} args
 */
export default async function prepareAurPkgbuild({ core, github, context }) {
	const tag = context.payload.release?.tag_name ?? context.payload.inputs?.tag ?? "";
	const version = TAG_RE.exec(tag)?.groups?.version;
	if (version === undefined) {
		core.error(`invalid tag: ${tag}`, {
			file: ".github/workflows/aur-release.yml",
			title: "Invalid release tag",
		});
		core.setFailed("invalid release tag");
		return;
	}

	const pkgname = env.PKGNAME;
	if (pkgname !== "runner-run" && pkgname !== "runner-run-bin") {
		core.error(`invalid AUR package: ${pkgname}`, {
			file: ".github/workflows/aur-release.yml",
			title: "Invalid AUR package",
		});
		core.setFailed("invalid AUR package");
		return;
	}

	const pkgbuild = join(env.GITHUB_WORKSPACE ?? cwd(), "aur", pkgname, "PKGBUILD");
	let content = await readFile(pkgbuild, "utf8");
	const versioned = replaceLine(core, content, /^pkgver=.*/m, `pkgver=${version}`, "pkgver");
	if (versioned === undefined) return;
	const resetPkgrel = replaceLine(core, versioned, /^pkgrel=.*/m, "pkgrel=1", "pkgrel");
	if (resetPkgrel === undefined) return;
	content = resetPkgrel;

	if (pkgname === "runner-run-bin") {
		const release = await github.rest.repos.getReleaseByTag({
			...context.repo,
			tag,
		});
		/** @type {ReleaseAsset[]} */
		const assets = await github.paginate(github.rest.repos.listReleaseAssets, {
			...context.repo,
			release_id: release.data.id,
			per_page: 100,
		});

		for (const [carch, triple] of Object.entries(BIN_TRIPLES)) {
			const sum = await checksumForAsset({
				assets,
				context,
				core,
				github,
				name: `runner-v${version}-${triple}.sha256`,
			});
			if (sum === undefined) return;
			const withArchSum = replaceLine(
				core,
				content,
				new RegExp(`^sha256sums_${carch}=\\(.*`, "m"),
				`sha256sums_${carch}=('${sum}')`,
				`sha256sums_${carch}`,
			);
			if (withArchSum === undefined) return;
			content = withArchSum;
		}

		const manSum = await checksumForAsset({
			assets,
			context,
			core,
			github,
			name: `runner-v${version}-man.sha256`,
		});
		if (manSum === undefined) return;
		const withManSum = replaceLine(core, content, /^sha256sums=\(.*/m, `sha256sums=('${manSum}')`, "sha256sums");
		if (withManSum === undefined) return;
		content = withManSum;
	}

	await writeFile(pkgbuild, content, "utf8");
	await core.summary
		.addHeading("AUR PKGBUILD")
		.addRaw(`Prepared ${pkgname} ${version}.`)
		.write();
}

/**
 * @param {{ assets: ReleaseAsset[], context: Context, core: Core, github: GitHub, name: string }} input
 * @returns {Promise<string | undefined>}
 */
async function checksumForAsset({ assets, context, core, github, name }) {
	const asset = assets.find((candidate) => candidate.name === name);
	if (asset === undefined) {
		fail(core, `release asset not found: ${name}`);
		return undefined;
	}

	const response = await github.request("GET /repos/{owner}/{repo}/releases/assets/{asset_id}", {
		...context.repo,
		asset_id: asset.id,
		headers: { accept: "application/octet-stream" },
	});
	const text = toText(response.data);
	const sum = text.trim().split(/\s+/)[0] ?? "";
	if (!SHA256_RE.test(sum)) {
		fail(core, `bad sha256 for ${name}: '${sum}'`);
		return undefined;
	}
	return sum;
}

/**
 * @param {unknown} data
 * @returns {string}
 */
function toText(data) {
	if (typeof data === "string") return data;
	if (data instanceof ArrayBuffer) return new TextDecoder().decode(data);
	if (ArrayBuffer.isView(data)) {
		return new TextDecoder().decode(new Uint8Array(data.buffer, data.byteOffset, data.byteLength));
	}
	return "";
}

/**
 * @param {Core} core
 * @param {string} content
 * @param {RegExp} pattern
 * @param {string} replacement
 * @param {string} label
 * @returns {string | undefined}
 */
function replaceLine(core, content, pattern, replacement, label) {
	if (!pattern.test(content)) {
		fail(core, `PKGBUILD line not found: ${label}`);
		return undefined;
	}
	return content.replace(pattern, replacement);
}

/**
 * @param {Core} core
 * @param {string} message
 */
function fail(core, message) {
	core.error(message, { file: ".github/workflows/aur-release.yml", title: "AUR prepare failed" });
	core.setFailed(message);
}
