import { describe, expect, it } from "vitest";
import { parseChangelog, tokenizeInline } from "./changelog";

const sample = `# Changelog

[Keep a Changelog]: https://keepachangelog.com/

## [Unreleased]

### Post-release checklist

- [ ] do not show this

## [0.11.0] - 2026-05-19

### Added

- Task chaining for \`runner run\`. New \`-s\` /
  \`--sequential\` flags turn positionals into a chain.
- Second item.

### Fixed

- A fix referencing https://github.com/kjanat/runner/pull/32 inline.

## [0.1.0] - 2026-01-01

### Added

- Initial release.

[0.1.0]: https://github.com/kjanat/runner/releases/tag/v0.1.0
<!-- markdownlint-disable-file -->
`;

describe("parseChangelog", () => {
	const releases = parseChangelog(sample);

	it("skips the Unreleased checklist block", () => {
		expect(releases.map((r) => r.version)).toEqual(["0.11.0", "0.1.0"]);
	});

	it("captures version and date", () => {
		expect(releases[0]).toMatchObject({ version: "0.11.0", date: "2026-05-19" });
	});

	it("folds wrapped continuation lines into one item", () => {
		const added = releases[0]?.groups.find((g) => g.name === "Added");
		expect(added?.items[0]).toBe(
			"Task chaining for `runner run`. New `-s` / `--sequential` flags turn positionals into a chain.",
		);
		expect(added?.items).toHaveLength(2);
	});

	it("keeps multiple groups per release", () => {
		expect(releases[0]?.groups.map((g) => g.name)).toEqual(["Added", "Fixed"]);
	});

	it("stops at the reference-link / comment trailer", () => {
		const last = releases[releases.length - 1];
		expect(last?.groups[0]?.items).toEqual(["Initial release."]);
	});
});

describe("tokenizeInline", () => {
	it("keeps plain text as a single text token (no HTML)", () => {
		expect(tokenizeInline("a <script> b")).toEqual([
			{ kind: "text", value: "a <script> b" },
		]);
	});

	it("splits code / bold / bare-URL out of surrounding text", () => {
		expect(tokenizeInline("use `runner run` now")).toEqual([
			{ kind: "text", value: "use " },
			{ kind: "code", value: "runner run" },
			{ kind: "text", value: " now" },
		]);
		expect(tokenizeInline("**important**")).toEqual([
			{ kind: "strong", value: "important" },
		]);
		expect(tokenizeInline("see https://example.com/x done")).toEqual([
			{ kind: "text", value: "see " },
			{ kind: "link", value: "https://example.com/x", href: "https://example.com/x" },
			{ kind: "text", value: " done" },
		]);
	});

	it("does not linkify a javascript: pseudo-URL", () => {
		expect(tokenizeInline("javascript:alert(1)")).toEqual([
			{ kind: "text", value: "javascript:alert(1)" },
		]);
	});

	it("reassembles to the original text (lossless)", () => {
		const src = "mix `code`, **bold** and https://x.dev/p end";
		expect(
			tokenizeInline(src).map((t) =>
				t.kind === "code" ? `\`${t.value}\`` : t.kind === "strong" ? `**${t.value}**` : t.value
			).join(""),
		).toBe(src);
	});
});
