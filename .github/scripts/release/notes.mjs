// @ts-check

const TAG_RE = /^v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;

/**
 * @param {Pick<import('@actions/github-script').AsyncFunctionArguments, 'core' | 'github' | 'context'>} args
 */
export default async function appendGeneratedReleaseNotes({ core, github, context }) {
	const tag = context.ref.replace(/^refs\/tags\//, "");
	if (!TAG_RE.test(tag)) {
		core.error(`invalid tag: ${tag}`, {
			file: ".github/workflows/release.yml",
			title: "Invalid release tag",
		});
		core.setFailed("invalid release tag");
		return;
	}

	const { owner, repo } = context.repo;
	const release = await github.rest.repos.getReleaseByTag({ owner, repo, tag });
	const generated = await github.rest.repos.generateReleaseNotes({
		owner,
		repo,
		tag_name: tag,
	});
	const body = `${
		[release.data.body, generated.data.body]
			.map((part) => (part ?? "").trim())
			.filter(Boolean)
			.join("\n\n")
	}\n`;

	await github.rest.repos.updateRelease({
		owner,
		repo,
		release_id: release.data.id,
		body,
	});
	await core.summary
		.addHeading("Release notes")
		.addRaw(`Updated notes for ${tag}.`)
		.write();
}
