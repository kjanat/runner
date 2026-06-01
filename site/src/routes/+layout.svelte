<script lang="ts">
import { afterNavigate } from "$app/navigation";
import { resolve } from "$app/paths";
import { page } from "$app/state";
import { live } from "$lib/announce.svelte";
import { site } from "$lib/content/site";
import SiteFooter from "$lib/sections/SiteFooter.svelte";
import "../app.css";

let { children } = $props();

// Trailing slashes are CANONICAL — `+layout.ts` sets
// `trailingSlash = "always"`. `resolve()` does not auto-append, so
// hrefs without the slash hit a 307 redirect AND, for any route with
// a `+page.server.ts` (just `/changelog/` today), make SvelteKit's
// client-side router fetch `<route>__data.json` (no slash) → 404 →
// the click silently aborts. Keep the slashes here.
const nav = [
	{ id: "/", label: "home" },
	{ id: "/demo/", label: "demo" },
	{ id: "/completion/", label: "completion" },
	{ id: "/why/", label: "why" },
	{ id: "/changelog/", label: "changelog" },
] as const;

const norm = (p: string) => (p.length > 1 ? p.replace(/\/$/, "") : p);

let mainEl: HTMLElement | undefined = $state();

// Move focus to <main> on client-side navigation so keyboard/SR
// users aren't stranded at the old position; the polite live region
// announces the new page title.
afterNavigate(({ from }) => {
	if (from) {
		mainEl?.focus();
		live.message = `${document.title} — loaded`;
	}
});
</script>

<a class="skip-link" href="#main">Skip to content</a>

<p
	class="visually-hidden"
	role="status"
	aria-live="polite"
	aria-atomic="true"
>
	{live.message}
</p>

<main id="main" bind:this={mainEl} tabindex="-1">
	<nav class="site" aria-label="Primary">
		{#each nav as item (item.id)}
			<a
				href={resolve(item.id)}
				aria-current={norm(page.url.pathname) === norm(resolve(item.id))
				? "page"
				: undefined}
			>
				{item.label}
			</a>
		{/each}
	</nav>

	{@render children?.()}

	<SiteFooter {site} />
</main>

<style>
/* Scoped: skip link + primary nav are authored only in this
 * layout. .visually-hidden stays global (generic utility). */
.skip-link {
	position: absolute;
	left: 0.5rem;
	top: -3rem;
	z-index: 10;
	padding: 0.5rem 0.85rem;
	color: var(--bg);
	background: var(--ink);
	border-radius: var(--radius-sm);
	transition: top var(--dur-base) linear;
}
.skip-link:focus {
	top: 0.5rem;
}
nav.site {
	position: relative;
	z-index: 1;
	display: flex;
	flex-wrap: wrap;
	gap: 0.25rem 1.25rem;
	/* Clear separation from the hero: the wordmark is up to 6.5rem
	 * at line-height 0.95, so its glyphs overflow their box upward.
	 * A bare top margin let them render over these links. */
	margin: 0 0 3.25rem;
	font-size: 0.875rem;
}
nav.site a[aria-current="page"] {
	color: var(--tomato);
}
</style>
