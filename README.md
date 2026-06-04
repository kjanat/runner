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
run 0.10.0

  Package Managers    cargo
  Task Runners        just, bacon

  justfile        build-packages
  justfile        default
  justfile        gen-schema           just gen-schema && git diff --exit-code schemas/
  justfile        install
  justfile        ls
  justfile        run
  justfile        runner
  justfile        test-release         Build release bin and verify the facade shims spawn the native binary.
  config.toml     b                    build
  config.toml     bb                   build --bin run --bin runner
  config.toml     bbr                  build --bin run --bin runner --release
  config.toml     bin-run              run --bin run --quiet
  config.toml     bin-runner           run --bin runner --quiet
  config.toml     c                    check
  config.toml     cl                   clippy --all-targets --all-features
  config.toml     comp                 run --bin runner --quiet -- completions
  config.toml     d                    doc
  config.toml     i                    install --path .
  config.toml     l                    clippy --all-targets --all-features -- -D warnings -D clippy::all
  config.toml     lint                 clippy --all-targets --all-features -- -D warnings -D clippy::all
  config.toml     meta                 metadata --format-version 1
  config.toml     r                    run
  config.toml     rbin-run             run --bin run --quiet --release
  config.toml     rbin-runner          run --bin runner --quiet --release
  config.toml     rm                   remove
  config.toml     rr                   run --release
  config.toml     runner               run --bin runner
  config.toml     schema               run --quiet --example gen-schema --features schema-gen
  config.toml     t                    test
  bacon.toml      bins                 cargo build --bin runner --bin run --color=always
  bacon.toml      check                cargo check
  bacon.toml      check-all            cargo check --all-targets
  bacon.toml      clippy               cargo clippy
  bacon.toml      clippy-all           cargo clippy --all-targets
  bacon.toml      doc                  cargo doc --no-deps
  bacon.toml      doc-open             cargo doc --no-deps --open
  bacon.toml      ex                   cargo run --example
  bacon.toml      lint                 cargo clippy --all-targets --all-features --color=always -- -D warnings -D clippy::all
  bacon.toml      nextest              cargo nextest run --hide-progress-bar --failure-output final
  bacon.toml      pedantic             cargo clippy -- -W clippy::pedantic
  bacon.toml      run                  cargo run
  bacon.toml      run-long             cargo run
  bacon.toml      test                 cargo test
  bacon.toml      test-all             cargo test --all-features --all-targets --color=always
