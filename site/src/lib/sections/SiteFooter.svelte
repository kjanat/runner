<script lang="ts">
import type { SiteData } from "$lib/content/site";

let { site }: { site: SiteData } = $props();
</script>

<footer>
	{site.license} · v{site.version} · by
	<a href="mailto:{site.authorEmail}">{site.authorName}</a> ·
	<a href={site.repo}>source</a> ·
	<a href="{site.repo}/blob/master/CHANGELOG.md">changelog</a> ·
	<a href="https://crates.io/crates/{site.cratesName}">crates.io</a> ·
	<a href="https://npm.im/{site.npmName}">npm</a>
</footer>

<style>
/* Scoped: the <footer> element + its links are authored only here.
 * Footer links keep the dim colour; hover falls through to the
 * global `a:hover` (unchanged from the original cascade). */
footer {
	padding-top: 1.25rem;
	margin-top: 4rem;
	font-size: 0.85rem;
	color: var(--dim);
	border-top: 1px solid var(--ink);
}
footer a {
	color: var(--dim);
}
</style>
