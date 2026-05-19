import { execSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";
import { commands, site } from "./lib/content/site";

// Integration guard over the actual prerendered HTML. Unit tests proved
// the data layer; this proves the bytes that ship. Catches the classes
// of regression that have actually bitten this site: unresolved
// {{placeholders}}, missing SEO tags, broken internal nav links, and
// command-string drift in the rendered output.

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const pagesDir = join(root, ".svelte-kit", "output", "prerendered", "pages");
const routes = {
	"/": "index.html",
	"/demo/": join("demo", "index.html"),
	"/completion/": join("completion", "index.html"),
	"/why/": join("why", "index.html"),
	"/changelog/": join("changelog", "index.html"),
} as const;

const html = new Map<string, string>();

beforeAll(() => {
	// Build once if ANY expected route file is stale/absent; reuse
	// otherwise so the suite stays fast on repeat runs. Checking every
	// file (not just index.html) stops a partial build from causing an
	// ENOENT in the readFileSync loop below.
	const allPresent = Object.values(routes).every((file) => existsSync(join(pagesDir, file)));
	if (!allPresent) {
		execSync("bun run build", { cwd: root, stdio: "inherit" });
	}
	for (const [route, file] of Object.entries(routes)) {
		html.set(route, readFileSync(join(pagesDir, file), "utf8"));
	}
}, 120_000);

describe("prerendered output", () => {
	it("emits every route", () => {
		for (const route of Object.keys(routes)) {
			expect(html.get(route), `${route} prerendered`).toBeTruthy();
		}
	});

	it("contains no unresolved {{placeholders}} on marketing routes", () => {
		for (const [route, doc] of html) {
			// /changelog/ renders historical CHANGELOG prose that itself
			// documents the retired build.ts `{{var}}` templating, so a
			// literal "{{" is valid content there. The leak invariant
			// only applies to the build-data-templated marketing pages.
			if (route === "/changelog/") continue;
			expect(doc, `${route} has no {{}}`).not.toMatch(/\{\{/);
		}
	});

	it("gives every page a title and canonical link", () => {
		for (const [route, doc] of html) {
			expect(doc, `${route} <title>`).toMatch(/<title>[^<]+<\/title>/);
			expect(doc, `${route} canonical`).toMatch(
				/<link[^>]+rel="canonical"[^>]+href="https:\/\/runner\.kjanat\.dev/,
			);
		}
	});

	it("has the full Open Graph set on the landing page", () => {
		const home = html.get("/") ?? "";
		for (const prop of ["og:title", "og:description", "og:url", "og:type"]) {
			expect(home, prop).toContain(`property="${prop}"`);
		}
	});

	it("renders install/completion commands from the canonical names", () => {
		const c = commands(site);
		const home = html.get("/") ?? "";
		expect(home).toContain(c.npm);
		expect(home).toContain(c.cargoBinstall);
		const completion = html.get("/completion/") ?? "";
		expect(completion).toContain(c.completionsPosix);
	});

	it("has no dangling internal links (every href resolves to a route)", () => {
		const known = new Set(Object.keys(routes));
		for (const [route, doc] of html) {
			const internal = [...doc.matchAll(/href="(\/[^"#?]*)"/g)]
				.map((m) => m[1] ?? "")
				.filter((h) => !h.startsWith("/_app") && h !== "");
			for (const href of internal) {
				const norm = href.endsWith("/") ? href : `${href}/`;
				const ok = known.has(href) || known.has(norm) || href === "/";
				expect(ok, `${route} → dangling link ${href}`).toBe(true);
			}
		}
	});

	it("keeps the footer attribution links intact", () => {
		const home = html.get("/") ?? "";
		expect(home).toContain(`crates.io/crates/${site.cratesName}`);
		expect(home).toContain(`npm.im/${site.npmName}`);
		expect(home).toContain(`mailto:${site.authorEmail}`);
		expect(home).toContain(`${site.repo}/blob/master/CHANGELOG.md`);
	});

	it("renders the parsed changelog (current version + a code span)", () => {
		const cl = html.get("/changelog/") ?? "";
		// The site's own release is in CHANGELOG.md, so it must appear.
		expect(cl).toContain(site.version);
		expect(cl).toContain("class=\"section-tag\"");
		// Scope-tolerant: Svelte adds a hashed class to scoped tags.
		expect(cl).toMatch(/<article[^>]*>\s*<h2[^>]*>/);
		// Inline markdown was rendered, not dumped raw, and the raw
		// "## [x.y.z]" / "### Group" markdown headings never leak.
		expect(cl).toContain("<code>");
		expect(cl).not.toContain("## [");
		expect(cl).not.toMatch(/^### /m);
	});
});
