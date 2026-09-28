# runner-run

[![NPM](https://img.shields.io/npm/v/runner-run?logo=npm&labelColor=CB3837&color=black)][npm]
[![Socket](https://badge.socket.dev/npm/package/runner-run)][socket]

**runner** is for people who bounce between codebases and refuse to memorize
each repo's private little task-running religion.

Install it from npm, then stop guessing:

```sh
npm i -g runner-run
run <TAB>
runner install --frozen test build
```

Instead of remembering whether this project wants `npm run`, `pnpm exec`,
`bun x`, `cargo`, `uv run`, `deno task`, `make`, `just`, `go-task`, or a monorepo
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
npm i -g runner-run
```

Other package managers work too:

```sh
pnpm add -g runner-run
yarn global add runner-run
bun i -g runner-run
```

Or pin it per project for CI/dev shells:

```sh
npm install --save-dev runner-run
```

For `cargo-binstall`, source builds, Arch packages, container images, and the
shell installer, see [all installation methods][install].

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

Unix-like installs include man pages:

```sh
man runner
man run
man runner-list
```

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

## Supported Platforms

| OS      | Architectures                                |
| ------- | -------------------------------------------- |
| Linux   | x64/arm64 glibc, x64/arm64 musl, armv7 glibc |
| macOS   | x64, arm64                                   |
| Windows | x64, arm64, ia32                             |
| Android | arm64 best-effort (Termux)                   |
| FreeBSD | x64, arm64 experimental                      |
| NetBSD  | x64 experimental                             |

If your platform is not listed, a source build may still work. See the
[installation methods][install] or poke the issue cave:
<https://github.com/kjanat/runner/issues>.

## Troubleshooting

### `runner` works but `run` does not complete

Regenerate shell completions after installing or upgrading:

```sh
eval "$(runner completions)"
```

Both commands must live in the same bin directory for one generated completion
registration to cover both.

## More Docs

- Full README: <https://github.com/kjanat/runner#readme>
- Site: <https://runner.kjanat.dev>
- Issues: <https://github.com/kjanat/runner/issues>

## License

[MIT](https://github.com/kjanat/runner/blob/master/LICENSE) (c) 2026 Kaj Kowalski

[npm]: https://npm.im/runner-run
[socket]: https://socket.dev/npm/package/runner-run
[install]: https://github.com/kjanat/runner#install
