/// <reference types="node" />
"use strict";

const { optionalDependencies, name: pkgName } = require("#pkg");
const { platform, arch } = require("node:process");
const { dirname, join } = require("node:path");
const { existsSync } = require("node:fs");

const repo = "https://github.com/kjanat/runner";
const subPackages = Object.keys(optionalDependencies || {});

/** Formats text as a clickable hyperlink in supported terminals using OSC 8 escape sequences.
 * @param {string} url - The URL that the hyperlink points to.
 * @param {string} text - The display text for the hyperlink. Defaults to the URL if not provided.
 * @returns {string} The formatted string with OSC 8 escape sequences.
 */
const osc8 = (url, text = url) => `\u001B]8;;${url}\u0007${text}\u001B]8;;\u0007`;

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
	const errors = [];
	for (const subPkg of subPackages) {
		let pkgJsonPath;
		try {
			pkgJsonPath = require.resolve(`${subPkg}/package.json`);
		} catch {
			// Keep this to a single line. Node appends a multi-frame
			// "Require stack" to every MODULE_NOT_FOUND message; across a
			// dozen optional deps that buries the one package that matters
			// (the current platform's) under identical stacks. The failure
			// is always module-not-found here, so the message adds nothing.
			errors.push(`${subPkg}: not installed`);
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

	// Lead with the package that *should* satisfy this host so it isn't lost
	// among the other-platform candidates; the per-arch names embed
	// `<platform>-<arch>`, so a substring match picks the relevant ones
	// (both libc variants on Linux). Fall back to the full list if nothing
	// matches the naming convention.
	const wanted = `${platform}-${arch}`;
	const matching = errors.filter((line) => line.includes(wanted));
	const shown = matching.length > 0 ? matching : errors;
	const detail = shown.length > 0
		? `\n\nExpected a ${wanted} package; attempted resolutions:\n  - ` + shown.join("\n  - ")
		: "";

	const [indent, blueText, redText, yellowText, reset] = ["  ", "\x1b[36m", "\x1b[31m", "\x1b[33m", "\x1b[0m"];

	const errorText =
		`${redText}${pkgName}${reset}: no prebuilt binary found for ${yellowText}${platform}-${arch}${reset}.

This usually means your package manager skipped ${blueText}optionalDependencies${reset}
(common with ${blueText}--no-optional${reset}, ${blueText}--omit=optional${reset}, or some Docker/CI setups).

Workarounds:
${indent}- reinstall without: ${blueText}--no-optional${reset} / ${blueText}--omit=optional${reset}
${indent}- install from source: ${blueText}cargo install --git=${repo}/ runner${reset}
${indent}- file an issue if your platform is unsupported: ${osc8(`${repo}/issues`)}${detail}
`;

	console.error(errorText);

	throw new Error("No prebuilt binary found for the current platform and architecture.");
}

module.exports = { resolveBinary };
