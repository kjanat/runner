# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Post-release checklist

- [ ] Move completed `Unreleased` items into a new version section.
- [ ] Update the `[Unreleased]` compare link to the new tag.
- [ ] Create and push a signed `vX.Y.Z` tag from `master`.

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

[Unreleased]: https://github.com/kjanat/runner/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/kjanat/runner/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/kjanat/runner/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/kjanat/runner/releases/tag/v0.1.0
