# Discoveries — runner site

Hard-won, repo-specific findings. Read before "finishing" the CSS
scoping or touching the terminal sections.

## Svelte scoped styles never reach `{@html}` content

`Demo.svelte` and `Completion.svelte` render their terminal
transcripts as trusted static strings via `{@html}` (the
Svelte/dprint formatter reflows element markup and corrupts the
whitespace-exact ASCII otherwise — see those components' comments).

Svelte adds its scope hash only to elements in a component's
*template*. `{@html}` output is injected at runtime and gets **no
scope class**. Therefore every `.term`, `.term .dim/.bold/.green/
.link/.arrow/.cursor/.prompt` rule **must stay in global
`app.css`** (or be wrapped in `:global()`). Moving them into a
component `<style>` silently unstyles the terminal.

## Some display classes are shared — keep them global

`.section-tag` (5 sections), `.tagline` (Wordmark + Completion +
404), `.meta` (most sections + 404), `.install` (Install +
Completion), `hr.rule` (route pages), and `.wordmark` (Wordmark
**and `routes/+error.svelte`'s 404**) are authored in more than one
component. Svelte cannot share one component's scoped style with
another, so these are global by necessity. The `.wordmark`/404
coupling was caught only by a post-refactor selector-accounting
sweep — re-run that sweep after any further scoping (`-F` = fixed-string, so
selectors like `.copy .toast` aren't treated as regexes):
`for sel in ...; do grep -rlF "$sel" src/app.css src/lib src/routes; done`

## What is legitimately scoped

`.copy*` → CopyButton, `.matrix*` → Speaks, `.why*` → Why,
`footer*` → SiteFooter, `nav.site`/`.skip-link` → +layout. These
elements are authored in exactly one component's template.

## Roadmap (not deferred indefinitely — next, in order)

1. Extract the `:root` token block into `src/lib/styles/tokens.css`
   imported by the layout (pure separation, zero render change;
   the spec's original intent). Low risk, do soon.
2. Automated a11y: `@axe-core` over every prerendered route.
3. Visual-regression guard (pixel diff) so CSS refactors stop
   relying on manual review — the gap that let the wordmark/nav
   overlap ship.
