#!/usr/bin/env bun
import { cp, readdir, rm } from "node:fs/promises";
import { join } from "node:path";
import cargo from "../Cargo.toml" with { type: "toml" };

const root = import.meta.dir;
const dist = join(root, "dist");
const src = join(root, "src");
const pub = join(root, "public");

const pkg = cargo.package;
const author = pkg.metadata.authors[0];
const repo = pkg.repository.replace(/\/$/, "");

const tokens: Record<string, string> = {
	version: pkg.version,
	repo,
	repoShort: repo.replace(/^https?:\/\//, ""),
	license: pkg.license,
	description: pkg.description,
	npmName: pkg.metadata.npm.name,
	authorName: author.name,
	authorEmail: author.email,
};

const analyticsSnippet = [
	"\t\t<!-- Cloudflare Web Analytics -->",
	// biome-ignore lint/security/noSecrets: Cloudflare Web Analytics token is public page config.
	"\t\t<script defer src=\"https://static.cloudflareinsights.com/beacon.min.js\" data-cf-beacon='{\"token\": \"092edc6dde124fe4816fd2d95c16db39\"}'></script>",
	"\t\t<!-- End Cloudflare Web Analytics -->",
].join("\n");

interface BuildOptions {
	analytics?: boolean;
}

export async function build(options: BuildOptions = {}): Promise<void> {
	await rm(dist, { recursive: true, force: true });

	const result = await Bun.build({
		entrypoints: [join(src, "index.html"), join(src, "404.html")],
		outdir: dist,
		target: "browser",
		minify: true,
	});

	if (!result.success) {
		for (const log of result.logs) console.error(log);
		throw new Error("build failed");
	}

	const emptyJsOutputs = result.outputs.filter(
		(out) => out.size === 0 && out.path.endsWith(".js"),
	);
	const emptyChunks = new Set<string>();
	for (const out of emptyJsOutputs) {
		const name = out.path.split("/").pop();
		if (name) emptyChunks.add(name);
	}
	await Promise.all(emptyJsOutputs.map((out) => rm(out.path, { force: true })));
	const emptyScript = emptyChunks.size
		? new RegExp(
			`<script[^>]+src="\\.?/?(?:${[...emptyChunks].join("|")})"[^>]*></script>`,
			"g",
		)
		: null;

	const placeholder = /\{\{(\w+)\}\}/g;
	const htmls = (await readdir(dist)).filter((f) => f.endsWith(".html"));
	await Promise.all(
		htmls.map(async (file) => {
			const path = join(dist, file);
			let html = await Bun.file(path).text();
			if (emptyScript) html = html.replace(emptyScript, "");
			html = html.replace(placeholder, (raw, key: string) => {
				const value = tokens[key];
				if (value === undefined) {
					throw new Error(`unknown placeholder ${raw} in ${file}`);
				}
				return value;
			});
			html = applyAnalytics(html, file, options.analytics);
			await Bun.write(path, html);
		}),
	);

	await cp(pub, dist, { recursive: true });
}

function applyAnalytics(
	html: string,
	file: string,
	enabled: boolean | undefined,
): string {
	if (enabled !== true) return html;
	return injectAnalytics(html, file);
}

function injectAnalytics(html: string, file: string): string {
	const closingBody = "</body>";
	if (!html.includes(closingBody)) {
		throw new Error(`missing ${closingBody} in ${file}`);
	}
	return html.replace(closingBody, `${analyticsSnippet}\n\t</body>`);
}

export const meta = { dist, src, pub, root, version: tokens.version };

if (import.meta.main) {
	await build({ analytics: true });
	console.log(`built v${tokens.version} → ${dist}`);
}
