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

/**
 * Render one item's restricted inline markdown to safe HTML. HTML is
 * escaped first, then a fixed allow-list is reintroduced: `code`,
 * **bold**, and autolinked bare http(s) URLs. Source is our own
 * changelog, but escaping first keeps this injection-safe regardless
 * (it is fed to `{@html}`).
 */
export function renderInline(md: string): string {
	let s = md
		.replace(/&/g, "&amp;")
		.replace(/</g, "&lt;")
		.replace(/>/g, "&gt;")
		.replace(/"/g, "&quot;");
	s = s.replace(/`([^`]+)`/g, "<code>$1</code>");
	s = s.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
	s = s.replace(
		/(^|[\s(])(https?:\/\/[^\s<)]+)/g,
		(_m, pre: string, url: string) => `${pre}<a href="${url}" rel="noopener noreferrer">${url}</a>`,
	);
	return s;
}
