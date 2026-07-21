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
[![Socket](https://badge.socket.dev/npm/package/runner-run)][socket]
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
run 0.12.2

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
-q, --quiet       -- Quiet level, repeatable (-q/-qq/-qqq). Silences runner's own output AND passes the spawned tool's silence flag (npm --silent, cargo -q, make -s, ...). -qq also mutes warnings. Also reads RUNNER_QUIET (a number 0-3 or a truthy word); inherited by a nested runner.
--host-stream     -- Keep the host tool's stdout clean by diverting its diagnostics to stderr: inherit (default) | stderr. Only pnpm can (via --use-stderr); other hosts no-op. Also reads RUNNER_HOST_STREAM.
--schema-version  -- Pin JSON output schema version (currently always 1). Affects --json output of doctor/list/why only.
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

<a href="https://repology.org/project/runner-run/versions">
    <img src="https://repology.org/badge/vertical-allrepos/runner-run.svg" alt="Packaging status" align="right">
</a>

```sh
paru -S runner-run-bin # or `paru -S runner-run` (builds from source)
yay  -S runner-run-bin # .. `yay  -S runner-run`
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
# One-liner (latest):
curl -fsSL https://raw.githubusercontent.com/kjanat/runner/master/install.sh | sh

# Or download then run, optionally pinning a version:
curl -fsSLO https://raw.githubusercontent.com/kjanat/runner/master/install.sh
sh install.sh
sh install.sh 0.12.2
sh install.sh v0.12.2
```

---

</details>

## GitHub Actions

Use the action to install runner in CI ([view on marketplace](https://github.com/marketplace/actions/setup-runner-cli "I don't know why you would, but ok.")):

```yaml
- uses: kjanat/runner@master
- run: runner install --frozen test build
```

`runner install` is not a task; it runs the project's toolchain command(s)
(`npm ci`, `cargo fetch`, `uv sync`, …), then chains the listed tasks
(`test`, then `build`) sequentially.

That is the point: the workflow stays boring even when the project underneath is
npm, pnpm, bun, Cargo, Deno, uv, Make, just, or whatever automation that repo
uses.

A chain of more than one task closes with a roll-up on stderr, so a failure in
a long `--keep-going` run does not have to be found by scrolling:

```text
· summary: 7 tasks, 5 ok, 1 failed, 1 skipped (exit 1, first failure)
·   ✓ typecheck       0.9s
·   ✗ test:bun        2.1s (exit 1)
·   – test:regex      skipped
```

Under Actions, each failed task also lands in the Annotations panel. The
annotations follow `[github].group_output`; the roll-up itself does not, so
opting out of Actions decoration keeps the summary. `--quiet` silences both,
along with everything else runner prints.

### Quiet, all the way down

`-q` no longer stops at runner's own output — it **crosses into the spawned
tool**. `run -q build` both hides runner's arrow/trace/summary *and* passes the
host its own silence flag (`npm --silent`, `pnpm --silent`, `yarn --silent`,
`bun run --silent`, `cargo -q`, `deno task -q`, `make -s`, `task -s`, `mise
--quiet`, and the `uv`/`poetry` `--quiet`). That matters because some hosts
(npm on an npm project) write their lifecycle banner to **stdout**, so before
this a `run -q <task>` piped into a parser was still corrupted one layer down.
Hosts with no such flag — or whose only "quiet" is a full mute that would eat
the task's own output (`just`, `turbo`, `go run`, `bacon`, `pipenv`) — are left
untouched, never an error. The level escalates pytest-style: `-qq` also mutes
runner's warnings, `-qqq` is the floor. `RUNNER_QUIET` takes a number (`0`–`3`)
or a truthy word; a falsy word (`0`, `false`, `off`) or a passed `-q` on the
command line both override it — the former to explicitly turn inherited quieting
off, the latter because the CLI flag wins over the env. The resolved level is
inherited by a nested `runner` a task shells out to.

Orthogonally, `--host-stream stderr` (`RUNNER_HOST_STREAM`) asks the host to
keep **stdout** clean by routing its diagnostics to stderr. Only pnpm has the
primitive (`--use-stderr`); elsewhere it no-ops. It composes with any quiet
level.

Both knobs are configurable **per task** in `[tasks]` (see below), so a single
noisy task can be pinned quiet without a global flag.

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

runner install [--frozen] [--no-scripts|--scripts]  # install dependencies
runner clean [-y] [--include-framework]
runner list [--raw] [--json]        # list available tasks
runner info [--json]                # show detected project info
runner doctor [--json]              # show every resolver signal
runner why <task> [--json]          # explain how a task would dispatch
runner config <init|show|validate|path>  # manage runner.toml
runner completions [<shell>] [-o <path>]
```

### Forwarding arguments

Runner's own flags go **before** the task; everything after it belongs to the
task, including flags that spell the same as runner's:

```sh
run -q tsc -p tsconfig.json --noEmit   # -q is runner's, -p is tsc's
```

The rule holds through nested dispatch, so a package script may delegate to
another task without counting `--` delimiters:

```json
{ "scripts": { "typecheck": "run -q tsc -p tsconfig.json --noEmit" } }
```

`--` is still accepted for a task whose *name* starts with a hyphen, and for
readability.

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
install channel, AUR (`runner-run` / `runner-run-bin`), npm
(`npm i -g runner-run`), crates.io, and `install.sh`. The pages are rendered
from the CLI definition at release time, not committed.

## Task Resolution

`runner run <target>` first looks for a matching task.

If no task exists, runner tries `<target>` as a local file, then as an
installed dependency: `run @typescript/native` reads
`node_modules/@typescript/native/package.json` and runs the binary it
declares, without touching the network. A package that declares several
binaries none of which is named after it, or none at all, is reported rather
than guessed at.

Failing that, it executes `<target>` through the detected toolchain where
appropriate, such as:

```text
npm exec / npx, yarn run / yarn exec, pnpm exec, bun x / bunx,
deno x, uvx, go run
```

For package managers without a matching exec primitive, runner falls back to
executing `<target>` directly from `PATH`.

A task that resolves back to itself through a nested `runner`/`run` is
refused with the cycle it found (`package.json:tsc -> package.json:tsc`)
instead of spawning copies of itself.

The `run` binary is equivalent to `runner run`, so:

```sh
run clean
run install
```

runs a project task named `clean` or `install` when one exists, even though
those names are also built-in `runner` subcommands. When no such task exists, a
bare built-in verb (`install`, `clean`, `list`, `info`, `completions`) falls
back to that built-in's default form (so `run install` installs dependencies)
rather than the package-manager exec path.

The explicit subcommand is the inverse: `runner install` (and `runner clean`,
`runner list`, …) is **always** the built-in and never runs a same-named task;
use `run install` / `runner run install` to reach a task called `install`.

## Configuration

Auto-detection needs no config. To override it per project, drop a
`runner.toml` at the repo root. Scaffold one with every knob documented:

```sh
runner config init          # write a commented runner.toml (--force to overwrite)
runner config show          # print the effective config (--json for machine output)
runner config validate      # parse + check it; exit 2 on error
runner config path          # print the resolved runner.toml path
```

Settings layer, highest priority first: **CLI flags → `RUNNER_*` env vars →
`runner.toml` → manifest declarations** (`packageManager`, `devEngines`).

`runner config init` writes a `#:schema` directive on line 1, so editors with a
TOML language server (tombi, taplo) get autocompletion and validation with no
extra setup.

```toml
#:schema https://kjanat.github.io/runner/schemas/runner.toml.schema.json

# Force the package manager per ecosystem, overriding lockfile detection.
[pm]
node   = "pnpm"  # npm | pnpm | yarn | bun | deno
python = "uv"    # uv | poetry | pipenv

# Per-task configuration, keyed by task name the way Cargo's [dependencies] is
# keyed by crate name. `prefer` (global rank) and `overrides` (legacy per-task
# pin map) are reserved keys; every other key is a task entry. A task entry is
# either a string (shorthand source/runner pin, like serde = "1.0") or a table
# of settings (runner, verbosity, ... like serde = { version, features }).
# Labels are task runners, package managers (bun, npm, ... map to package.json),
# or source names (package.json). Rank-only: unlisted sources still run. An
# explicit qualifier (package.json:test), --runner, or --pm still outranks these.
[tasks]
prefer    = ["turbo", "bun"]                  # global order: turbo, then package.json
overrides = { dev = "bun", build = "turbo" }  # legacy per-task pins beat the order

# Task entries (Cargo-[dependencies] style):
# build = "turbo"                                  # string → source/runner pin
# test  = { runner = "bun", verbosity = "quiet" }  # table of per-task settings
# [tasks.lint]                                      # sub-table form
# verbosity = { level = "quiet", stream = "stderr" }  # off|quiet|very-quiet|silent

# `verbosity` is the per-task form of the -q / --host-stream flags: a string
# (off|quiet|very-quiet|silent) or a { level, stream } table, deep-merged under
# any global flag/env. So a single noisy task can run quiet without -q.

# Deprecated, superseded by [tasks] above. Legacy ranked allow-list of task
# runners that also *restricts* candidates (a same-named task under an unlisted
# runner is rejected). Still honored for existing configs, with a warning.
# [task_runner]
# prefer = ["just", "turbo"]  # turbo, nx, make, just, task, mise, bacon

# Restrict which detected package managers `runner install` runs. Empty/absent
# installs every detected PM. Overridden by RUNNER_INSTALL_PMS
# (comma-separated). `[pm]` above only scopes script dispatch, not the install
# fan-out.
# `on_collision` decides what happens when two of them write the same directory
# (bun and a nodeModulesDir-enabled deno both writing node_modules). "resolve"
# (the default) installs with the PM the resolver already picked for the
# ecosystem and skips the other, saying so; naming both in `pms` runs both, one
# after another over the shared tree. "error" refuses to pick and exits 2.
# Overridden by RUNNER_INSTALL_ON_COLLISION.
# `scripts` controls install-time lifecycle scripts (the main supply-chain
# attack surface): "deny" skips them where the PM allows it
# (npm/yarn/pnpm/bun/composer; deno already denies); "allow" forces them on
# where the PM can express it (npm --no-ignore-scripts, yarn-berry
# YARN_ENABLE_SCRIPTS=true, deno --allow-scripts), useful now that npm/pnpm are
# moving to scripts-off-by-default. bun and pnpm (>=10) can't be forced on by a
# flag (their dependency build scripts need a trustedDependencies /
# onlyBuiltDependencies manifest allowlist runner won't write), so they warn.
# Precedence: CLI --no-scripts/--scripts > RUNNER_INSTALL_SCRIPTS > [install].scripts.
[install]
pms          = ["bun"]    # only install with these; each must be detected
scripts      = "deny"     # deny | allow  (absent = each PM's own default)
on_collision = "resolve"  # resolve (one writer per install dir) | error

# Resolver policy knobs.
[resolution]
fallback    = "probe"  # probe (PATH probe) | npm (legacy) | error
on_mismatch = "warn"   # warn | error (exit 2) | ignore  (manifest vs lockfile)

# Failure policy for `-s`/`-p` chains and `install <tasks>`.
# keep_going and kill_on_fail are mutually exclusive; setting both is an error.
[chain]
keep_going   = false  # run every task despite failures (same as -k)
kill_on_fail = false  # parallel: kill siblings on first failure (same as -K)

# GitHub Actions output grouping (active only under Actions).
[github]
group_output   = true  # ::group:: each task; annotate failed chain tasks
group_parallel = true  # buffer parallel tasks, print each as one block

# Parallel (`-p`) output presentation outside GitHub Actions.
[parallel]
grouped = false  # buffer + print each task as one block on completion
```

Unknown keys are rejected at parse time. Every field is optional; omit a
section to keep its defaults. A committed JSON Schema lives at
[`schemas/runner.toml.schema.json`](schemas/runner.toml.schema.json) for
editor autocompletion.

### Editor support (language server)

`runner` ships a language server for `runner.toml`:

```sh
cargo install runner-run   # or build locally: cargo build
runner lsp                  # speaks LSP over stdio
```

It provides, reusing the same logic the CLI uses:

- **diagnostics**, the exact `runner config validate` checks (syntax, unknown
  keys, bad package-manager / runner / source labels, conflicting policies) plus
  deprecation hints, live as you type;
- **hover**, section and field documentation, sourced from the JSON Schema;
- **completion**, section names, field names, and value sets (package managers,
  the `[tasks]` runner/PM/source labels, policy enums, booleans).

Point your editor's generic LSP client at `runner lsp` for files named
`runner.toml`. Example (Neovim):

```lua
vim.lsp.start({
  name = "runner",
  cmd = { "runner", "lsp" },
  root_dir = vim.fs.dirname(vim.fs.find({ "runner.toml" }, { upward = true })[1]),
})
```

For schema-only autocompletion without the server, the `#:schema` directive that
`runner config init` writes is enough for editors with a TOML language server.

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
[socket]: https://socket.dev/npm/package/runner-run

<!-- markdownlint-disable-file MD013 MD033 MD041 -->
