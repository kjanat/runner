/// <reference types="node" />
const { optionalDependencies, name: pkgName } = require("#pkg");
const { platform, arch } = require("node:process");
const { dirname, join, resolve } = require("node:path");

const repo = "https://github.com/kjanat/runner";
const subPackages = Object.keys(optionalDependencies || {});

/** Formats text as a clickable hyperlink in supported terminals using OSC 8 escape sequences.
 * @param {string} url - The URL that the hyperlink points to.
 * @param {string | undefined} text - The display text for the hyperlink.
 * @returns {string} The formatted string with OSC 8 escape sequences.
 */
const osc8 = (url, text = undefined) => `\u001B]8;;${url}\u0007${text ?? url}\u001B]8;;\u0007`;

/**
 * Resolves the path to the prebuilt binary for the current platform and architecture.
 * It checks each optional sub-package for a `bin` directory containing the executable.
 * If no suitable binary is found, it throws an error with troubleshooting steps.
 *
 * @param {string} name - The base name of the binary (without extension).
 * @returns {string} The resolved path to the binary executable.
 * @throws {Error} If no suitable binary is found for the current platform and architecture.
 */
function resolveBinary(name) {
	const exe = platform === "win32" ? `${name}.exe` : name;
	const errors = [];
	for (const subPkg of subPackages) {
		let pkgJsonPath;
		try {
			pkgJsonPath = resolve(`${subPkg}/package.json`);
		} catch (err) {
			errors.push(`${subPkg}: ${err instanceof Error ? err.message : String(err)}`);
			continue;
		}
		return join(dirname(pkgJsonPath), "bin", exe);
	}

	const detail = errors.length > 0
		? "\n\nDetails of attempted resolutions:\n  - " + errors.join("\n  - ")
		: "";

	throw new Error(
		`${pkgName}: no prebuilt binary found for ${platform}-${arch}.
This usually means your package manager skipped \`optionalDependencies\`
(common with \`--no-optional\`, \`--omit=optional\`, or some Docker/CI setups).
Workarounds:
  - reinstall without: \`--no-optional\` / \`--omit=optional\`
  - install from source: \`cargo install --git=${repo}/ runner\`
  - file an issue if your platform is unsupported: ${osc8(`${repo}/issues`)}${detail}
`,
	);
}

module.exports = { resolveBinary };
