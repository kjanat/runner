/// <reference types="node" />
"use strict";

const { optionalDependencies, name: pkgName } = require("#pkg");
const { platform, arch } = require("node:process");
const { dirname, join } = require("node:path");
const { existsSync } = require("node:fs");

const repo = "https://github.com/kjanat/runner";
const subPackages = Object.keys(optionalDependencies || {});

// ansispeck handles color and OSC 8 hyperlink capability detection (NO_COLOR,
// TTY, terminal support). It is ESM-only and require(esm) needs Node >= 20.19,
// while this package's engines contract is node >= 18: on Node 18 and Node 20
// before 20.19 (or when the install skipped the dependency) the require throws
// and diagnostics degrade to plain text without colors or OSC 8 links.
const { red, yellow, cyan, link, space } = (() => {
	try {
		return require("ansispeck");
	} catch {
		/** @param {unknown} value */
		const plain = (value) => String(value);
		return { red: plain, yellow: plain, cyan: plain, link: plain, space: () => "  " };
	}
})();

/**
 * Locate the prebuilt executable matching the current platform and architecture.
 *
 * Searches optional-dependency sub-packages for a matching `bin/<exe>` and returns its filesystem path.
 * If no candidate is found, an explanatory error message is written to stderr and an `Error` is thrown.
 *
 * @param {string} name - Base name of the executable (without platform-specific extension).
 * @returns {string} The filesystem path to the resolved executable.
 * @throws {Error} If no suitable binary is found for the current platform and architecture.
 */
function resolveBinary(name) {
	const exe = platform === "win32" ? `${name}.exe` : name;
	const missing = [];
	const errors = [];
	for (const subPkg of subPackages) {
		let pkgJsonPath;
		try {
			pkgJsonPath = require.resolve(`${subPkg}/package.json`);
		} catch (err) {
			// MODULE_NOT_FOUND is the expected miss for every platform package
			// npm skipped; a require stack per package is pure noise. Only an
			// unexpected error deserves its message, and only the first line.
			if (err && err.code === "MODULE_NOT_FOUND") {
				missing.push(subPkg);
			} else {
				errors.push(`${subPkg}: ${String(err instanceof Error ? err.message : err).split("\n")[0]}`);
			}
			continue;
		}
		const binPath = join(dirname(pkgJsonPath), "bin", exe);
		// `require.resolve` proves the package.json exists, not the binary.
		// Could mismatch if a user manually deletes the bin, or a partial
		// install half-succeeded. Prefer a clear error here over an opaque
		// `ENOENT` from `spawnSync` later in `launch.cjs`.
		if (!existsSync(binPath)) {
			errors.push(`${subPkg}: package present but bin missing at ${binPath}`);
			continue;
		}
		return binPath;
	}

	if (missing.length > 0) {
		errors.push(`not installed (${missing.length}): ${missing.join(", ")}`);
	}
	const detail = errors.length > 0
		? "\n\nDetails of attempted resolutions:\n  - " + errors.join("\n  - ")
		: "";

	const indent = space(2);

	const errorText = `${red(pkgName)}: no prebuilt binary found for ${yellow(`${platform}-${arch}`)}.

This usually means your package manager skipped ${cyan("optionalDependencies")}
(common with ${cyan("--no-optional")}, ${cyan("--omit=optional")}, or some Docker/CI setups).

Workarounds:
${indent}- reinstall without: ${cyan("--no-optional")} / ${cyan("--omit=optional")}
${indent}- bun + ${cyan("minimumReleaseAge")}: add the ${cyan("@runner-run/*")} platform packages (not just ${
		cyan(pkgName)
	}) to ${cyan("minimumReleaseAgeExcludes")}; a fresh release is otherwise age-gated
${indent}- install from source: ${cyan(`cargo install --git=${repo}/ runner`)}
${indent}- file an issue if your platform is unsupported: ${link(`${repo}/issues`)}${detail}
`;

	console.error(errorText);

	throw new Error("No prebuilt binary found for the current platform and architecture.");
}

module.exports = { resolveBinary };
