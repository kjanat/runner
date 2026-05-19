<script lang="ts">
// Counts in the headings derive from these arrays so they can never
// drift from the list contents.
const packageManagers = [
	"npm",
	"yarn",
	"pnpm",
	"bun",
	"cargo",
	"deno",
	"uv",
	"poetry",
	"pipenv",
	"go",
	"bundler",
	"composer",
];
const taskRunners = [
	"turbo",
	"nx",
	"make",
	"just",
	"go-task",
	"mise",
	"bacon",
];
const taskSources = [
	"package.json",
	"turbo.json(c)",
	"Makefile",
	"justfile",
	"Taskfile",
	"deno.json(c)",
	"bacon.toml",
	"mise.toml",
];
</script>

<section aria-labelledby="matrix-tag">
	<span class="section-tag" id="matrix-tag">it speaks</span>
	<div class="matrix">
		<div>
			<h3>Package managers · {packageManagers.length}</h3>
			<ul>
				{#each packageManagers as pm (pm)}
					<li>{pm}</li>
				{/each}
			</ul>
		</div>
		<div class="single">
			<h3>Task runners · {taskRunners.length}</h3>
			<ul>
				{#each taskRunners as tr (tr)}
					<li>{tr}</li>
				{/each}
			</ul>
		</div>
		<div>
			<h3>Task sources · {taskSources.length}</h3>
			<ul>
				{#each taskSources as ts (ts)}
					<li>{ts}</li>
				{/each}
			</ul>
		</div>
	</div>
	<p class="meta">nx is detection-only for now.</p>
</section>
