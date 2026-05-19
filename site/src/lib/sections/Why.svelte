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
