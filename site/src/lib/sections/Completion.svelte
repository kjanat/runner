<script lang="ts">
import CopyButton from "$lib/components/CopyButton.svelte";
import { commands, type SiteData } from "$lib/content/site";

let { site }: { site: SiteData } = $props();
const cmd = $derived(commands(site));

// See Demo.svelte: whitespace-exact, string literal, trusted static
// content rendered with {@html}.
const transcript = `<span class="prompt"></span><span>cd ~/some-project</span>
<span class="prompt"></span><span>runner </span><span class="dim">&lt;TAB&gt;</span>
<span class="dim">-- package.json --</span>
build      <span class="dim">compile the thing</span>
test       <span class="dim">run tests</span>
dev        <span class="dim">start dev server</span>
<span class="dim">-- justfile --</span>
ci         <span class="dim">lint + tests</span>
fmt        <span class="dim">format</span>
release    <span class="dim">cut a release</span>
<span class="dim">-- justfile (aliases) --</span>
b          <span class="dim">→ build</span>
t          <span class="dim">→ test</span>
<span class="dim">-- Commands --</span>
list       <span class="dim">list tasks</span>
info       <span class="dim">show detected project</span>
clean      <span class="dim">clean build artefacts</span><span class="cursor"> </span>`;
</script>

<section aria-labelledby="completion-tag">
	<span class="section-tag" id="completion-tag">tab-fucking completion</span>
	<p class="tagline">
		Drop one line in your shell rc. Now <code>&lt;TAB&gt;</code> hits the binary and asks <em>this</em> project what tasks it knows about — grouped by source,
		with descriptions. Same line registers both <code>runner</code> and <code>run</code>.
	</p>
	<div class="install">
		<CopyButton
			label="bash · zsh · fish · auto-detects $SHELL"
			command={cmd.completionsPosix}
			primary
		/>
		<CopyButton label="powershell" command={cmd.completionsPwsh} />
	</div>
	<pre class="term">{@html transcript}</pre>
	<div class="meta">
		<p>
			Path-typed flags like <code>--dir &lt;TAB&gt;</code> delegate to the shell's own file completer.
		</p>
		<p><code>~/</code>, globs, and <code>cdpath</code> all work.</p>
	</div>
</section>
