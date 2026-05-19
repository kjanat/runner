<script lang="ts">
import { tokenizeInline } from "$lib/changelog";

let { md }: { md: string } = $props();
const tokens = $derived(tokenizeInline(md));
</script>

<!--
	No {@html}: tokens render as real elements (Svelte auto-escapes
	text). Text tokens carry their own exact spacing; Svelte strips
	whitespace-only text nodes between these block tags, so adjacent
	tokens reassemble the source string with no injected spaces.
-->
{#each tokens as t, i (i)}{#if t.kind === "code"}<code>{t.value}</code>{:else if t.kind === "strong"}<strong>{t.value}</strong>{:else if t.kind === "link"}<a
			href={t.href}
			rel="external noopener noreferrer"
		>{t.value}</a>{:else}{t.value}{/if}{/each}
