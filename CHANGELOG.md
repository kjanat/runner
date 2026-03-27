# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Add `install.sh` convenience installer for Linux release assets, including
  latest/pinned version resolution, checksum verification, and arch selection.

### Changed

- Make installer destination fallback explicit as nested precedence:
  `RUNNER_INSTALL_DIR` -> `XDG_BIN_HOME` -> `~/.local/bin`.

### Post-release checklist

- [ ] Move completed `Unreleased` items into a new version section.
- [ ] Update the `[Unreleased]` compare link to the new tag.
- [ ] Create and push a signed `vX.Y.Z` tag from `master`.

## [0.1.0] - 2026-03-27

### Added

- Initial `runner` CLI release for unified project task execution.
- Auto-detection for package managers and task sources across ecosystems.
- `run` alias binary for shorter invocation.
- Unified commands for task run/list, dependency install, clean, and exec.

[Unreleased]: https://github.com/kjanat/runner/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/kjanat/runner/releases/tag/v0.1.0
