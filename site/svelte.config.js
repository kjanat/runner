import adapter from "@sveltejs/adapter-cloudflare";
import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

/** @type {import('@sveltejs/kit').Config} */
const config = {
	preprocess: vitePreprocess(),
	kit: {
		adapter: adapter(),
		// Marketing site has no per-request state: prerender everything.
		// `+layout.ts` sets `prerender = true`; this enforces it and
		// fails the build on any non-prerenderable route.
		prerender: {
			handleHttpError: "fail",
			handleMissingId: "fail",
		},
	},
};

export default config;
