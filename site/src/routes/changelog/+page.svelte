<script lang="ts">
import InlineMd from "$lib/components/InlineMd.svelte";
import type { PageData } from "./$types";

let { data }: { data: PageData } = $props();
</script>

<svelte:head>
	<title>runner — changelog</title>
	<meta
		name="description"
		content="Release history for the runner CLI, parsed from CHANGELOG.md at build time."
	/>
	<link rel="canonical" href="https://runner.kjanat.dev/changelog/" />
</svelte:head>

<section aria-labelledby="changelog-tag" class="changelog">
	<span class="section-tag" id="changelog-tag">changelog</span>
	{#each data.releases as release (release.version)}
		<article>
			<h2>
				<span class="v">{release.version}</span>
				{#if release.date}<span class="date">{release.date}</span>{/if}
			</h2>
			{#each release.groups as group (group.name)}
				<h3>{group.name}</h3>
				<ul>
					{#each group.items as item, i (i)}
						<li><InlineMd md={item} /></li>
					{/each}
				</ul>
			{/each}
		</article>
	{/each}
</section>

<style>
/* Structural rules are scoped (these elements are template
 * markup). The inline <code>/<a> inside each {@html} item are not
 * reachable by scoped CSS, but inherit the global `code`/`a`
 * styles from app.css — intentional, see site/DISCOVERIES.md. */
.changelog article {
	padding-top: 2rem;
	margin-top: 2rem;
	border-top: 1px solid var(--rule);
}
.changelog article:first-of-type {
	padding-top: 0.5rem;
	margin-top: 0;
	border-top: 0;
}
.changelog h2 {
	display: flex;
	flex-wrap: wrap;
	align-items: baseline;
	gap: 0 0.75rem;
	font-size: 1.5rem;
}
.changelog h2 .v::before {
	color: var(--tomato);
	content: "v";
}
.changelog h2 .date {
	font-size: var(--text-xs);
	font-weight: 400;
	color: var(--dim);
	text-transform: uppercase;
	letter-spacing: var(--ls-caps);
}
.changelog h3 {
	margin: 1.25rem 0 0.4rem;
	font-size: var(--text-xs);
	color: var(--dim);
	text-transform: uppercase;
	letter-spacing: var(--ls-caps);
}
.changelog ul {
	margin: 0;
	padding-left: 1.1rem;
}
.changelog li {
	margin: 0.3rem 0;
	font-size: 0.92rem;
}
</style>
