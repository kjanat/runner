import { describe, expect, it } from "vitest";
import { CargoParseError, parseCargoToml, renderGenerated } from "./site-data";

const valid = `
[package]
name = "runner-run"
version = "0.11.0"
license = "MIT"
repository = "https://github.com/kjanat/runner/"
authors = ["Kaj Kowalski <info@kajkowalski.nl>"]

[package.metadata.npm]
name = "runner-run"
`;

describe("parseCargoToml", () => {
	it("parses the canonical manifest", () => {
		const d = parseCargoToml(valid);
		expect(d.version).toBe("0.11.0");
		expect(d.cratesName).toBe("runner-run");
		expect(d.npmName).toBe("runner-run");
		expect(d.license).toBe("MIT");
		// trailing slash + .git stripped, github prefix → short form
		expect(d.repo).toBe("https://github.com/kjanat/runner");
		expect(d.repoShort).toBe("kjanat/runner");
		expect(d.authorName).toBe("Kaj Kowalski");
		expect(d.authorEmail).toBe("info@kajkowalski.nl");
	});

	it("strips a .git suffix from the repo URL", () => {
		const d = parseCargoToml(
			valid.replace(
				"https://github.com/kjanat/runner/",
				"https://github.com/kjanat/runner.git",
			),
		);
		expect(d.repo).toBe("https://github.com/kjanat/runner");
	});

	it("falls back npmName → crate name when no npm metadata", () => {
		const noNpm = valid.slice(0, valid.indexOf("[package.metadata.npm]"));
		expect(parseCargoToml(noNpm).npmName).toBe("runner-run");
	});

	it("defaults license to MIT when absent", () => {
		expect(parseCargoToml(valid.replace("license = \"MIT\"\n", "")).license)
			.toBe("MIT");
	});

	for (const field of ["version", "name", "repository"] as const) {
		it(`throws CargoParseError when [package].${field} is missing`, () => {
			const broken = valid.replace(
				new RegExp(`^${field} = .*$`, "m"),
				"",
			);
			expect(() => parseCargoToml(broken)).toThrow(CargoParseError);
		});

		it(`throws when [package].${field} is empty`, () => {
			const empty = valid.replace(
				new RegExp(`^${field} = .*$`, "m"),
				`${field} = ""`,
			);
			expect(() => parseCargoToml(empty)).toThrow(CargoParseError);
		});
	}

	it("never emits a {{placeholder}} in the rendered module", () => {
		const out = renderGenerated(parseCargoToml(valid));
		expect(out).not.toMatch(/\{\{/);
		expect(out).toContain("version: \"0.11.0\"");
		expect(out).toContain("as const;");
	});
});
