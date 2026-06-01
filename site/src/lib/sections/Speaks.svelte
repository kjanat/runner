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
	<h2 class="section-tag" id="matrix-tag">it speaks</h2>
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

<style>
/* Scoped: .matrix* is authored only here. */
.matrix {
	display: grid;
	grid-template-columns: 1fr;
	gap: 1.75rem;
	margin: 0.5rem 0 0;
}
@media (min-width: 44rem) {
	.matrix {
		grid-template-columns: 1.4fr 1fr 1.4fr;
		gap: 2rem;
	}
}
.matrix h3 {
	margin-bottom: 0.5rem;
	font-size: var(--text-xs);
	color: var(--dim);
	text-transform: uppercase;
	letter-spacing: var(--ls-caps);
}
.matrix ul {
	margin: 0;
	padding: 0;
	list-style: none;
	columns: 2;
	column-gap: 1.25rem;
}
.matrix .single ul {
	columns: 1;
}
.matrix li {
	padding: 0.05rem 0;
	break-inside: avoid;
}
@media (max-width: 40rem) {
	.matrix ul {
		columns: auto;
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		font-size: 0.9rem;
	}
	.matrix li {
		padding: 0;
	}
	.matrix li + li::before {
		padding: 0 0.5rem;
		color: var(--dim);
		content: "·";
		opacity: 0.7;
	}
}
</style>
