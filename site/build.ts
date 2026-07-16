#!/usr/bin/env bun
import { env } from "bun";
import { readdir, rm } from "node:fs/promises";
import { join, relative } from "node:path";
import { brotliCompressSync, constants as zlib, gzipSync } from "node:zlib";

import cargo from "../Cargo.toml" with { type: "toml" };

const root = import.meta.dir;
const [dist, src, pub] = [join(root, "dist"), join(root, "src"), join(root, "public")];

const pkg = cargo.package;
const author = pkg.metadata.authors[0];
const repo = pkg.repository.replace(/\/$/, "");

const CF_BEACON_TOKEN = "092edc6dde124fe4816fd2d95c16db39";

const tokens: Record<string, string> = {
	version: pkg.version,
	repo,
	repoShort: repo.replace(/^https?:\/\//, ""),
	license: pkg.license,
	description: pkg.description,
	npmName: pkg.metadata.npm.name,
	cratesName: pkg.name,
	authorName: author.name,
	authorEmail: author.email,
};

const analyticsSnippet = `
	<!-- Cloudflare Web Analytics -->
	<script defer src="https://static.cloudflareinsights.com/beacon.min.js" data-cf-beacon='{"token": "${
	env["CF_BEACON_TOKEN"] || CF_BEACON_TOKEN
}"}'></script>
	<!-- End Cloudflare Web Analytics -->
`;

type DirMode = "full" | "relative";

interface BuildOptions {
	analytics?: boolean;
	dir?: DirMode;
}

export interface DistFile {
	bytes: Uint8Array;
	path: string;
}
const isCI = env["CI"] === "true" || env["CI"] === "1";
const isGithubActions = env["GITHUB_ACTIONS"] === "true";
function getGitHubRepo(_repo: string | unknown = env["GITHUB_REPOSITORY"]) {
	if (typeof _repo !== "string") return;

	function isGitHubRepo(_value: string): _value is `${string}/${string}` {
		const parts = _value.split("/");
		// Check of er exact 2 delen zijn en of beide delen content bevatten
		return parts.length === 2 && parts.every(part => part.length > 0);
	}

	if (isGitHubRepo(_repo)) {
		console.log(`Working in: ${_repo}`);
		// Omdat isGitHubRepo true is, weten we dat split() exact 2 delen geeft
		return _repo.split("/") as [string, string];
	}
}

const githubRepo = getGitHubRepo();
const isGitHubRepo = typeof githubRepo !== "undefined";

const isCIorGHactions = isCI || (isGithubActions && isGitHubRepo);

export async function build(options: BuildOptions = {}): Promise<DistFile[]> {
	await rm(dist, { recursive: true, force: true });
	const githubPagesUrl = () => {
		if (githubRepo) {
			const [owner, repo] = githubRepo;
			return `https://${owner}.github.io/${repo}/`;
		}
	};

	const publicPath = env["PUBLIC_PATH"] || githubPagesUrl() || "/";

	console.debug(`pub → ${publicPath}`);

	const result = await Bun.build({
		entrypoints: [join(src, "index.html"), join(src, "404.html")],
		outdir: dist,
		target: "browser",
		minify: true,
		publicPath,
		sourcemap: env["SENTRY_DSN"] ? "external" : false,
	});

	if (!result.success) {
		for (const log of result.logs) console.error(log);
		throw new Error("build failed");
	}

	const emptyJsOutputs = new Set(
		result.outputs.filter((out) => out.size === 0 && out.path.endsWith(".js")),
	);
	const emptyChunks = new Set<string>();
	for (const out of emptyJsOutputs) {
		const name = out.path.split("/").pop();
		if (name) emptyChunks.add(name);
	}
	await Promise.all([...emptyJsOutputs].map((out) => rm(out.path, { force: true })));
	const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
	const emptyScript = emptyChunks.size
		? new RegExp(
			`<script[^>]+src="\\.?/?(?:${[...emptyChunks].map(escapeRe).join("|")})"[^>]*></script>`,
			"g",
		)
		: null;

	const placeholder = /\{\{(\w+)\}\}/g;
	const encoder = new TextEncoder();
	const dirMode: DirMode = options.dir ?? "relative";
	const display = (abs: string, base: string) => dirMode === "full" ? abs : relative(base, abs);

	// Bun.build's outputs already hold the bundled bytes in memory, no re-read from disk.
	// HTMLs get post-processed (placeholders, analytics, dead-script pruning)
	// and rewritten; everything else stays as Bun emitted it.
	const fromBundle = await Promise.all(
		result.outputs
			.filter((out) => !emptyJsOutputs.has(out))
			.map(async (out): Promise<DistFile> => {
				const path = display(out.path, dist);
				if (out.path.endsWith(".html")) {
					let html = await out.text();
					if (emptyScript) html = html.replace(emptyScript, "");
					html = html.replace(placeholder, (raw, key: string) => {
						const value = tokens[key];
						if (value === undefined) {
							throw new Error(`unknown placeholder ${raw} in ${path}`);
						}
						return value;
					});
					html = applyAnalytics(html, path, options.analytics);
					const bytes = encoder.encode(html);
					await Bun.write(out.path, bytes);
					return { path, bytes };
				}

				return { path, bytes: new Uint8Array(await out.arrayBuffer()) };
			}),
	);

	const fromPublic = await copyTree(pub, dist, dirMode);
	return [...fromBundle, ...fromPublic];
}

// Copy `srcDir` into `destDir` recursively, returning each file's final bytes
// so the caller can compute sizes without a second read pass.
async function copyTree(
	srcDir: string,
	destDir: string,
	dirMode: DirMode,
): Promise<DistFile[]> {
	const entries = await readdir(srcDir, { recursive: true, withFileTypes: true });
	return Promise.all(
		entries
			.filter((e) => e.isFile())
			.map(async (e): Promise<DistFile> => {
				const full = join(e.parentPath, e.name);
				const rel = relative(srcDir, full);
				const dest = join(destDir, rel);
				const bytes = await Bun.file(full).bytes();
				await Bun.write(dest, bytes);
				return { path: dirMode === "full" ? dest : rel, bytes };
			}),
	);
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
	if (!isCIorGHactions) return html;
	const closingBody = "</body>";

	if (!html.includes(closingBody)) {
		console.error(`missing ${closingBody} in ${file}`);
		return html;
	}

	return html.replace(closingBody, `${analyticsSnippet}</body>`);
}

export const meta = { dist, src, pub, root, version: tokens.version };

export function summarize(files: DistFile[]): void {
	const sizes = files.map((f) => ({
		path: f.path,
		raw: f.bytes.length,
		// Mirror what a CDN actually serves: max-quality gzip + brotli.
		gzip: gzipSync(f.bytes, { level: 9 }).length,
		brotli: brotliCompressSync(f.bytes, {
			params: { [zlib.BROTLI_PARAM_QUALITY]: 11 },
		}).length,
	}));

	sizes.sort((a, b) => b.raw - a.raw);

	const totals = sizes.reduce(
		(acc, f) => ({
			raw: acc.raw + f.raw,
			gzip: acc.gzip + f.gzip,
			brotli: acc.brotli + f.brotli,
		}),
		{ raw: 0, gzip: 0, brotli: 0 },
	);

	const header = ["file", "raw", "gzip", "br"];
	const body = sizes.map((f) => [f.path, fmtSize(f.raw), fmtSize(f.gzip), fmtSize(f.brotli)]);
	const total = ["total", fmtSize(totals.raw), fmtSize(totals.gzip), fmtSize(totals.brotli)];
	const rows = [header, ...body, total];
	const widths = header.map((_, i) => Math.max(...rows.map((r) => (r[i] ?? "").length)));
	const fmtRow = (r: string[]) =>
		r
			.map((c, i) => {
				const w = widths[i] ?? 0;
				return i === 0 ? c.padEnd(w) : c.padStart(w);
			})
			.join("  ");
	const sep = "─".repeat(widths.reduce((a, b) => a + b, 0) + (widths.length - 1) * 2);

	console.log();
	console.log(fmtRow(header));
	console.log(sep);
	for (const r of body) console.log(fmtRow(r));
	console.log(sep);
	console.log(fmtRow(total));
}

function fmtSize(n: number): string {
	if (n < 1024) return `${n} B`;
	if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} K`;
	return `${(n / 1024 / 1024).toFixed(2)} M`;
}

if (import.meta.main) {
	const files = await build({
		analytics: true,
		dir: env["FULL"] ? "full" : "relative",
	});
	summarize(files);
	console.log(`
built v${tokens.version} → ${dist}`);
}
