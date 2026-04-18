# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Detect justfile aliases (`alias b := build`) and surface them as
  first-class tasks in `runner list` and `runner <alias>`. Aliases
  inherit the target recipe's doc comment; private aliases (prefixed
  `_`, tagged `[private]`, or pointing at a private recipe) are hidden.
  Works via `just --dump-format json` when `just` is on PATH and via the
  regex fallback parser otherwise.

### Post-release checklist

- [ ] Move completed `Unreleased` items into a new version section.
- [ ] Update the `[Unreleased]` compare link to the new tag.
- [ ] Create and push a signed `vX.Y.Z` tag from `master`.

## [0.4.0] - 2026-04-17

### Added

- Add `shfmt` for shell script formatting via dprint command integration.
- `runner run <target>` now falls back to executing `<target>` through the
  detected package manager (`npx` / `pnpm exec` / `bunx` / `uv run` / …) when
  no matching task is defined, unifying task execution and ad-hoc command
  execution under one entrypoint.
- Shorthand `runner <name>` prefers a same-named task when one exists in the
  project, so tasks called `clean`, `install`, `list`, `info`, or
  `completions` are no longer shadowed by built-in subcommands. Passing any
  built-in-specific flag (`--frozen`, `-y`, `--include-framework`, `--raw`,
  the `completions` shell positional) keeps the built-in path as an escape
  hatch; `runner i` / `runner ls` aliases always hit the built-in.
- `run` alias binary now uses a dedicated parser (no subcommands), so
  `run clean`, `run install`, and friends always run the task/command even
  when the name matches a `runner` built-in.
- `runner completions <shell>` emits registration scripts for both `runner`
  and `run` in one invocation, so a single `eval "$(runner completions
  zsh)"` registers completion for both CLIs.
- `runner completions` now accepts `--output <PATH>` (`-o`) to write the
  script directly to a file instead of stdout. Parent directories are not
  auto-created; an existing file is overwritten; a stderr confirmation
  line (`wrote completion script to <PATH>`) is printed on success.
- Zsh completion honours path hints: `--dir <TAB>` (and any arg carrying
  `ValueHint::DirPath` / `FilePath` / `AnyPath` / `ExecutablePath`)
  delegates to zsh's native `_files` so `~/`, `~named-dir/`, globs, and
  `cdpath` all work.
- Declare MSRV `rust-version = "1.88"` in `Cargo.toml` (matches the
  let-chain syntax used throughout the crate).

### Changed

- Make `install.sh` accept both `X.Y.Z` and `vX.Y.Z` version arguments and
  environment overrides.
- Quiet installer downloads and checksum verification, and switch install
  output to a more compact structured summary with the installed version.
- Streamline install docs around the installer script and custom destination
  override details.
- `runner`'s shorthand for a name that matches a detected task is now
  preferred over the built-in subcommand when no built-in flag is set.
  **Breaking** for projects that relied on `runner install` / `runner clean`
  always hitting the built-in while also defining a same-named task.
- `run` alias binary now parses positionals unconditionally (no built-in
  subcommands); previously it inherited `runner`'s parser, so positional
  names that matched a built-in would dispatch there.
- Added `clap::ValueHint::DirPath` to the `--dir` flag on both CLIs so
  shell completion knows to offer directories.

### Removed

- **Breaking:** `runner exec <cmd>` is gone. Use `runner run <cmd>` (which
  now falls through to the package manager when no task matches) or the
  `run` alias binary.
- Remove the `tool::deno::exec_cmd` and `tool::cargo_pm::exec_cmd` helpers:
  `deno run <target>` treats the target as a local script, and
  `cargo <target>` dispatches to a cargo subcommand/plugin — neither runs
  arbitrary package binaries like `npx` does. `runner run <target>` in a
  Deno- or Cargo-only project now spawns `<target>` directly via `PATH`.

### Fixed

- Stop zsh completion from leaking caller-side `XTRACE` / alias / word-split
  state into the prompt: the completer function now starts with
  `emulate -L zsh`.
- `runner run <name>` in a Deno project no longer fires off
  `deno run <name>` (which would misinterpret `<name>` as a script path).
- Scope completion flag-hint lookup to the active subcommand chain instead
  of recursing through every subcommand: a sibling subcommand's `--flag`
  with a different `ValueHint` no longer bleeds into an unrelated context,
  and a subcommand-local boolean `--flag` correctly shadows an ancestor's
  value-taking definition.

## [0.3.1] - 2026-04-15

### Added

- Help output now includes a quiet `by Kaj Kowalski` attribution line, with an
  OSC8 `mailto:` link when rendered to a terminal.

### Changed

- Enable clap's `cargo`, `env`, and `wrap_help` features, and use clap cargo
  macros for package description/version metadata.
- Shorten help copy for `runner completions` and `--dir`, and show
  `RUNNER_DIR` directly in `--help` output.
- Make clap value parsers explicit for `--dir <PATH>` and the optional
  completions shell argument.

### Fixed

- Resolve Deno tasks, `deno.json(c)` configs, and `package.json` scripts from
  the nearest applicable ancestor config, while stopping at VCS boundaries and
  ignoring workspace roots that do not list the current path as a member.
- Prefer the nearest Deno task source when duplicate task names exist across
  ancestor configs.

## [0.3.0] - 2026-04-15

### Added

- Add global `--dir <PATH>` and `RUNNER_DIR` overrides to scan and run tasks
  against another project directory.

### Fixed

- Resolve Deno tasks and configs from the nearest applicable ancestor config,
  while stopping at VCS boundaries and ignoring workspace roots that do not
  list the current path as a member.
- Limit task source OSC8 hyperlinks to visible filename text so alignment
  padding is not clickable.
- Add repo and release-tag hyperlinks to `runner --version` and the `runner`
  info header version display.

## [0.2.1] - 2026-04-15

### Changed

- Bump `clap_complete` to `4.6.2`.

### Fixed

- Detect Deno projects from `packageManager: "deno@..."` and `deno.lock`
  instead of defaulting those repos to `npm`.
- Keep `package.json` tasks available for Deno projects during task discovery.

## [0.2.0] - 2026-03-29

### Added

- Add `install.sh` convenience installer for Linux release assets, including
  latest/pinned version resolution, checksum verification, and arch selection.
- Dynamic shell completions with live task candidates, source tags, and
  descriptions instead of static subcommand lists.
- Auto-detect shell from `$SHELL` when no completion argument is given.
- `description` field on `Task`, threaded from justfile doc comments and
  go-task `desc` fields into completion candidates.
- Tag-grouped zsh completions — candidates render under section headers
  (e.g. `-- justfile --`, `-- Commands --`) via custom `_describe` adapter.

### Changed

- Make installer destination fallback explicit as nested precedence:
  `RUNNER_INSTALL_DIR` -> `XDG_BIN_HOME` -> `~/.local/bin`.
- Extract zsh completion script to standalone `grouped.zsh` file, embedded
  via `include_str!` for syntax highlighting and linting support.

### Fixed

- Correct checksum filename in installer.

## [0.1.0] - 2026-03-27

### Added

- Initial `runner` CLI release for unified project task execution.
- Auto-detection for package managers and task sources across ecosystems.
- `run` alias binary for shorter invocation.
- Unified commands for task run/list, dependency install, clean, and exec.

[Unreleased]: https://github.com/kjanat/runner/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/kjanat/runner/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/kjanat/runner/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/kjanat/runner/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/kjanat/runner/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/kjanat/runner/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/kjanat/runner/releases/tag/v0.1.0
