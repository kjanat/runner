<script lang="ts">
import { site } from "$lib/content/site";
import Demo from "$lib/sections/Demo.svelte";
import Speaks from "$lib/sections/Speaks.svelte";
</script>

<svelte:head>
	<title>runner — what it looks like</title>
	<meta
		name="description"
		content="runner detects your toolchain and aggregates tasks across 12 package managers, 7 task runners, 8 sources."
	/>
	<link rel="canonical" href="https://runner.kjanat.dev/demo/" />
</svelte:head>

<Demo {site} />
<hr class="rule" />
<Speaks />
