<script lang="ts">
import { site } from "$lib/content/site";
import Install from "$lib/sections/Install.svelte";
import Wordmark from "$lib/sections/Wordmark.svelte";

const title = "runner — universal project task runner";
const description = "One CLI across npm, yarn, pnpm, bun, cargo, deno, uv, poetry, and more. Auto-detects toolchain.";
</script>

<svelte:head>
	<title>{title}</title>
	<meta name="description" content={description} />
	<link rel="canonical" href="https://runner.kjanat.dev/" />
	<meta property="og:title" content={title} />
	<meta
		property="og:description"
		content="One CLI for every toolchain. npm install -g {site.npmName}"
	/>
	<meta property="og:url" content="https://runner.kjanat.dev/" />
	<meta property="og:type" content="website" />
</svelte:head>

<Wordmark {site} />
<hr class="rule" />
<Install {site} />
<hr class="rule" />
<p class="meta">
	See it run → <a href="/demo/">what it looks like</a> ·
	<a href="/completion/">tab completion</a> ·
	<a href="/why/">why it's not shit</a>
</p>
