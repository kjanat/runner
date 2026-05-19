<section aria-labelledby="why-tag">
	<span class="section-tag" id="why-tag">why it's not shit</span>
	<ol class="why">
		<li>
			Auto-detection picks the right tool from your lockfiles. No flags, no config.
		</li>
		<li>
			One CLI across npm, yarn, pnpm, bun, cargo, deno, uv, poetry, pipenv, go, bundler, composer.
		</li>
		<li>
			Aggregates tasks from package.json, turbo.json(c), Makefile, justfile, Taskfile, deno.json(c), bacon.toml — qualified syntax (<code>run
				package.json:test</code>) when names collide.
		</li>
		<li>
			Monorepo-aware: turbo, nx, pnpm, npm/yarn workspaces, Cargo workspaces.
		</li>
		<li>
			No task by that name? Falls through to the package manager's exec primitive — <code>npx</code>, <code>bunx</code>,
			<code>pnpm exec</code>, <code>uv run</code>, etc.
		</li>
	</ol>
</section>

<style>
/* Scoped: .why* is authored only here. */
.why {
	padding: 0;
	margin: 0.5rem 0 0;
	list-style: none;
	counter-reset: why;
}
.why li {
	position: relative;
	padding: 0.5rem 0 0.5rem 3.25rem;
	counter-increment: why;
	border-top: 1px dashed var(--rule);
}
.why li:first-child {
	padding-top: 0.25rem;
	border-top: 0;
}
.why li:last-child {
	padding-bottom: 0.25rem;
}
.why li::before {
	position: absolute;
	top: 0.55rem;
	left: 0;
	font-size: 0.78rem;
	color: var(--dim);
	letter-spacing: 0.06em;
	content: counter(why, decimal-leading-zero);
}
.why li:first-child::before {
	top: 0.3rem;
}
</style>
