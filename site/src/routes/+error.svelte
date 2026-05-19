<script lang="ts">
import { page } from "$app/state";
</script>

<svelte:head>
	<title>runner — {page.status} {page.error?.message ?? "error"}</title>
	<meta name="robots" content="noindex" />
</svelte:head>

<header>
	<h1 class="wordmark">{page.status}</h1>
	<p class="tagline">
		{page.error?.message ?? "Something went sideways."}
	</p>
	<p class="meta"><a href="/">← back to runner</a></p>
</header>
