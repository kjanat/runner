<script lang="ts">
import type { SiteData } from "$lib/content/site";

let { site }: { site: SiteData } = $props();
</script>

<header>
	<h1 class="wordmark">runner</h1>
	<p class="tagline">
		Universal project task runner. Auto-detects toolchain, provides unified CLI.
	</p>
	<p class="meta">
		v{site.version} ·
		<a href={site.repo} rel="external noopener noreferrer">{site.repoShort}</a>
		· {site.license}
	</p>
</header>
