import { parseChangelog } from "$lib/changelog";
import { error } from "@sveltejs/kit";
import type { PageServerLoad } from "./$types";
// Vite inlines the workspace-root CHANGELOG.md as a string at build
// time (resolved from this source file, survives bundling, HMR in
// dev). Needs `server.fs.allow: ['..']` in vite.config.ts since the
// file lives outside the site root. Parsed once at prerender; the
// raw markdown never reaches the client bundle.
import changelogRaw from "../../../../CHANGELOG.md?raw";

export const load: PageServerLoad = () => {
	const releases = parseChangelog(changelogRaw);
	if (releases.length === 0) {
		error(500, "CHANGELOG.md parsed to zero releases");
	}
	return { releases };
};
