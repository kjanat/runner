// Pure Keep-a-Changelog parser + restricted inline renderer. No I/O,
// no framework imports — unit-testable in isolation. File reading is
// done by routes/changelog/+page.server.ts at prerender (build) time.
//
// Skips the `[Unreleased]` block (internal post-release checklist) and
// the trailing `[x.y.z]: url` / HTML-comment reference block. Bullets
// in this changelog wrap with a 2-space continuation indent; those
// continuation lines are folded back into one logical item.

export interface ChangelogGroup {
	/** "Added" | "Changed" | "Fixed" | … */
	name: string;
	/** Raw inline-markdown items, continuation lines folded. */
	items: string[];
}

export interface ChangelogRelease {
	/** e.g. "0.11.0". */
	version: string;
	/** e.g. "2026-05-19" or "TBD" or "". */
	date: string;
	groups: ChangelogGroup[];
}

const RELEASE_RE = /^##\s+\[([^\]]+)\]\s*(?:-\s*(.+?))?\s*$/;
const GROUP_RE = /^###\s+(.+?)\s*$/;
const ITEM_RE = /^[-*]\s+(.+?)\s*$/;
const REF_OR_COMMENT_RE = /^(\[[^\]]+\]:\s|<!--)/;

export function parseChangelog(md: string): ChangelogRelease[] {
	const releases: ChangelogRelease[] = [];
	let release: ChangelogRelease | undefined;
	let group: ChangelogGroup | undefined;
	let item: string[] | undefined;

	const flushItem = (): void => {
		if (release && group && item && item.length > 0) {
			group.items.push(item.join(" ").replace(/\s+/g, " ").trim());
		}
		item = undefined;
	};

	for (const line of md.split("\n")) {
		if (REF_OR_COMMENT_RE.test(line.trim())) {
			// `[label]: url` defs / HTML comments appear both in the
			// preamble (before any release) and as the trailing block
			// after the last release. Only the trailing block
			// terminates the parse; preamble defs are just skipped.
			if (releases.length > 0) {
				flushItem();
				break;
			}
			continue;
		}

		const rel = line.match(RELEASE_RE);
		if (rel) {
			flushItem();
			const version = rel[1] ?? "";
			if (version.toLowerCase() === "unreleased") {
				release = undefined;
				group = undefined;
				continue;
			}
			release = { version, date: rel[2]?.trim() ?? "", groups: [] };
			releases.push(release);
			group = undefined;
			continue;
		}

		if (!release) continue;

		const grp = line.match(GROUP_RE);
		if (grp) {
			flushItem();
			group = { name: grp[1] ?? "", items: [] };
			release.groups.push(group);
			continue;
		}

		const it = line.match(ITEM_RE);
		if (it) {
			flushItem();
			item = [it[1] ?? ""];
			continue;
		}

		if (item && /^\s+\S/.test(line)) {
			item.push(line.trim());
			continue;
		}

		if (line.trim() === "") flushItem();
	}

	flushItem();
	return releases.filter((r) => r.groups.some((g) => g.items.length > 0));
}

export type InlineToken =
	| { kind: "text"; value: string }
	| { kind: "code"; value: string }
	| { kind: "strong"; value: string }
	| { kind: "link"; value: string; href: string };

/**
 * Split one item's restricted inline markdown into typed tokens:
 * `code` (including double-backtick spans containing a single
 * backtick, e.g. `` `cmd` ``), **bold**, CommonMark autolinks
 * `<https://…>`, bare http(s) URLs, and plain text. Trailing
 * `.,;:!?` after a bare URL stay as text. Manual scanner because
 * a single regex cannot handle double-backtick code spans without
 * leaving a stray backtick that swallows arbitrary downstream text.
 *
 * The component renders these as real elements, so there is no
 * `{@html}` and no escaping concern — Svelte escapes text bindings.
 */
export function tokenizeInline(md: string): InlineToken[] {
	const tokens: InlineToken[] = [];
	let text = "";
	const flushText = (): void => {
		if (text.length > 0) {
			tokens.push({ kind: "text", value: text });
			text = "";
		}
	};

	let i = 0;
	while (i < md.length) {
		const ch = md[i] ?? "";

		// Code span: N backticks open, N backticks close. Per CommonMark,
		// one leading + one trailing space is stripped if present.
		if (ch === "`") {
			let n = 0;
			while (md[i + n] === "`") n++;
			const close = md.indexOf("`".repeat(n), i + n);
			const isExact = close !== -1 && md[close + n] !== "`";
			if (isExact) {
				let content = md.slice(i + n, close);
				if (
					content.startsWith(" ") && content.endsWith(" ") && content.trim() !== ""
				) {
					content = content.slice(1, -1);
				}
				flushText();
				tokens.push({ kind: "code", value: content });
				i = close + n;
				continue;
			}
		}

		// Bold: **text**.
		if (ch === "*" && md[i + 1] === "*") {
			const end = md.indexOf("**", i + 2);
			if (end > i + 2) {
				flushText();
				tokens.push({ kind: "strong", value: md.slice(i + 2, end) });
				i = end + 2;
				continue;
			}
		}

		// CommonMark autolink: <https://…> or <http://…>. Strip the
		// angle brackets entirely — they're markup, not content.
		if (ch === "<") {
			const rest = md.slice(i + 1);
			const m = rest.match(/^(https?:\/\/[^\s<>]+)>/);
			if (m && m[1]) {
				flushText();
				tokens.push({ kind: "link", value: m[1], href: m[1] });
				i += 1 + m[0].length;
				continue;
			}
		}

		// Bare http(s) URL. Trailing sentence punctuation falls back to
		// text so e.g. "see https://x.dev." doesn't capture the period.
		if ((ch === "h" || ch === "H") && /^https?:\/\//i.test(md.slice(i))) {
			const m = md.slice(i).match(/^https?:\/\/[^\s<>)]+/);
			if (m) {
				const url = m[0].replace(/[.,;:!?]+$/, "");
				if (url.length > 0) {
					flushText();
					tokens.push({ kind: "link", value: url, href: url });
					i += url.length;
					continue;
				}
			}
		}

		text += ch;
		i++;
	}
	flushText();
	return tokens;
}