```

and `run <TAB>` (zsh):

```shell
❯ run <TAB>waiting...
-- justfile --
build-packages
default
gen-schema                     -- just gen-schema && git diff --exit-code schemas/
install
ls
run
justfile:run
runner
justfile:runner
test-release                   -- Build release bin and verify the facade shims spawn the native binary.
-- cargo --
b                              -- build
bb                             -- build --bin run --bin runner
bbr                            -- build --bin run --bin runner --release
bin-run                        -- run --bin run --quiet
bin-runner                     -- run --bin runner --quiet
c                              -- check
cl                             -- clippy --all-targets --all-features
comp                           -- run --bin runner --quiet -- completions
d                              -- doc
i                              -- install --path .
l                              -- clippy --all-targets --all-features -- -D warnings -D clippy::all
lint                           -- clippy --all-targets --all-features -- -D warnings -D clippy::all
cargo:lint                     -- clippy --all-targets --all-features -- -D warnings -D clippy::all
meta                           -- metadata --format-version 1
r                              -- run
rbin-run                       -- run --bin run --quiet --release
rbin-runner                    -- run --bin runner --quiet --release
rm                             -- remove
rr                             -- run --release
cargo:runner                   -- run --bin runner
schema                         -- run --quiet --example gen-schema --features schema-gen
t                              -- test
-- bacon.toml --
bins                           -- cargo build --bin runner --bin run --color=always
check                          -- cargo check
check-all                      -- cargo check --all-targets
clippy                         -- cargo clippy
clippy-all                     -- cargo clippy --all-targets
doc                            -- cargo doc --no-deps
doc-open                       -- cargo doc --no-deps --open
ex                             -- cargo run --example
bacon.toml:lint                -- cargo clippy --all-targets --all-features --color=always -- -D warnings -D clippy::all
nextest                        -- cargo nextest run --hide-progress-bar --failure-output final
pedantic                       -- cargo clippy -- -W clippy::pedantic
bacon.toml:run                 -- cargo run
run-long                       -- cargo run
test                           -- cargo test
test-all                       -- cargo test --all-features --all-targets --color=always
-- Options --
--dir                          -- Use this directory instead of the current one
--pm                           -- Override the detected package manager (e.g. pnpm, bun, yarn). Also reads RUNNER_PM when omitted.
--runner                       -- Override the detected task runner (e.g. just, turbo, make). Also reads RUNNER_RUNNER when omitted.
--fallback                     -- What to do when no detection signal matches: probe (default, PATH probe), npm (legacy silent fallback), error (refuse). Also reads RUNNER_FALLBACK when omitted.
--on-mismatch                  -- What to do when the manifest declaration disagrees with the lockfile: warn (default), error (exit 2), ignore (silent). Also reads RUNNER_ON_MISMATCH when omitted.
--explain                      -- Print a one-line trace describing how the package manager was resolved. Also enabled when RUNNER_EXPLAIN is set to a truthy value.
--no-warnings                  -- Suppress all non-fatal warnings on stderr. Also enabled when RUNNER_NO_WARNINGS is set to a truthy value.
--help                         -- Print help
--version                      -- Print version
```

---

</details>

runner detects the project, finds its tasks, and completes them through one
command.

Use the same shape everywhere:

```sh
run <TAB>
runner install --frozen
run test
run build
run deploy
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

Or on Debian / Ubuntu:

```sh
sudo install -d -m 0755 /etc/apt/keyrings
curl -fsSL https://apt.runner.kjanat.dev/runner-run.gpg | sudo tee /etc/apt/keyrings/runner-run.gpg >/dev/null
echo "deb [signed-by=/etc/apt/keyrings/runner-run.gpg] https://apt.runner.kjanat.dev stable main" | sudo tee /etc/apt/sources.list.d/runner-run.list >/dev/null
sudo apt update && sudo apt install runner-run
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
# Debian/Ubuntu — direct .deb without the apt repo (arch ∈ amd64 arm64 armhf):
curl -fsSLO https://github.com/kjanat/runner/releases/download/v0.12.0/runner-run_0.12.0_amd64.deb
sudo apt install ./runner-run_0.12.0_amd64.deb
```

```sh
curl -fsSLO https://raw.githubusercontent.com/kjanat/runner/master/install.sh
bash install.sh
bash install.sh 0.10.0
bash install.sh v0.10.0
```

---

</details>

## GitHub Actions

Use the action to install runner in CI:

```yaml
- uses: kjanat/runner@master
- run: runner install --frozen
- run: run test
- run: run build
```

<!--
Future shorthand once install/task chaining is supported:

```yaml
- uses: kjanat/runner@master
- run: runner install test build deploy
#             ^^^^^^^
# `runner install` here is not a task, but runs the needed toolchain command(s)
# for the project, such as `npm ci`, `cargo fetch`, `uv sync`, etc.
```
-->

`runner install` here is not a task, but runs the needed toolchain command(s)
for the project, such as `npm ci`, `cargo fetch`, `uv sync`, etc.

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

It can list and run tasks from:

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

## Development

```sh
./bin/runner <args>
./bin/run <args>
```

With `direnv`:

```sh
runner <args>
```

## Links

- Site: [runner.kjanat.dev]
- npm: [`runner-run`][npm]
- crates.io: [`runner-run`][crates]

## License

[MIT][LICENSE] © 2026 Kaj Kowalski

[npm]: https://npm.im/runner-run
[crates]: https://crates.io/crates/runner-run
[runner.kjanat.dev]: https://runner.kjanat.dev "Site for runner"
[LICENSE]: https://github.com/kjanat/runner/blob/master/LICENSE

<!-- markdownlint-disable-file MD013 MD033 MD041 -->
