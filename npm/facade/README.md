# runner-run

**runner** is for people who bounce between codebases and refuse to memorize
each repo's private little task-running religion.

Install it from npm, then stop guessing:

```sh
npm install -g runner-run
run <TAB>
runner install --frozen test build
```

Instead of remembering whether this project wants `npm run`, `pnpm exec`,
`bunx`, `cargo`, `uv run`, `deno task`, `make`, `just`, `go-task`, or a monorepo
wrapper, use the same shape everywhere.

## What You Get

`runner-run` puts two commands on your `PATH`:

| command  | use it for                                            |
| -------- | ----------------------------------------------------- |
| `runner` | Full CLI: detect, install, clean, list, completions.  |
| `run`    | Short task-first alias: `run test`, `run build`, etc. |

`runner` is the grown-up command. `run` is the muscle memory command.

## Install

```sh
npm install -g runner-run
```

Other package managers work too:

```sh
pnpm add -g runner-run
yarn global add runner-run
bun add -g runner-run
```

Or pin it per project for CI/dev shells:

```sh
npm install --save-dev runner-run
```

## Use It

```sh
runner                              # show detected project info
runner <task> [-- <args...>]        # run a task
runner run <target> [-- <args...>]  # run a task or command
run <target> [-- <args...>]         # alias for `runner run`

runner install [--frozen]           # install dependencies
runner clean [-y] [--include-framework]
runner list [--raw] [--json]        # list available tasks
runner doctor [--json]              # show resolver signals
runner why <task> [--json]          # explain how dispatch works
runner completions [<shell>] [-o <path>]
```

`runner run <target>` looks for a project task first. If no task exists, it
falls back to an exec-style command through the detected toolchain when that
ecosystem supports one.

The `run` binary always means "task or command". So:

```sh
run clean
run install
```

runs a task or command named `clean` or `install`, even though those are also
`runner` built-ins.

## Completions

Completions are the main trick. Let the shell ask runner what exists *right now*
in the current project:

```sh
eval "$(runner completions)"
run <TAB>
```

Explicit shell forms:

```sh
eval "$(runner completions bash)"
eval "$(runner completions zsh)"
eval "$(runner completions fish)"
```

PowerShell:

```powershell
runner completions powershell | Out-String | Invoke-Expression
```

No per-project command archaeology. No guessing which wrapper this repo invented
in 2021.

## Man Pages

Unix-like npm installs include man pages:

```sh
man runner
man run
man runner-list
```

They are generated from the CLI definition at release time and shipped in the
npm package. Nothing gets generated during install.

## What Runner Understands

Package managers and ecosystems:

```text
npm, yarn, pnpm, bun, cargo, deno, uv, poetry, pipenv, go, bundler, composer
```

Task sources:

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

Workspace context:

```text
turbo, nx, pnpm, npm/yarn workspaces, Cargo workspaces
```

## The npm Package Bit

This package is a tiny shim plus platform packages. It does not build Rust on
your machine and it does not fetch binaries in a `postinstall` script.

At publish time, `runner-run` declares packages like `@runner-run/linux-x64-gnu`
as `optionalDependencies`. Your package manager picks the one matching your
OS/CPU/libc, then the `runner` and `run` shims exec the local binary.

Useful consequences:

- no `postinstall` script
- no install-time network fetch
- no local Rust toolchain needed
- lockfiles can pin the main package and platform package together

## Supported npm Targets

| OS      | Architectures                                |
| ------- | -------------------------------------------- |
| Linux   | x64/arm64 glibc, x64/arm64 musl, armv7 glibc |
| macOS   | x64, arm64                                   |
| Windows | x64, arm64, ia32                             |
| FreeBSD | x64, arm64 experimental                      |
| NetBSD  | x64 experimental                             |

If your platform is not listed, use Cargo:

```sh
cargo install runner-run
```

Or poke the issue cave: <https://github.com/kjanat/runner/issues>

## Troubleshooting

### `no prebuilt binary found`

Your package manager probably skipped `optionalDependencies`, or your lockfile
came from a different platform.

Common causes:

- `npm install --omit=optional` or `npm install --no-optional`
- `yarn install --ignore-optional`
- pnpm configured with `optional=false`
- a lockfile committed from another OS/CPU/libc target

Reinstall with optional dependencies enabled. If the lockfile is stale for your
target, regenerate it there or force a reinstall:

```sh
npm install --force
```

If optional dependencies are disabled on purpose, use Cargo:

```sh
cargo install runner-run
```

### `runner` works but `run` does not complete

Regenerate shell completions after installing or upgrading:

```sh
eval "$(runner completions)"
```

Both commands must live in the same bin directory for one generated completion
registration to cover both.

### `Cannot find module '…\node_modules\runner-run\bin\runner.cjs'`

Node is reporting that the launcher file itself is **missing on disk** — not a
bug in the package (the published tarball always ships `bin/runner.cjs`). The
same symptom appears for any thin-launcher package (`dprint`, `tombi`, …) in the
same tree, often all at once.

This is a **corrupt or half-finished install**, not a runner problem. Tell-tale
signs on Windows: package directories with `1601` (zero-epoch) timestamps,
"permission denied" when reading a file that exists, or a `node_modules/<pkg>`
folder present but empty. Common causes:

- an interrupted `bun install` / `npm install` (network blip, `Ctrl-C`, crash)
- a pruned, moved, or corrupted package-manager content store the install
  hard-links from (breaks every project pointing at it at once)
- OneDrive "Files On-Demand" dehydrating files under your projects folder
- antivirus quarantining `.cjs` / `.node` files mid-extraction

Because the directory exists, a plain reinstall may skip it. Remove the tree and
reinstall from scratch:

```sh
rm -rf node_modules            # Windows: cmd /c "rmdir /s /q node_modules"
<pm> install                   # bun install / npm ci / pnpm install …
```

If files keep vanishing after a clean reinstall, exclude your projects directory
(and `~/.bun` / the npm cache) from cloud-sync and antivirus.

## More Docs

- Full README: <https://github.com/kjanat/runner#readme>
- Site: <https://runner.kjanat.dev>
- Issues: <https://github.com/kjanat/runner/issues>

## License

[MIT](https://github.com/kjanat/runner/blob/master/LICENSE) (c) 2026 Kaj Kowalski
