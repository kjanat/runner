import { sveltekit } from "@sveltejs/kit/vite";
import { defineConfig } from "vitest/config";

export default defineConfig({
	plugins: [sveltekit()],
	// Allow importing the workspace-root CHANGELOG.md (?raw) from the
	// /changelog route — it sits one level above the site root.
	server: { fs: { allow: [".."] } },
	test: {
		include: [
			"src/**/*.{test,spec}.{js,ts}",
			"scripts/**/*.{test,spec}.{js,ts}",
		],
		environment: "node",
	},
});
