# site

Source for [runner.kjanat.com](https://runner.kjanat.com/).

Static site bundled with Bun (`build.ts`) and deployed to a Cloudflare
Worker (`name = "runner"` in `wrangler.jsonc`) via the Static Assets
binding. `src/` is the editable source; `dist/` is the build output and
the directory the Worker actually serves.

## Build

```sh
bun --cwd=site build      # bundles src/ → dist/, copies public/ on top
bun --cwd=site dev        # local dev server with WS live-reload
bun --cwd=site deploy     # build + wrangler deploy
```

The build templates `{{version}}`, `{{repo}}`, `{{authorName}}` etc.
from the workspace `Cargo.toml` so the site and the crate share one
source of truth for metadata. Other scripts live in
[`package.json`](./package.json) (`check`, `lint`, `fmt`, `typecheck`,
`tail`).

First deploy creates the `runner` Worker and provisions the
custom-domain route `runner.kjanat.com` (Cloudflare auto-issues the
TLS cert; DNS is managed in the same zone).

## Layout

```text
site/
├── src/
│   ├── index.html        # landing page (templated by build.ts)
│   ├── 404.html          # branded not-found page
│   ├── app/copy.ts       # client-side click-to-copy
│   ├── assets/icon.svg
│   └── styles/
├── public/
│   ├── _headers          # cache + security headers (CF native)
│   └── robots.txt
├── build.ts              # Bun bundler + token substitution
├── dev.ts                # dev server + watcher + WS live-reload
├── biome.json            # lint config
├── package.json          # scripts
└── wrangler.jsonc        # Static Assets binding, custom domain
```

`dist/` is gitignored — produced by `build.ts`, consumed by
`wrangler deploy`.
