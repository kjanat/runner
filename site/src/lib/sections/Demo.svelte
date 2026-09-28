<script lang="ts">
import type { SiteData } from "$lib/content/site";

let { site }: { site: SiteData } = $props();

// Whitespace-exact terminal transcript. Kept as a string literal,
// not template markup, because the Svelte/dprint formatter reflows
// element markup and any injected newline/indent becomes visible
// text inside <pre>. Content is static and trusted (only build-time
// site constants interpolated), so {@html} is safe here.
const transcript = $derived(`<span class="prompt"></span><span>runner</span>
<a class="bold link" href="${site.repo}/">runner</a> <a class="link" href="${site.repo}/releases/tag/v${site.version}">${site.version}</a>
  <span class="dim">Package Managers</span>    bun, pnpm
  <span class="dim">Task Runners</span>        turbo, just
  <span class="dim">Node</span>                22 (.nvmrc), current v22.11.0 <span class="green">(ok)</span>
  <span class="dim">Monorepo</span>            <span class="green">yes</span>

  <span class="bold link">package.json</span>    build               <span class="dim">compile the thing</span>
  <span class="bold link">package.json</span>    test                <span class="dim">run tests</span>
  <span class="bold link">package.json</span>    dev                 <span class="dim">start dev server</span>
  <span class="bold link">justfile</span>        ci, fmt, release
  <span class="bold">justfile (aliases)</span> b <span class="arrow">→</span> <span class="dim">build</span>, t <span class="arrow">→</span> <span class="dim">test</span>

<span class="prompt"></span><span>run test</span>
<span class="dim">› bun test</span><span class="cursor"> </span>`);
</script>

<section aria-labelledby="demo-tag">
	<h2 class="section-tag" id="demo-tag">what it looks like</h2>
	<pre class="term">{@html transcript}</pre>
</section>
