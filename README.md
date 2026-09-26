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

Instead of guessing whether this one wants `npm run`, `pnpm exec`, `bun x`,
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
  cargo           i                    install --path crates/cli
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
  cargo           schema               run --quiet -- schema
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
format        -- → run --quiet --bin run -- --pm npm dprint fmt                       schema        -- → run --quiet -- schema
i             -- → install --path crates/cli                                          t             -- → test
l             -- → clippy --all-targets --all-features -- -D warnings -D clippy::all
-- Options --
--dir             -- Project directory, the current one when unset
--pm              -- The package manager to use (npm, yarn, pnpm, bun, deno, cargo, go, uv, poetry, pipenv, bundler, composer)
--runtime         -- The JavaScript runtime to use (bun, deno, node)
--source          -- The task source that must supply the task (turbo, package.json, make, just, task, deno, cargo, go, bacon, mise, pyproject.toml)
--package         -- Run <TASK> as the binary npm package <NAME> declares
--download        -- Download packages a command needs: true, false or ask
--no-download     -- Refuse a command that needs a download
--dry-run         -- Print what would run and why, without running it
--warnings        -- Print warnings
--no-warnings     -- Hide warnings
-q, --quiet       -- Print less, repeatable: -q through -qqqq
--schema-version  -- Pin --json schema (currently always 1)
--sequential      -- Chain tasks in order
--parallel        -- Chain tasks concurrently
--on-fail         -- What a chain does after a task fails
--keep-going      -- Alias for --on-fail continue
--kill-on-fail    -- Alias for --on-fail kill
--help            -- Print help
--version         -- Print detailed build information
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
cargo install --path crates/cli
```

```sh
# AUR source build (compiles via cargo):
yay -S runner-run
```

```dockerfile
# Container image, for build stages that run `run`-form package scripts:
COPY --from=ghcr.io/kjanat/runner:0.26.2 /run /usr/local/bin/run
COPY --from=ghcr.io/kjanat/runner:0.26.2 /runner /usr/local/bin/runner
```

Also on Docker Hub as [`kjanat/runner`][dockerhub]. The image is `scratch` plus
two musl-static binaries, so one tag serves Alpine and Debian stages alike.

It also runs directly, if you'd like:

```sh
docker run --rm kjanat/runner:0.26.2 --version
docker run --rm --entrypoint /run -v "$PWD:/w" kjanat/runner build
```

Without `-v`/`--volume`, the container sees no project.
Only `--version` or `--help` are meaningful without a mount.

```sh
# One-liner (latest):
curl -fsSL https://raw.githubusercontent.com/kjanat/runner/master/install.sh | sh

# Or download then run, optionally pinning a version:
curl -fsSLO https://raw.githubusercontent.com/kjanat/runner/master/install.sh
sh install.sh
sh install.sh 0.12.2
sh install.sh v0.12.2
```

The installer also works on Termux/Android (aarch64, best-effort) from runner
release v0.24.0, where it selects a native `aarch64-linux-android` binary.
Earlier releases lack that asset, so the installer refuses them on Android.

### Verify provenance

Releases after v0.25.1 carry GitHub build-provenance attestations on every
release archive, the man-page tarball, and the container image.
`mise install github:kjanat/runner` checks them on its own; by hand:

```sh
gh attestation verify runner-v<version>-<target>.tar.gz --repo kjanat/runner
gh attestation verify oci://ghcr.io/kjanat/runner:<version> --repo kjanat/runner
```

---

</details>

## GitHub Actions

Use the action to install runner in CI ([view on marketplace](https://github.com/marketplace/actions/setup-runner-cli "I don't know why you would, but ok.")):

```yaml
- uses: kjanat/runner@master
- run: runner install --frozen test build
```

The action downloads the platform package from npm, checks it against the
registry integrity hash, and runs `gh attestation verify` on it when GitHub
holds a build-provenance attestation for that tarball. `verify: require` makes
a missing attestation fatal; `verify: off` skips the check.

```yaml
- uses: kjanat/runner@master
  with: { version: "0.26", verify: require }
