<script lang="ts">
import CopyButton from "$lib/components/CopyButton.svelte";
import { commands, type SiteData } from "$lib/content/site";

let { site }: { site: SiteData } = $props();
const cmd = $derived(commands(site));
const installerUrl = $derived(
	`https://raw.githubusercontent.com/${site.repoShort}/${site.defaultBranch}/install.sh`,
);
</script>

<section aria-labelledby="install-tag">
	<h2 class="section-tag" id="install-tag">install</h2>
	<div class="install">
		<CopyButton label="npm · primary" command={cmd.npm} primary />
		<CopyButton
			label="cargo · binstall · primary"
			command={cmd.cargoBinstall}
			primary
		/>
		<CopyButton
			label="cargo · build from source"
			command={cmd.cargoInstall}
		/>
		<CopyButton
			label="linux installer"
			command={cmd.linuxInstaller}
			split={{ pre: "curl -fsSL ", shrink: installerUrl, post: " | sh" }}
		/>
	</div>
	<p class="meta">
		The npm package is a façade — installs only the
		<a
			href="{site.repo}/tree/{site.defaultBranch}/npm"
			rel="external noopener noreferrer"
		>prebuilt binary</a> for your platform via <code>optionalDependencies</code>. No postinstall, no network at install time.
	</p>
	<p class="meta">
		Using <code>cargo binstall</code>? Run
		<code>cargo install cargo-binstall</code> once first. After that, every <code>cargo binstall &lt;crate&gt;</code> pulls a prebuilt binary from GitHub
		releases instead of compiling.
	</p>
</section>
