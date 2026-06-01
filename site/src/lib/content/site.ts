// The single typed site-data source. Values are resolved at build
// time from the workspace-root Cargo.toml by scripts/gen-site-data.ts
// (run via `prebuild`). Components receive this as props — they never
// reach for it directly, so each stays composable and unit-testable.

import { generated } from "./generated";

export interface SiteData {
	/** Crate/release version, e.g. "0.11.0". */
	version: string;
	/** crates.io crate name. */
	cratesName: string;
	/** npm package name (façade). */
	npmName: string;
	/** SPDX license id. */
	license: string;
	/** Canonical repo URL, no trailing slash, no .git. */
	repo: string;
	/** "owner/name" short form. */
	repoShort: string;
	authorName: string;
	authorEmail: string;
	/** Repo default branch (e.g. "master"). Single source for
	 * branch-bound URLs (footer changelog link, raw-githubuser-
	 * content installer URL); rename = one Cargo.toml edit. */
	defaultBranch: string;
}

export const site: SiteData = generated;

/** Canonical install/setup commands, derived from {@link site} so the
 * displayed strings can never drift from the real package names. */
export function commands(s: SiteData) {
	return {
		npm: `npm install -g ${s.npmName}`,
		cargoBinstall: `cargo binstall ${s.cratesName}`,
		cargoInstall: `cargo install ${s.cratesName}`,
		linuxInstaller: `curl -fsSL https://raw.githubusercontent.com/${s.repoShort}/${s.defaultBranch}/install.sh | sh`,
		completionsPosix: `eval "$(runner completions)"`,
		completionsPwsh: `runner completions powershell | Out-String | Invoke-Expression`,
	} as const;
}
