# runner

<picture height="160" align="right" alt="runner logo">
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.github.com/kjanat/runner/ea333a0e/branding/wordmark-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="https://raw.github.com/kjanat/runner/f90940f8/branding/wordmark.svg">
  <img alt="Fallback image" height="160" align="right" src="https://raw.github.com/kjanat/runner/ea333a0e/branding/wordmark-dark.svg">
</picture>

<!--
TODO: add back the supported task runners that gippity deleted out of spite
And re-read/review the readme...
-->

[![Crates.io](https://img.shields.io/crates/v/runner-run?logo=rust&labelColor=B7410E&color=black)][crates]
[![NPM](https://img.shields.io/npm/v/runner-run?logo=npm&labelColor=CB3837&color=black)][npm]
[![License: MIT](https://img.shields.io/npm/l/runner-run?color=blue)][LICENSE]

**runner** is for people who bounce between codebases and refuse to memorize
each repo’s private little task-running religion.

Instead of guessing whether this one wants `npm run`, `pnpm exec`, `bunx`,
`cargo`, `uv run`, `deno task`, `turbo`, `make`, `just`, etc. type:

```sh
run <TAB>
```

<details><summary><i><code>run</code> ran in this very project</i></summary>

```shell
❯ run
run 0.12.1

  Package Managers    bun, cargo
  Task Runners        just
  Node                24.14.1
  Monorepo            yes

  just            build-packages
  just            default
  just            gen-schema           Drift guard: just gen-schema && git diff --exit-code schemas/
  just            install
  just            ls
  just            run
  just            runner
  just            test-release         Build release bin and verify the facade shims spawn the native binary.
  cargo           b                    build
  cargo           bb                   build --bin run --bin runner
  cargo           bbr                  build --bin run --bin runner --release
  cargo           bin-run              run --quiet --bin run
  cargo           bin-runner           run --quiet --bin runner
  cargo           c                    check
  cargo           cl                   clippy --all-targets --all-features
  cargo           comp                 run --quiet --bin runner -- completions
  cargo           d                    doc
  cargo           f                    run --quiet --bin run -- --pm npm dprint fmt
  cargo           format               run --quiet --bin run -- --pm npm dprint fmt
  cargo           i                    install --path .
  cargo           l                    clippy --all-targets --all-features -- -D warnings -D clippy::all
  cargo           lint                 clippy --all-targets --all-features -- -D warnings -D clippy::all
  cargo           man                  run --quiet --features man -- man
  cargo           meta                 metadata --format-version 1
  cargo           r                    run
  cargo           rbin-run             run --quiet --bin run --release
  cargo           rbin-runner          run --quiet --bin runner --release
  cargo           rm                   remove
  cargo           rq                   run --quiet
  cargo           rr                   run --release
  cargo           runner               run --quiet --bin runner
  cargo           schema               run --quiet --features schema -- schema
  cargo           t                    test
```

and `run <TAB>` (zsh):

```shell
❯ run <TAB>
-- just --
build-packages                                                                    run
default                                                                           runner
gen-schema      -- Drift guard: just gen-schema && git diff --exit-code schemas/  just:runner
install                                                                           test-release    -- Build release bin and verify the facade shims spawn the native binary.
ls
-- cargo (aliases) --
b             -- → build                                                              lint          -- → clippy --all-targets --all-features -- -D warnings -D clippy::all
bb            -- → build --bin run --bin runner                                       man           -- → run --quiet --features man -- man
bbr           -- → build --bin run --bin runner --release                             meta          -- → metadata --format-version 1
bin-run       -- → run --quiet --bin run                                              r             -- → run
bin-runner    -- → run --quiet --bin runner                                           rbin-run      -- → run --quiet --bin run --release
c             -- → check                                                              rbin-runner   -- → run --quiet --bin runner --release
cl            -- → clippy --all-targets --all-features                                rm            -- → remove
comp          -- → run --quiet --bin runner -- completions                            rq            -- → run --quiet
d             -- → doc                                                                rr            -- → run --release
f             -- → run --quiet --bin run -- --pm npm dprint fmt                       cargo:runner  -- → run --quiet --bin runner
format        -- → run --quiet --bin run -- --pm npm dprint fmt                       schema        -- → run --quiet --features schema -- schema
i             -- → install --path .                                                   t             -- → test
l             -- → clippy --all-targets --all-features -- -D warnings -D clippy::all
-- Options --
--dir             -- Use this directory instead of the current one
--pm              -- Override the detected package manager (also reads RUNNER_PM when omitted). Valid: npm, yarn, pnpm, bun, cargo, deno, uv, poetry, pipenv, go, bundler (alias: bundle), composer
--runner          -- Override the detected task runner (also reads RUNNER_RUNNER when omitted). Valid: turbo, nx, make, just, task (alias: go-task), mise, bacon
--fallback        -- What to do when no detection signal matches: probe (default, PATH probe), npm (legacy silent fallback), error (refuse). Also reads RUNNER_FALLBACK when omitted.
--on-mismatch     -- What to do when the manifest declaration disagrees with the lockfile: warn (default), error (exit 2), ignore (silent). Also reads RUNNER_ON_MISMATCH when omitted.
--explain         -- Print a one-line trace describing how the package manager was resolved. Also enabled when RUNNER_EXPLAIN is set to a truthy value.
--no-warnings     -- Suppress all non-fatal warnings on stderr. Also enabled when RUNNER_NO_WARNINGS is set to a truthy value.
--schema-version  -- Pin JSON output schema version (1 or 2). Defaults to latest. Affects --json output of doctor/list/why only.
--sequential      -- Run the given tasks sequentially. Conflicts with `--parallel`
--parallel        -- Run the given tasks in parallel. Conflicts with `--sequential`
--keep-going      -- Run every task in the chain regardless of failures. Conflicts with `--kill-on-fail`
--kill-on-fail    -- Parallel only: SIGKILL siblings on first failure. Accepted but unused in sequential mode
--help            -- Print help
--version         -- Print version
```

---

</details>

runner detects the project, finds its tasks, and completes them through one
command.

Use the same shape everywhere:

```sh
run <TAB>
runner install test build deploy
```

Let each repo decide what the tasks actually mean.

## Install

```sh
npm install -g runner-run
```

Or:

```sh
cargo binstall runner-run
```

Or on Arch Linux:

```sh
yay -S runner-run-bin
```

<details>
<summary><i>Other install methods</i></summary>

```sh
cargo install runner-run
cargo install --git=https://github.com/kjanat/runner/ runner-run
cargo install --path .
```

```sh
# AUR source build (compiles via cargo):
yay -S runner-run
```

```sh
curl -fsSLO https://raw.githubusercontent.com/kjanat/runner/master/install.sh
bash install.sh
bash install.sh 0.12.1
bash install.sh v0.12.1
```

---

</details>

## GitHub Actions

Use the action to install runner in CI:

```yaml
- uses: kjanat/runner@master
- run: runner install --frozen test build
```

`runner install` is not a task — it runs the project's toolchain command(s)
(`npm ci`, `cargo fetch`, `uv sync`, …), then chains the listed tasks
(`test`, then `build`) sequentially.

That is the point: the workflow stays boring even when the project underneath is
npm, pnpm, bun, Cargo, Deno, uv, Make, just, or whatever automation that repo
uses.

<details>
<summary><i>Install mechanics and outputs</i></summary>

The action installs the `runner-run` npm package into the runner tool cache with
`npm install --global --ignore-scripts --prefix`, verifies the installed
`runner` shim by running `runner --version`, and adds the npm bin directory to
`PATH` for later steps.

| I/O    | name      | description                                                                           |
| ------ | --------- | ------------------------------------------------------------------------------------- |
| Input  | `version` | npm version spec for `runner-run`; defaults to `latest`; accepts numeric `v?` forms   |
| Output | `version` | Concrete version reported by the installed `runner --version` smoke test              |
| Output | `bin-dir` | npm global bin directory containing `runner` / `run`; added to `PATH` for later steps |

Exact `X.Y.Z` pins are checked against the executed CLI version; a mismatch
fails the action.

---

</details>

## Usage

```sh
runner                              # show detected project info
runner <task> [-- <args...>]        # run a task
runner run <target> [-- <args...>]  # run a task or command
run <target> [-- <args...>]         # alias for `runner run`

runner install [--frozen]           # install dependencies
runner clean [-y] [--include-framework]
runner list [--raw] [--json]        # list available tasks
runner info [--json]                # show detected project info
runner doctor [--json]              # show every resolver signal
runner why <task> [--json]          # explain how a task would dispatch
runner completions [<shell>] [-o <path>]
```

## Completions

`runner completions` generates dynamic shell completion registrations.

For bash, zsh, and fish, runner can auto-detect `$SHELL`:

```sh
eval "$(runner completions)"
```

<details>
<summary><i>...or get explicit with it</i></summary>

```sh
eval "$(runner completions bash)"
eval "$(runner completions zsh)"
eval "$(runner completions fish)"
```

---

</details>

### PowerShell

```powershell
runner completions powershell | Out-String | Invoke-Expression
```

The generated registration includes `runner` and, when the sibling `run` binary
exists next to it, `run` too.

So after setup, this is the workflow:

```sh
run <TAB>
```

No per-project command archaeology. No guessing whether this one wants npm,
Cargo, Make, just, Deno, uv, or some handcrafted nonsense from 2021.

## Man pages

`man runner` and `man run` (plus `man runner-<subcommand>`) ship with every
install channel — AUR (`runner-run` / `runner-run-bin`), npm
(`npm i -g runner-run`), crates.io, and `install.sh`. The pages are rendered
from the CLI definition at release time, not committed.

## Task Resolution

`runner run <target>` first looks for a matching task.

If no task exists, it falls back to executing `<target>` through the detected
toolchain where appropriate, such as:

```text
npm exec / npx, yarn run / yarn exec, pnpm exec, bun x / bunx,
deno x, uvx, go run
```

For package managers without a matching exec primitive, runner falls back to
executing `<target>` directly from `PATH`.

The `run` binary is equivalent to `runner run`, so:

```sh
run clean
run install
```

runs a task or command named `clean` or `install`, even when those names also
exist as built-in `runner` subcommands.

## Supported Ecosystems

runner detects and works with:

```text
npm, yarn, pnpm, bun, cargo, deno, uv, poetry, pipenv, go, bundler, composer
```

It aggregates tasks from these runners:

```text
turbo, nx, make, just, go-task, mise, bacon
```

reading them from:

```text
package.json / package.json5 / package.yaml
turbo.json / turbo.jsonc
deno.json / deno.jsonc
Makefile
justfile
Taskfile
bacon.toml
mise.toml / .mise.toml
Cargo aliases from .cargo/config.toml
pyproject.toml [project.scripts] (run via uv / poetry / pipenv)
```

It also understands monorepo/workspace context from:

```text
turbo, nx, pnpm, npm/yarn workspaces, Cargo workspaces
```

<details>
<summary><i>Support notes</i></summary>

`nx` is currently detection-only. runner uses it for project context, but does
not extract Nx tasks as direct task entries yet.

When multiple sources define the same task, runner chooses deterministically:
turbo tasks first, then package manifest scripts, then other matching sources.

---

</details>

## Features

- `run <TAB>` task completion across projects
- One command shape across many ecosystems
- Simple CI with `runner install --frozen` plus `run <task>` steps
- First-class GitHub Actions install step
- Automatic toolchain detection
- Task aggregation from common config files
- Task-first execution with command fallback
- Monorepo/workspace awareness
- Safe clean defaults
- Node version mismatch warnings

## Links

- Site: [runner.kjanat.dev]
- npm: [`runner-run`][npm]
- crates.io: [`runner-run`][crates]
- aur: [`runner-run`][aur:runner-run], [`runner-run-bin`][aur:runner-run-bin]

## License

[MIT][LICENSE] © 2026 Kaj Kowalski

[LICENSE]: https://github.com/kjanat/runner/blob/master/LICENSE
[aur:runner-run-bin]: https://aur.archlinux.org/packages/runner-run-bin
[aur:runner-run]: https://aur.archlinux.org/packages/runner-run
[crates]: https://crates.io/crates/runner-run
[npm]: https://npm.im/runner-run
[runner.kjanat.dev]: https://runner.kjanat.dev "Site for runner"

<!-- markdownlint-disable-file MD013 MD033 MD041 -->
