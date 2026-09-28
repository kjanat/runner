# Site rewrite: SvelteKit, composable, a11y-first

Date: 2026-05-19
Branch: `site/sveltekit-rewrite`
Status: design approved, pending spec review

## Problem

`site/` is a hand-rolled static site: a 211-line single-page
`src/index.html` with `{{var}}` placeholders, custom `build.ts` /
`dev.ts` Bun scripts doing the templating and bundling, plain CSS, and
a 23-line `copy.ts` for clipboard buttons. Deployed to Cloudflare
Workers via `wrangler` at `runner.kjanat.dev`.

Two problems: (1) the build pipeline is bespoke and hard to reason
about ("scripts and fuckery"); (2) the front page crams seven distinct
content blocks into one cluttered scroll.

## Goals

- Replace the bespoke build with a standard framework.
- Decompose the page into self-contained blocks that can be freely
  recomposed into pages ("composable for easier toying around").
- Preserve the existing visual design language (no redesign); the
  front page may change moderately, need not be pixel-identical.
- First-class accessibility.
- Keep the Cloudflare deploy target, domain, and edge headers.

## Non-goals (YAGNI)

No CMS, no i18n, no analytics rework, no docs system, no visual
redesign, no new content. Same content and look, recomposed.

## Stack

- SvelteKit 2 + Svelte 5 (runes) + TypeScript.
- `@sveltejs/adapter-cloudflare`.
- **Fully prerendered**: `export const prerender = true` in the root
  layout. Output is static; no runtime server. Rationale: a marketing
  site has no per-request state; prerender maximizes load speed and SEO
  and keeps the Cloudflare deploy a static upload.
- Vite dev server replaces `dev.ts`; `vite build` replaces `build.ts`.
  Both scripts are deleted.

## Architecture: composable blocks

The composability is the architecture. Three layers:

### 1. Content — single typed source

`src/lib/content/site.ts` exports one typed object:

```ts
export interface SiteData {
	npmName: string;
	cratesName: string;
	repo: string; // https://github.com/kjanat/runner
	version: string; // e.g. "0.11.0"
	domain: string; // runner.kjanat.dev
}
```

`version` (and any other release-derived value) is resolved **at build
time** from the workspace root `Cargo.toml` and emitted into a
generated, git-ignored `src/lib/content/generated.ts` by a prebuild
step (`scripts/gen-site-data.ts`, run via the `prebuild` npm script and
in CI before `vite build`). `site.ts` imports the generated values and
re-exports the typed `SiteData`. This replaces `build.ts`'s `{{var}}`
string templating with a typed module — single source, never stale,
fails the build loudly if `Cargo.toml` can't be read or parsed.

### 2. Sections — one self-contained component per block

`src/lib/sections/`, one component per current block:

- `Wordmark.svelte` — wordmark + tagline + meta (the `<header>`).
- `Install.svelte` — install command buttons.
- `Demo.svelte` — the "what it looks like" terminal transcript.
- `Completion.svelte` — completion setup + transcript.
- `Speaks.svelte` — the "it speaks" PM/runner support matrix.
- `Why.svelte` — the "why it's not shit" rationale list.
- `SiteFooter.svelte` — the footer.

Each component: owns its markup and component-scoped `<style>`, takes
typed props (data it needs is passed in, not reached for), no knowledge
of which page it sits on. Acceptance per component: it can be dropped
into any page with its props and render correctly in isolation.

Shared interaction: the `copy.ts` clipboard logic becomes a reusable
`<CopyButton>` component plus a `use:copyable` Svelte action, consumed
by `Install` and `Completion`. It keeps the existing
`aria-live`/`role="status"` copied-feedback behavior (see a11y).

### 3. Routes — thin compositions

`src/routes/` pages are thin: a page is an ordered list of section
components. Default composition:

