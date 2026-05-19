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
