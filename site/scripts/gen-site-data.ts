#!/usr/bin/env bun
// Build-time site data generator (CLI wrapper).
//
// Reads the workspace-root Cargo.toml and emits a typed
// `src/lib/content/generated.ts`. All parsing is in the pure,
// unit-tested `site-data.ts`; this file is just Bun file I/O + the
// loud-failure exit so a stale/empty version can never ship.

import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { CargoParseError, parseCargoToml, renderGenerated } from "./site-data";

const here = dirname(fileURLToPath(import.meta.url));
const cargoPath = join(here, "..", "..", "Cargo.toml");
const outPath = join(here, "..", "src", "lib", "content", "generated.ts");

function die(msg: string): never {
	console.error(`gen-site-data: ${msg}`);
	process.exit(1);
}

const raw = await Bun.file(cargoPath).text().catch(() => die(`cannot read ${cargoPath}`));

let body: string;
try {
	const data = parseCargoToml(raw);
	body = renderGenerated(data);
	await Bun.write(outPath, body);
	console.log(`gen-site-data: wrote ${outPath} (v${data.version})`);
} catch (err) {
	die(
		err instanceof CargoParseError
			? err.message
			: `unexpected: ${err instanceof Error ? err.message : String(err)}`,
	);
}
