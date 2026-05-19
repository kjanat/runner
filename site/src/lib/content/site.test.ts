import { describe, expect, it } from "vitest";
import { commands, site } from "./site";

// Guards against displayed-command drift: every install/completion
// string the UI shows must be built from the canonical package names
// in site.ts (sourced from Cargo.toml at build time), never hardcoded.

describe("site data", () => {
	it("has the canonical generated values", () => {
		expect(site.cratesName).toBe("runner-run");
		expect(site.npmName).toBe("runner-run");
		expect(site.repoShort).toBe("kjanat/runner");
		expect(site.repo).toBe("https://github.com/kjanat/runner");
		expect(site.repo).not.toMatch(/\/$|\.git$/);
		expect(site.version).toMatch(
			/^\d+\.\d+\.\d+(?:-[0-9A-Za-z-.]+)?(?:\+[0-9A-Za-z-.]+)?$/,
		);
	});

	it("derives commands from the package names (no drift)", () => {
		const c = commands(site);
		expect(c.npm).toBe(`npm install -g ${site.npmName}`);
		expect(c.cargoBinstall).toBe(`cargo binstall ${site.cratesName}`);
		expect(c.cargoInstall).toBe(`cargo install ${site.cratesName}`);
		expect(c.linuxInstaller).toContain(`/${site.repoShort}/master/install.sh`);
		expect(c.completionsPosix).toBe("eval \"$(runner completions)\"");
		expect(c.completionsPwsh).toContain("runner completions powershell");
	});
});
