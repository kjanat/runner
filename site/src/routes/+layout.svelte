<script lang="ts">
import { afterNavigate } from "$app/navigation";
import { page } from "$app/state";
import { live } from "$lib/announce.svelte";
import { site } from "$lib/content/site";
import SiteFooter from "$lib/sections/SiteFooter.svelte";
import "../app.css";

let { children } = $props();

const nav = [
	{ href: "/", label: "home" },
	{ href: "/demo/", label: "demo" },
	{ href: "/completion/", label: "completion" },
	{ href: "/why/", label: "why" },
];

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
		{#each nav as item (item.href)}
			<a
				href={item.href}
				aria-current={page.url.pathname === item.href
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
