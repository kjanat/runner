<script lang="ts">
import { announce } from "$lib/announce.svelte";

interface Props {
	/** Uppercase micro-label above the command. */
	label: string;
	/** The exact command copied to the clipboard. */
	command: string;
	/** Tomato-bordered primary styling. */
	primary?: boolean;
	/** Render the command split (prefix / shrinkable middle /
	 * suffix) so long URLs ellipsize instead of overflowing. */
	split?: { pre: string; shrink: string; post: string };
}

let { label, command, primary = false, split }: Props = $props();

let copied = $state(false);
let resetTimer: ReturnType<typeof setTimeout> | undefined;

async function copy(): Promise<void> {
	let ok = false;
	try {
		await navigator.clipboard.writeText(command);
		ok = true;
	} catch {
		ok = false;
	}
	announce(ok ? "Copied to clipboard" : "Copy failed");
	if (!ok) return;
	copied = true;
	clearTimeout(resetTimer);
	resetTimer = setTimeout(() => {
		copied = false;
	}, 1400);
}
</script>

<button
	class="copy"
	class:pri={primary}
	class:copied
	type="button"
	onclick={copy}
>
	<span class="label">{label}</span>
	{#if split}
		<span class="cmd cmd-split"><span>{split.pre}</span><span class="cmd-shrink">{split.shrink}</span><span>{split.post}</span></span>
	{:else}
		<span class="cmd">{command}</span>
	{/if}
	<span class="toast" aria-hidden="true">copied</span>
</button>
