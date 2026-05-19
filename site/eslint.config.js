import js from "@eslint/js";
import svelte from "eslint-plugin-svelte";
import globals from "globals";
import tseslint from "typescript-eslint";

/** @type {import("eslint").Linter.Config[]} */
export default [
	{
		ignores: [
			".svelte-kit/",
			"build/",
			"dist/",
			".wrangler/",
			"node_modules/",
			"src/lib/content/generated.ts",
		],
	},
	js.configs.recommended,
	...tseslint.configs.recommended,
	...svelte.configs.recommended,
	{
		languageOptions: {
			globals: { ...globals.browser, ...globals.node },
		},
	},
	{
		files: ["**/*.svelte", "**/*.svelte.ts", "**/*.svelte.js"],
		languageOptions: {
			parserOptions: {
				parser: tseslint.parser,
				extraFileExtensions: [".svelte"],
			},
		},
	},
	{
		rules: {
			// Deliberate, reviewed project-wide decisions (not silent
			// per-line suppressions):
			// - no-at-html-tags: every {@html} renders trusted
			//   build-time content via an escape-first (`renderInline`,
			//   test-covered) or whitespace-exact-literal renderer; no
			//   user input ever reaches it.
			// - no-navigation-without-resolve: fully prerendered site
			//   at the domain root, no `base` path, no i18n; internal
			//   links gain nothing from resolve() and external links
			//   (github/crates/npm/mailto) cannot use it.
			"svelte/no-at-html-tags": "off",
			"svelte/no-navigation-without-resolve": "off",
		},
	},
];
