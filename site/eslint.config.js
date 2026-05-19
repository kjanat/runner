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
];
