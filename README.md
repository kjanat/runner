# runner

Universal project task runner. Auto-detects toolchain, provides unified CLI.

## Features

- **Auto-detection**: Scans for lockfiles/configs and picks the right tool
- **Unified interface**: Same workflow across npm/yarn/pnpm/bun/cargo/deno/uv/poetry/pipenv/go/bundler/composer
- **Task aggregation**: Lists tasks from package.json/package.json5/package.yaml, Makefile, justfile, Taskfile, turbo.json, deno.json, deno.jsonc
- **Deterministic task routing**: Prefers turbo task, then package.json, then other matching sources
- **Monorepo aware**: Detects workspaces (turbo, nx, pnpm, npm/yarn workspaces, Cargo workspaces)
- **Resilient detection**: Surfaces non-fatal parse/read warnings in info/list/run output
- **Safe clean defaults**: Skips framework build dirs unless explicitly requested
- **Node version checking**: Warns on .nvmrc/.node-version mismatch

## Tool Support

**Package managers (detect + install + exec):** npm, yarn, pnpm, bun, cargo, deno, uv, poetry, pipenv, go, bundler, composer

**Task sources (list + run):** package manifests (`package.json`, `package.json5`, `package.yaml`) scripts, turbo.json tasks/pipeline, Makefile, justfile, Taskfile, deno.json, deno.jsonc

**Task-runner detection signals:** turbo, nx, make, just, go-task, mise

> Note: nx and mise are currently detection-only (metadata/monorepo context), not direct task execution backends.

## Usage

```sh
runner                              # show detected project info
runner <task> [-- <args...>]        # run task (auto-routed)
run <task> [-- <args...>]           # alias binary for quicker access
runner run <task> [-- <args...>]    # explicit run form
runner install [--frozen]           # install deps via detected PM
runner clean [-y] [--include-framework]
runner list [--raw]                 # list tasks (raw = one name per line)
runner exec <cmd> [args...]         # run command through PM
runner completions <shell>
```

## Install

```sh
# installs both binaries: runner + run
cargo install --path .

# Or from git
cargo install --git=https://github.com/kjanat/runner/ runner
```

Or use the dev wrapper:

```sh
./bin/runner <args>
./bin/run <args>

# If direnv is set up, just run:
runner <args>
```

## Shell Completions

```sh
mkdir -p ~/.local/share/bash-completion/completions
runner completions bash > ~/.local/share/bash-completion/completions/runner

mkdir -p ~/.zfunc
runner completions zsh > ~/.zfunc/_runner
# add once to ~/.zshrc:
# fpath=(~/.zfunc $fpath)
# autoload -Uz compinit && compinit

mkdir -p ~/.config/fish/completions
runner completions fish > ~/.config/fish/completions/runner.fish

# If you use the `run` alias binary, replace `runner` with `run`
# and output to matching completion filenames.
```

## License

[MIT][license]

[license]: https://github.com/kjanat/runner/blob/master/LICENSE
