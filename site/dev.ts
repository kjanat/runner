#!/usr/bin/env bun
import type { ServerWebSocket } from "bun";
import { watch } from "node:fs/promises";
import { join, relative, resolve } from "node:path";
import { build, meta } from "./build.ts";

const port = Number(process.env.PORT ?? 3000);
const reloadPath = "/__reload";
const reloadSnippet = `\
<script>
	(() => {
		const u = (location.protocol === "https:" ? "wss" : "ws") + "://" + location.host + "${reloadPath}";
		let d = 400;
		const c = () => {
			const w = new WebSocket(u);
			w.onmessage = e => {
				if (e.data === "reload") location.reload();
			};
			w.onclose = () => {
				setTimeout(c, d = Math.min(d * 1.5, 5000));
			};
			w.onopen = () => {
				d = 400;
			};
		};
		c();
	})();
</script>`;

await build();
console.log(`built v${meta.version}`);

const sockets = new Set<ServerWebSocket<unknown>>();

const server = Bun.serve({
	port,
	development: true,
	async fetch(req, srv) {
		const url = new URL(req.url);
		if (url.pathname === reloadPath) {
			if (srv.upgrade(req)) return;
			return new Response("upgrade required", { status: 426 });
		}

		const rel = url.pathname === "/" ? "/index.html" : url.pathname;
		const target = resolve(meta.dist, `.${rel}`);
		if (relative(meta.dist, target).startsWith("..")) {
			return new Response("forbidden", { status: 403 });
		}

		const file = Bun.file(target);
		if (await file.exists()) return injectReload(file, 200);

		const fallback = Bun.file(join(meta.dist, "404.html"));
		return injectReload(fallback, 404);
	},
	websocket: {
		open(ws) {
			sockets.add(ws);
		},
		close(ws) {
			sockets.delete(ws);
		},
		message() {},
	},
});

async function injectReload(
	file: ReturnType<typeof Bun.file>,
	status: number,
): Promise<Response> {
	if (file.type.startsWith("text/html")) {
		const text = await file.text();
		const body = text.replace("</body>", `${reloadSnippet}</body>`);
		return new Response(body, {
			status,
			headers: { "content-type": "text/html; charset=utf-8" },
		});
	}
	return new Response(file, { status });
}

console.log(`dev → ${server.url}`);

let pending: ReturnType<typeof setTimeout> | null = null;
let rebuilding = false;
function scheduleRebuild(reason: string) {
	if (pending) clearTimeout(pending);
	pending = setTimeout(async () => {
		if (rebuilding) return;
		rebuilding = true;
		try {
			const t0 = performance.now();
			await build();
			console.log(
				`rebuilt (${reason}) in ${(performance.now() - t0).toFixed(0)}ms`,
			);
			for (const ws of sockets) ws.send("reload");
		} catch (err) {
			console.error("build error:", err);
		} finally {
			rebuilding = false;
		}
	}, 80);
}

const targets = [
	{ path: meta.src, recursive: true, label: "src" },
	{ path: meta.pub, recursive: true, label: "public" },
	{ path: resolve(meta.root, ".."), recursive: false, label: "Cargo.toml" },
];

for (const t of targets) {
	(async () => {
		try {
			const w = watch(t.path, { recursive: t.recursive });
			for await (const ev of w) {
				if (t.label === "Cargo.toml" && ev.filename !== "Cargo.toml") continue;
				scheduleRebuild(`${t.label}/${ev.filename ?? "?"}`);
			}
		} catch (err) {
			console.error(`watcher (${t.label}) failed:`, err);
		}
	})();
}
