# site

Source for [runner.kjanat.dev](https://runner.kjanat.dev/).

One static page, served by a Cloudflare Worker via the Static Assets
binding. No build step, no deps — `public/index.html` ships as-is.

## Deploy

```sh
bun --cwd=site deploy
```

First deploy creates the `runner-site` Worker and provisions the
custom-domain route `runner.kjanat.dev` (Cloudflare auto-issues the
TLS cert; DNS is managed in the same zone).

## Local

```sh
bun --cwd=site dev          # http://localhost:8787
```

Or open `public/index.html` directly in a browser — it's a single,
self-contained file.

## Layout

```text
site/
├── public/
│   ├── index.html        # the whole site
│   ├── 404.html          # branded not-found page
│   ├── _headers          # cache + security headers (CF native)
│   └── robots.txt
└── wrangler.jsonc        # Static Assets binding, custom domain
```
