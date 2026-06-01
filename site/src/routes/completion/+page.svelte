<script lang="ts">
import { site } from "$lib/content/site";
import Completion from "$lib/sections/Completion.svelte";
</script>

<svelte:head>
	<title>runner — tab completion</title>
	<meta
		name="description"
		content="One line in your shell rc. TAB asks the project what tasks it knows, grouped by source."
	/>
	<link rel="canonical" href="https://runner.kjanat.dev/completion/" />
</svelte:head>

<Completion {site} />