```

`runner install` is not a task; it runs the project's toolchain command(s)
(`npm ci`, `cargo fetch`, `uv sync`, …), then chains the listed tasks
(`test`, then `build`) sequentially. When the project has a mise config,
`mise install` runs first so the package managers it declares exist before
they are called, and everything mise manages is on the `PATH` of the
processes runner spawns next; `--no-tools` skips that step.

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
annotations follow `[output] errors` and the roll-up follows `[output]
summary`, so `groups = false` keeps both. `-q` hides the roll-up and `-qqq`
the annotations.

### Quiet, all the way down

`-q` is a preset over the individual `[output]` settings:

| Level   | Runner output                                            | The tool                |
| ------- | -------------------------------------------------------- | ----------------------- |
| `-q`    | Hide progress, groups, timing, the summary, and prefixes | unchanged               |
| `-qq`   | Also hide warnings                                       | its own quiet flag      |
| `-qqq`  | Also hide runner's error messages                        | stronger safe reduction |
| `-qqqq` | Print nothing of runner's own; keep the exit status      | stronger safe reduction |

Larger counts clamp to `-qqqq`. Task stdout and stderr survive every rung;
`[tasks.<name>.output.task] stdout = false` (or `stderr`) discards one.
`RUNNER_QUIET` takes the count, and nested runner processes inherit it.
`--dry-run` stays visible and reports the effective settings, the tool's
arguments, and any fallback.

The preset expands at its own layer and sets only what its row names. `-q`
with `[output] warnings = false` hides both progress and warnings, `-qq
--warnings` shows warnings again, and `-q` leaves a task's `[output.tool] quiet`
alone because `-q` does not touch the tool.

Tool flags are adapter-specific, never inferred from similar names. See the
[host quiet support matrix](docs/host-quiet-support-matrix.md) for exact flags,
stream effects, exclusions, and version caveats.

For example, keep only the final chain summary:

```toml
[output]
progress = false
warnings = false
errors   = false
groups   = false
timing   = false
summary  = true

[output.tool]
quiet = true