- `/` — `Wordmark`, `Install`, a condensed `Demo` teaser, nav links.
- `/demo` — `Demo`, `Speaks`.
- `/completion` — `Completion`.
- `/why` — `Why`.
- `SiteFooter` + nav in the root layout (present on every page).

Re-splitting the site later = editing the composition in one
`+page.svelte`; no section internals change. This is the explicit
payoff of the block design.

## Styling

- `src/lib/styles/tokens.css` — design tokens (color, type scale,
  spacing, radii) extracted from the current CSS, imported once in the
  root layout. The existing look is preserved; centralizing tokens
  makes a future tweak a one-file change.
- Global resets/base from current `base.css` → root layout global
  stylesheet. Section-specific rules move into the relevant
  component's scoped `<style>`. `index.css`/`404.css` are ported, not
  reinvented; the rendered design language is unchanged.

## Accessibility (first-class)

- Semantic landmarks: one `<main>` per page, `<nav>` for site nav,
  `<header>`/`<footer>` in the layout; every section labelled
  (`aria-labelledby` on its heading), preserving the current pattern.
- Skip-to-content link as the first focusable element in the layout.
- Copy buttons: real `<button>`, keyboard-operable, with the existing
  `role="status"` `aria-live="polite"` region announcing
  copied/failed. Visible focus styles (no focus suppression).
- Route changes: move focus to the page `<h1>`/`<main>` and announce
  via a polite live region so keyboard/SR users aren't stranded
  (SvelteKit `afterNavigate`).
- `prefers-reduced-motion`: the terminal cursor/typing animation and
  any transitions are disabled under the media query.
- Color contrast: tokens chosen/verified to meet WCAG 2.1 AA
  (≥ 4.5:1 body text, ≥ 3:1 large text/UI).
- The wordmark SVG has an accessible name; decorative SVGs are
  `aria-hidden`.

## Testing (scaled to a marketing site — light but real)

- `svelte-check` + `tsc` clean (no `any`, no suppressions).
- `vite build` prerenders **every** route with zero warnings; CI fails
  on prerender warnings or unresolved internal links.
- Unit: `src/lib/content/site.ts` exposes the canonical names; a test
  asserts every install/completion command string in `Install`/
  `Completion` is built from `site.ts` values (no hardcoded drift).
- Accessibility: automated `axe-core` scan (Playwright +
  `@axe-core/playwright`) over every prerendered route, zero
  violations; a keyboard-only smoke test that the skip link works and
  copy buttons are reachable and announce.
- `scripts/gen-site-data.ts` has a test for the Cargo.toml→version
  parse, including the failure path (missing/invalid manifest → build
  error, not a silent empty version).

## Deploy

- `@sveltejs/adapter-cloudflare`; `wrangler.jsonc` updated to serve the
  adapter output. Domain `runner.kjanat.dev` unchanged.
- `public/_headers` and `public/robots.txt` carried over (SvelteKit
  `static/`).
- `site/package.json` scripts: `dev` → `vite dev`, `build` →
  `prebuild` (gen-site-data) + `vite build`, `deploy` → build +
  `wrangler deploy`, `check` → `svelte-check`, plus `test`. `build.ts`,
  `dev.ts`, old `copy.ts`, `biome.json` if superseded by the SvelteKit
  toolchain — removed once their function is replaced (not before).

## Risks / decisions

- Prerender vs SSR: prerender chosen; no dynamic data exists. If a
  future need arises it is a one-line adapter/route change, not a
  rearchitecture.
- Build-time `version`: depends on the site building from within the
  repo (it does, in CI and locally). A detached build would fail
  loudly — acceptable and preferable to a stale value.
- Toolchain churn (Biome vs SvelteKit's eslint/prettier story):
  resolved during planning; not a design fork.

## Out-of-scope cleanup explicitly deferred

The stale `site/dist/` and `site/.wrangler/` artifacts in the working
tree are not part of this design; their handling (gitignore/removal)
is an implementation-plan task, noted so it is not lost.
