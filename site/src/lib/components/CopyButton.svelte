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
		// ok stays false
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

<style>
/* Scoped: every .copy* element is authored in this template only.
 * The .install grid wrapper stays global (shared with Completion). */
.copy {
	position: relative;
	min-width: 0;
	max-width: 100%;
	padding: 0.85rem 1rem;
	font: inherit;
	color: var(--ink);
	text-align: left;
	cursor: pointer;
	background: transparent;
	border: 1px solid var(--ink);
	border-radius: var(--radius-sm);
	transition: background var(--dur-fast) linear, color var(--dur-fast) linear;
}
.copy:hover {
	color: var(--bg);
	background: var(--ink);
}
.copy:focus-visible {
	outline: var(--ring-width) solid var(--tomato);
	outline-offset: var(--ring-offset);
}
.copy.pri {
	border-color: var(--tomato);
}
.copy.pri:hover {
	color: var(--bg);
	background: var(--tomato);
	border-color: var(--tomato);
}
.copy .label {
	display: block;
	margin-bottom: 0.25rem;
	font-size: var(--text-micro);
	color: var(--dim);
	text-transform: uppercase;
	letter-spacing: var(--ls-caps);
}
.copy:hover .label {
	color: inherit;
	opacity: 0.7;
}
.copy .cmd {
	display: block;
	overflow: hidden;
	font-size: 0.95rem;
	text-overflow: ellipsis;
	white-space: nowrap;
}
.copy .cmd-split {
	display: flex;
}
.copy .cmd-split::before {
	content: "$\00a0";
}
.copy .cmd-split .cmd-shrink {
	overflow: hidden;
	text-overflow: ellipsis;
	min-width: 0;
}
.copy .cmd::before {
	content: "$\00a0";
	opacity: 0.5;
}
.copy .toast {
	position: absolute;
	top: 0.4rem;
	right: 0.55rem;
	visibility: hidden;
	font-size: var(--text-micro);
	color: var(--moss);
	text-transform: uppercase;
	letter-spacing: var(--ls-caps);
	pointer-events: none;
	opacity: 0;
	transform: translateY(-2px);
	transition: opacity var(--dur-base) linear, transform var(--dur-base) linear, visibility 0s linear var(--dur-base);
}
.copy.copied .toast {
	visibility: visible;
	opacity: 1;
	transform: translateY(0);
	transition: opacity var(--dur-base) linear, transform var(--dur-base) linear, visibility 0s linear 0s;
}
.copy:hover .toast {
	color: var(--bg);
}
@media (max-width: 40rem) {
	.copy {
		padding: 0.7rem 0.85rem;
	}
	.copy .cmd {
		overflow-x: visible;
		font-size: 0.85rem;
		overflow-wrap: anywhere;
		white-space: normal;
	}
	.copy .cmd-split {
		display: block;
	}
	.copy .label {
		white-space: normal;
	}
}
</style>