[output.task]
stdout = false
stderr = false
```

<details>
<summary><i>Install mechanics and outputs</i></summary>

The action resolves the platform's `@runner-run/*` package, downloads its
tarball from the npm registry, verifies the `sha512` integrity npm publishes for
it, and extracts the `runner` / `run` binaries. Fetching the tarball directly
avoids the node startup and dependency-tree resolution of `npm install`, and the
sub-second download beats any cross-job cache restore, so the action always
fetches fresh. The install dir is added to `PATH` and the binary is smoke-tested
with `runner --version`.

| I/O    | name         | description                                                                    |
| ------ | ------------ | ------------------------------------------------------------------------------ |
| Input  | `version`    | Version to install; defaults to `latest`; accepts exact pins and `v?` prefixes |
| Output | `version`    | Concrete version reported by the installed `runner --version` smoke test       |
| Output | `bin-dir`    | Directory holding the `runner` / `run` binaries; added to `PATH`               |
| Output | `runner-bin` | Full path to the `runner` binary                                               |
| Output | `run-bin`    | Full path to the `run` binary                                                  |

Network fetches retry twice on failure or stall. Exact `X.Y.Z` pins are checked
against the executed CLI version; a mismatch fails the action.

---

</details>

## Usage

```sh
runner                              # show detected project info
runner <task> [-- <args...>]        # run a task
runner run <target> [-- <args...>]  # run a task or command
run <target> [-- <args...>]         # alias for `runner run`

runner install [--frozen] [--no-scripts|--scripts] [--no-tools]  # install dependencies
runner clean [-y] [--include-framework]
runner list [--raw] [--json] [--only <source>]  # list available tasks
runner info [--json]                # show detected project info
runner doctor [--json]              # show every resolver signal
runner why <task> [--json]          # explain how a task would dispatch
runner config <init|show|validate|path>  # manage runner.toml
runner completions [<shell>] [-o <path>]
```

### JSON output

`schema_version` is `1` and bumps when a field is removed, renamed, or
retyped. Ignore keys you do not know. Schemas live in [`schemas/`](schemas/).

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
npm exec / npx, yarn run / yarn exec, pnpm exec, bun x,
deno x, uvx, go run
```

For package managers without a matching exec primitive, runner falls back to
executing `<target>` directly from `PATH`.

A task that resolves back to itself through a nested `runner`/`run` is
refused with the cycle it found (`package.json:tsc -> package.json:tsc`)
instead of spawning copies of itself.

### Runtime

`--pm` says who installs and who invokes a script. `--runtime` says what the
script and the binaries it shells out to actually execute on. Each runtime uses
its own script runner, file runner and package-exec primitive. npm, pnpm and
Yarn run on Node, so `--pm pnpm --runtime node` runs `pnpm run <task>`. Any
other JavaScript package manager chosen beside a runtime is refused with both
names:

| `--runtime` | `package.json` script  | local file        | ad-hoc binary |
| ----------- | ---------------------- | ----------------- | ------------- |
| `node`      | `node --run <task>`    | `node <file>`     | `npx`         |
| `bun`       | `bun --bun run <task>` | `bun <file>`      | `bun x --bun` |
| `deno`      | `deno task <task>`     | `deno run <file>` | `deno x`      |

```sh
run --runtime bun build      # bun --bun run build
run --runtime bun ./cli.js   # bun ./cli.js, even with a #!/usr/bin/env node line
run --runtime bun eslint .   # bun x --bun eslint .
```

`bun run build` starts the script under bun, but a dependency bin carrying a
`#!/usr/bin/env node` shebang still resolves to system Node. `--runtime bun`
adds bun's `--bun`, which puts that bin on bun too. It applies regardless of
which package manager wrote the lockfile, and it outranks a local file's `#!`
line, which is how it reaches `node_modules/.bin` entries.

`node --run` is Node's own script runner (Node 22+). It deliberately skips
`pre<task>` / `post<task>` lifecycle scripts, which `npm run`, `bun run` and
`deno task` all execute; runner warns when the task you dispatch has one.

A local file on `deno` runs with filesystem, network, environment, subprocess
and system-info permissions granted (`--allow-read`, `--allow-write`,
`--allow-net`, `--allow-env`, `--allow-run`, `--allow-sys`), since `deno run
<file>` denies these by default and ignores the file's shebang. Deno's default
remote-import allowlist is left in place, so a file cannot import and execute
code from an arbitrary host, matching how node and bun already refuse remote
imports. This applies to any detected Deno project, not only `--runtime deno`.

A runtime you set never applies silently to nothing. When the task that wins
selection comes from a source with no runtime to choose (`make`, `just`,
`Taskfile`, `turbo`, cargo, …), runner says so. `package.json` (and `deno.json`
under `--runtime deno`) outranks the other sources while a runtime is forced,
so `--runtime bun build` in a turborepo runs the script rather than the turbo
task.

Set it per project with `[runtime] javascript`, per task with
`[tasks.<name>.runtime] javascript`, or per invocation with `RUNNER_RUNTIME`.
Nested `runner`/`run` calls inherit the flag and the variable.

### Package selection

A bare binary name resolves through whichever package won the shared
`node_modules/.bin` link. `--package` names the package instead, and the task
token names one of the binaries its manifest declares:

```sh
run --package typescript tsc -v     # typescript's own bin/tsc, never another package's tsc
run --package typescript tsserver   # any binary the manifest declares
```

An installed package resolves from its own `package.json`, or from `yarn bin`
under Yarn Plug'n'Play. One that is not installed goes to the package manager's
package-selecting form: `npx --package`, `bun x --package`,
`pnpm --package=<name> dlx`, `yarn dlx --package` (Yarn 2+),
`deno x npm:<name>/<bin>`, `uvx --from`; `--runtime` and a non-Node `--pm`
pick the form the same way they do for a bare binary. A binary the package does
not declare is an error listing the ones it has, and so is a fetch for a binary
another installed package already provides, since the fetched package could not
be told apart from it. An `npm:` prefix on a task token is refused with the
equivalent `--package` form.

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
`runner.toml` at the repo root. Scaffold one from the schema:

```sh
runner config init          # write the schema pragma and every default (--force to overwrite)
runner config show          # print the effective config (--json for machine output)
runner config validate      # parse + check it; exit 2 on error
runner config path          # print the resolved runner.toml path
```

Settings layer, highest priority first: **CLI flags, `RUNNER_*` variables,
`[tasks.<name>]`, the rest of `runner.toml`, project evidence** (lockfiles,
`packageManager`, `devEngines`), **defaults**. An explicit `false` is a value
and overrides the layers below it. `env` maps merge by variable name.

`runner config init` writes a `#:schema` directive on line 1, so editors with a
TOML language server (tombi, taplo) get autocompletion and validation with no
extra setup.

```toml
#:schema https://kjanat.github.io/runner/schemas/runner.toml.schema.json

# Downloads a command needs: true, false or "ask". Unset, runner asks on an
# interactive terminal outside CI and allows elsewhere. "ask" without a
# terminal refuses.
download = "ask"

[runtime]
javascript = "bun"  # node | bun | deno

[chain]
on_fail = "wait"  # continue | wait | kill

[install]
frozen  = false  # install exactly what the lockfile pins
scripts = false  # dependencies' lifecycle scripts; unset keeps each manager's default
tools   = true   # `mise install` first when a mise config is detected

[output]
warnings = true
errors   = true  # runner's error text; a failed command still fails
summary  = true  # the roll-up after a chain
progress = true  # the `→ source task` line
groups   = true  # GitHub Actions groups
timing   = true  # each chain task's timing line

[output.tool]
quiet = false  # pass the tool its own quiet flag (npm --silent, make -s)

[output.task]
stdout = true  # false discards the task's stdout
stderr = true

[output.parallel]
buffer = false  # print each parallel task as one block; on by default under Actions

[env]
FORCE_COLOR = "1"

[tools.npm.env]
npm_config_fund = "false"

# `source` is a hard selection: the task must come from that source.
[tasks.build]
source  = "turbo"
pm      = "pnpm"
runtime = { javascript = "node" }
env     = { NODE_ENV = "production" }

[tasks.build.output]
timing = false

# Keys layer from least to most specific: `site` (every task by that name),
# `package.json:site` (that source), `rfc:site` (workspace member `rfc`),
# `rfc:package.json#site` (the name `doctor --json` prints).
[tasks."rfc:site".output.task]
stdout = false
```

`[tasks.<name>.output]` takes the same `progress`, `groups`, `timing`, `tool`
and `task` settings as `[output]`.

`scripts = false` skips install-time lifecycle scripts where the package
manager allows it (npm, yarn, pnpm, bun, composer; deno denies them already).
`scripts = true` forces them on where the manager can express it. bun and pnpm
10+ need a `trustedDependencies` / `onlyBuiltDependencies` allowlist in the
manifest for that and warn instead.

### Environment variables

Every flag reads a variable named after it: `RUNNER_<FLAG>` for a global flag
and `RUNNER_<COMMAND>_<FLAG>` for a command's own. A `--no-` form and an alias
set the same variable as their flag. The `run` binary's own flags read
`RUNNER_RUN_*`.

| Variable                 | Flag                                       |
| ------------------------ | ------------------------------------------ |
| `RUNNER_DIR`             | `--dir`                                    |
| `RUNNER_PM`              | `--pm`                                     |
| `RUNNER_RUNTIME`         | `--runtime`                                |
| `RUNNER_SOURCE`          | `--source`                                 |
| `RUNNER_PACKAGE`         | `--package`                                |
| `RUNNER_DOWNLOAD`        | `--download`, `--no-download`              |
| `RUNNER_ON_FAIL`         | `--on-fail`, `-k`, `-K`                    |
| `RUNNER_DRY_RUN`         | `--dry-run`                                |
| `RUNNER_WARNINGS`        | `--warnings`, `--no-warnings`              |
| `RUNNER_QUIET`           | `-q` (takes the count)                     |
| `RUNNER_INSTALL_FROZEN`  | `runner install --frozen`, `--no-frozen`   |
| `RUNNER_INSTALL_SCRIPTS` | `runner install --scripts`, `--no-scripts` |
| `RUNNER_INSTALL_TOOLS`   | `runner install --tools`, `--no-tools`     |
| `RUNNER_LIST_ONLY`       | `runner list --only` (comma-separated)     |

Boolean variables take `1`/`0`, `true`/`false`, `yes`/`no` or `on`/`off`.
`runner doctor` reports a variable with an invalid value and keeps going;
other commands refuse to run.

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
  keys, bad package-manager / runtime / source labels), live as you type;
- **hover**, section and field documentation, sourced from the JSON Schema;
- **completion**, section names, field names, task names, and value sets
  (package managers, runtimes, sources, policy enums, booleans).

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
turbo, nx, pnpm-workspace.yaml, npm/yarn/bun workspaces, lerna.json,
deno.json workspace, Cargo workspaces
```

From the workspace root, every member's `package.json` scripts and `deno.json`
tasks are listed and runnable:

```console
$ runner list
  package.json    rfc:site
  package.json    rfc:typecheck
  package.json    @acme/web:site
  cargo           test

$ run rfc:site            # member by manifest name, path, or directory name
$ run apps/web:site
$ run typecheck           # bare name: only `rfc` defines it, so it runs there
$ run site                # error: `rfc` and `@acme/web` both define it
```

Every directory beneath the workspace root sees the whole workspace. Inside a
member, that member's tasks come first and complete bare; the root's tasks are
next; other members stay `member:task`. A root task a member shadows is
reachable as `root:task`. Local-file tokens (`./gen.sh`) and package-manager
exec fallbacks still resolve against the directory you are in.

```console
$ cd rfc
$ runner list
  package.json    site                # rfc's
  package.json    typecheck
  package.json    @acme/web:site
  cargo           test                # the root's
```

A root task always wins a bare name over another member's same-named task. A
bare name that several members define is refused with the qualified spellings.
Member tasks run in the member's directory through the package manager the
root resolves to. `doctor --json` and `why --json` address every task as
`<scope>:<source>#<task>` with `scope` being `root` or the member name, and
that form works from any directory in the workspace.

<details>
<summary><i>Support notes</i></summary>

`nx` is currently detection-only. runner uses it for project context, but does
not extract Nx tasks as direct task entries yet.

When multiple sources define the same task, runner chooses deterministically:
turbo tasks first, then package manifest scripts, then other matching sources.

Workspace members contribute their own manifest scripts (`package.json`,
`package.json5`, `package.yaml`) and `deno.json` tasks. Other sources
(`Makefile`, `justfile`, …) are read from the root only. A member whose name
collides with a source label (`just`, `make`, `deno`, …) must be addressed by
path.

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
- docker: [`ghcr.io/kjanat/runner`][ghcr], [`kjanat/runner`][dockerhub]

## License

[MIT][LICENSE] © 2026 Kaj Kowalski

[LICENSE]: https://github.com/kjanat/runner/blob/master/LICENSE
[aur:runner-run-bin]: https://aur.archlinux.org/packages/runner-run-bin
[aur:runner-run]: https://aur.archlinux.org/packages/runner-run
[crates]: https://crates.io/crates/runner-run
[dockerhub]: https://hub.docker.com/r/kjanat/runner
[ghcr]: https://github.com/kjanat/runner/pkgs/container/runner
[npm]: https://npm.im/runner-run
[runner.kjanat.dev]: https://runner.kjanat.dev "Site for runner"
[socket]: https://socket.dev/npm/package/runner-run

<!-- markdownlint-disable-file MD013 MD033 MD041 -->
