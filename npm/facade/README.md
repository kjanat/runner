# runner-run

Universal project task runner. Auto-detects the toolchain
(npm/yarn/pnpm/bun/cargo/deno/uv/poetry/pipenv/go/bundler/composer) and
provides a unified CLI for installing dependencies, listing tasks, and
running them. Works in monorepos (turbo, nx, pnpm/npm/yarn workspaces,
Cargo workspaces).

## Install

```sh
npm install -g runner-run
# or
pnpm add -g runner-run
# or
yarn global add runner-run
# or per-project
npm install --save-dev runner-run
```

This installs two commands: `runner` and `run`.

## Usage

```sh
runner                              # show detected project info
runner <task> [-- <args...>]        # run task (falls back to PM exec)
run <task> [-- <args...>]           # alias: always runs as task/exec
runner install [--frozen]           # install deps via detected PM
runner clean [-y] [--include-framework]
runner list [--raw]                 # list tasks
runner completions [<shell>]        # shell completion script
```

See the [project README](https://github.com/kjanat/runner#readme) for full
documentation.

## How distribution works

`runner-run` is a façade package. It declares one
`@runner-run/<platform>-<arch>[-<libc|abi>]` package per supported
platform in `optionalDependencies`. npm/pnpm/yarn use each sub-package's
`os` / `cpu` / `libc` fields to install only the one matching your
machine — there is no `postinstall` script and no network access at
install time. The `runner` and `run` shims locate the installed
sub-package via `require.resolve` at runtime and exec the native binary.

### Supported platforms

| OS      | Architectures                          |
| ------- | -------------------------------------- |
| Linux   | x64 (gnu, musl), arm64 (gnu, musl), armv7 (gnu) |
| macOS   | x64, arm64                             |
| Windows | x64, arm64, ia32                       |
| FreeBSD | x64, arm64 (experimental)              |
| NetBSD  | x64                                    |
| OpenBSD | x64 (experimental)                     |

If your platform isn't listed, install from source with
`cargo install runner` or
[file an issue](https://github.com/kjanat/runner/issues).

### Troubleshooting

> `runner-run: no prebuilt binary found for <platform>-<arch>`

Your package manager skipped `optionalDependencies`. Common causes:

- `npm install --omit=optional` / `--no-optional`
- `yarn install --ignore-optional`
- pnpm with `optional=false` configured
- A stale lockfile committed from a different OS — regenerate it on the
  target OS, or run `npm install --force`

Reinstall without those flags, or use `cargo install runner`.

## License

[MIT](https://github.com/kjanat/runner/blob/master/LICENSE) © Kaj Kowalski
