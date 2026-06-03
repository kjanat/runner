# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog], and this project adheres to [Semantic Versioning].

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
[Semantic Versioning]: https://semver.org/spec/v2.0.0.html

## [Unreleased]

### Added

- AUR distribution channel. Two packages on the Arch User Repository:
  `runner-run-bin` (prebuilt binaries for `x86_64`, `aarch64`, `armv7h`)
  and `runner-run` (source build for `x86_64`, `aarch64`). `-bin`
  `provides`/`conflicts` `runner-run`, so install whichever you prefer —
  https://aur.archlinux.org/packages/runner-run-bin and
  https://aur.archlinux.org/packages/runner-run.
- Shell completions shipped by both AUR packages and auto-loaded from
  the canonical system dirs: bash at
  `/usr/share/bash-completion/completions/{runner,run}`, zsh at
  `/usr/share/zsh/site-functions/{_runner,_run}`, fish at
  `/usr/share/fish/vendor_completions.d/{runner,run}.fish`. PowerShell
  on Linux has no autoload convention, so the pwsh script is installed
  at `/usr/share/runner/runner.ps1` for users to dot-source from their
  `$PROFILE`. Completions are clap-dynamic — the shell shells out to
  the binary for candidates, so tab-completing in a project picks up
  the *current* task list from `package.json` / `turbo.json` /
  `Justfile` / etc., not a static snapshot.
- `.github/workflows/aur-release.yml` publishes both packages on every
  `release: published` event (with manual `workflow_dispatch` +
  `dry-run` for validation). Gated behind a dedicated `aur` GitHub
  Environment so the `AUR_SSH_PRIVATE_KEY` secret is only readable from
  that job. Per-pkg `concurrency:` group serializes manual + automatic
  triggers for the same package without blocking the other matrix leg.
- `.github/scripts/publish/aur-prepare.sh` rewrites `pkgver`/`pkgrel`
  in the checked-in PKGBUILDs and, for the `-bin` package, injects
  per-arch `sha256sums_*` read directly from the release's published
  `.sha256` companion assets (avoids the `updpkgsums` host-arch-only
  limitation). Strict semver regex on the version input refuses
  anything containing `&`, `/`, `\`, or newlines before any `sed`
  runs.
- Man pages for `runner`, `run`, and each subcommand. Rendered from the
  clap command tree by a `man` subcommand gated behind the `man`
  feature (off by default — never in the shipped binary, never committed)
  and shipped by every channel: crates.io (in the published crate), npm
  (facade `man` field), both AUR packages (`/usr/share/man/man1/`), and a
  `runner-<tag>-man.tar.gz` GitHub release asset that `install.sh` and
  `runner-run-bin` pull from. `man runner` / `man run` work everywhere.

### Security

- All third-party `uses:` in `crates-release.yml`, `npm-release.yml`,
  and `release.yml` pinned to commit SHAs (with a `# vN` trailing
  comment for readability), so an upstream tag rewrite or
  account-takeover cannot silently swap in a different action build.
- `persist-credentials: false` added to the two `actions/checkout`
  steps in `release.yml` that were missing it (`create-release`,
  `upload-assets`), matching the hardening already in place on the
  other checkouts.

### Post-release checklist

- [ ] Move completed `Unreleased` items into a new version section.
- [ ] Update the `[Unreleased]` compare link to the new tag.
- [ ] Create and push a signed `vX.Y.Z` tag from `master`.

## [0.12.0] - 2026-06-01

### Added

- GitHub Actions log grouping for task output. Sequential and single
  task runs wrap each execution in a `runner: <task>` section, emitted
  as `::group::` / `::endgroup::` workflow commands under GitHub Actions
  (and left untouched in a plain terminal). Toggle with
  `[github].group_output` in `runner.toml` (default `true`).
- Grouped parallel (`-p`) output. Each task's stdout/stderr is captured
  and printed as one contiguous `runner: <task>` block when that task
  finishes (completion order — first done, first shown), instead of
  interleaving lines live. Under GitHub Actions the block is a `::group::`
  section; elsewhere it gets a plain colored header. Defaults diverge by
  environment so CI and local can differ: `[github].group_parallel`
  (default `true`, only when `[github].group_output` is also `true`)
  governs runs under GitHub Actions, `[parallel].grouped` (default
  `false`) governs runs elsewhere. Opting out on either path restores the
  live `[<task>]`-prefixed multiplexer.
- `[github]` and `[parallel]` sections in `runner.toml`, reflected in the
  generated JSON schema, for the grouping toggles above.
- `actions-rs` dependency for emitting GitHub Actions workflow commands.

## [0.11.0] - 2026-05-19

### Added

- Task chaining for `runner run` and `runner install`. New `-s` /
  `--sequential` and `-p` / `--parallel` flags turn the trailing
  positionals into a chain: `runner run -s build test lint` runs
  the three tasks in order; `runner run -p test:unit test:e2e`
  fans them out concurrently. `runner install build test` chains
  install → build → test (install head is always sequential; `-p`
  is rejected on `install`).
- Failure policies for chains. Default is fail-fast (sequential
  stops on first non-zero; parallel lets running siblings finish,
  doesn't start new ones). `-k` / `--keep-going` runs every task
  to completion regardless of failures, with the chain's final
  exit code reflecting the first failure. `--kill-on-fail`
  (parallel only) terminates siblings immediately when one fails.
  `-k` and `--kill-on-fail` are mutually exclusive across CLI,
  env, and config — conflicting layers surface
  `ResolveError::ConflictingFailurePolicy` with the offending
  source named.
- `[chain]` section in `runner.toml` plus `RUNNER_KEEP_GOING` /
  `RUNNER_KILL_ON_FAIL` env-var mirrors. Same resolver-chain
  precedence as the rest of the policy knobs: CLI > env > config.
  Env layer is presence-authoritative — `RUNNER_KEEP_GOING=0`
  overrides `[chain].keep_going = true` in config, not just the
  default.
- Line-prefix multiplexer for parallel chain output. Each task's
  piped stdout/stderr is captured by a reader thread, prefixed
  with `[<task-name>]` (right-padded to the longest name, colored
  from an 8-slot deterministic palette), and forwarded to the
  parent's stdout/stderr. Honors `NO_COLOR` and non-TTY parents.
- Resolver-warning deduplication across chain dispatch. Per-task
  warnings collect into a shared `HashSet`, then emit once at the
  end sorted by `Display` so output order is stable across runs.

### Changed

- `runner install --frozen <tasks>` now propagates the `--frozen`
  flag into the synthetic install head of the chain, so the
  install step runs lockfile-only when the flag is set. Previous
  behavior silently dropped the flag in chain mode.
- Root single-binary Go task name now derives from the `module`
  path in `go.mod` (last segment, with a `/vN` major-version
  suffix dropped to match how Go names the built binary) instead
  of the project directory name, so cloning a repo into a
  differently-named directory no longer changes the task name.
  Falls back to the directory name only when `go.mod` is absent
  or has no parseable `module` line.

### Fixed

- Node script discovery is no longer gated on a *detected*
  package manager. A `package.json` with `scripts` but no
  lockfile and no `packageManager` / `devEngines` field (a
  typical pnpm-workspace member directory) reported "No project
  detected" and `runner run build` fell through to a bogus
  `bun build`. Manifest presence is now the Node signal; *which*
  PM dispatches scripts is the resolver's runtime job. A
  manifest-less subdirectory still lists ancestor scripts, but
  only when it provably sits inside a JS monorepo
  (workspace-root-aware, VCS-bounded), so an unrelated outer
  project's `package.json` is never silently adopted.
  https://github.com/kjanat/runner/pull/32
- Detection now mirrors the resolver's package-manager chain
  (`packageManager` → `devEngines.packageManager` →
  enclosing-workspace lockfile/manifest), so `runner info` /
  `runner install` from a workspace member target the
  workspace's tool instead of resolving nothing. Corepack
  semantics preserved: a present-but-unparseable legacy
  `packageManager` still warns and is not silently superseded by
  `devEngines`.

## [0.10.0] - 2026-05-14

### Added

- mise task extraction and dispatch. `mise` was previously
  detection-only — `runner` listed it under "Task Runners" but its
  tasks were invisible to `runner list` and `runner run <task>`.
  New `TaskSource::MiseToml` makes mise a first-class source: tasks
  declared in `mise.toml` / `.mise.toml` (and the `*.local.toml`,
  `mise/config.toml`, `.config/mise.toml` companions in mise's
  documented precedence) appear in listings, participate in the
  selection priority, and dispatch via `mise run <task>`.
- Bacon-style two-tier extraction for mise. Primary path shells
  out to `mise tasks --json` — authoritative across mise's config
  layering and file-based tasks (`mise-tasks/*`); fallback parses
  the first project-local config when `mise` isn't on `$PATH`.
  Both paths exclude hidden tasks (`hide = true`),
  underscore-prefixed names, and tasks whose `source` lives
  outside the project root (so global / `~/.config/mise/*` tasks
  don't pollute the project's task list). Empty or
  whitespace-only `description = ""` values are treated as
  missing so the renderer falls through to the `run` body or
  `file` reference instead of showing a blank column. Aliases
  come through as separate entries pointing at their target,
  mirroring the justfile shape.

### Fixed

- Resolver no longer dispatches through a Node package manager in
  projects with no Node-ecosystem evidence (https://github.com/kjanat/runner/issues/23).
  `runner run <unknown-task>` in a Go-only repo with `bun` installed used to
  warn "no node signals matched" and then run `bun <task>` anyway;
  the `FallbackPolicy::Probe` PATH probe now requires a
  `package.json` (or equivalent manifest) somewhere upward of
  `ctx.root` before it considers the canonical Node order.
  Without that evidence the resolver returns the existing soft
  `NoSignalsFound` sentinel and `cmd::run::run_pm_exec_fallback`
  spawns the target directly on `$PATH` — no more wrong-ecosystem
  dispatch.

## [0.9.0] - 2026-05-13

### Added

- Unified package-manager resolution chain. `runner run` now follows a
  documented 8-step precedence — qualified syntax → `--pm` / `--runner`
  → `RUNNER_PM` / `RUNNER_RUNNER` → `runner.toml` → `package.json`
  (`packageManager` then `devEngines.packageManager`) → lockfile →
  `PATH` probe → terminal error — making toolchain selection
  predictable across Corepack, antfu/ni, mise, and pnpm v11+
  conventions. New `src/resolver/` module owns the chain end-to-end.
- `--pm` / `--runner` global flags with `RUNNER_PM` / `RUNNER_RUNNER`
  env-var mirrors. Cross-ecosystem overrides like `--pm cargo` against
  a Node project fall through to detection rather than hijacking
  dispatch. CLI wins over env; empty env strings are treated as unset.
- `runner.toml` project config (`[pm]`, `[task_runner]`,
  `[resolution]`, `[sources.*]`). Per-ecosystem PM overrides apply
  only when the named PM matches the requested ecosystem;
  `[resolution].fallback` controls the no-signal behavior.
- `devEngines.packageManager` parsing from `package.json` (OpenJS
  proposal, npm 10.9+). Single-object and array forms supported with
  per-entry `onFail` (ignore/warn/error). `download` collapses to
  `warn` since runner is not an installer. The legacy `packageManager`
  field still wins when both are present; manifest declarations win
  over lockfile signals (Corepack semantics) and emit a `package.json`
  warning when they disagree.
- Semver enforcement on `devEngines.version`. When the declared range
  doesn't match the installed PM's `--version`, `onFail=warn` emits a
  warning and `onFail=error` bails. Unparseable ranges or missing
  `--version` output skip the check silently so a partially-broken
  environment never blocks dispatch.
- `--fallback` policy (`probe` default | `npm` legacy | `error`) with
  `RUNNER_FALLBACK` env mirror. The default replaces the silent `npm`
  fallback with a canonical-order PATH probe (`bun > pnpm > yarn >
  npm`); when nothing matches, the user sees an actionable error
  listing every source that was checked.
- `--explain` flag (`RUNNER_EXPLAIN` env). Emits a one-line trace
  describing which chain step produced the PM decision: `· runner
  resolved: pnpm via package.json "packageManager"`.
- `runner doctor` subcommand. Dumps every signal the resolver
  considers: detected PMs/runners, override sources in effect (with
  origin attribution), manifest declarations, lockfile presence,
  PATH probe results for each Node PM, the final decision, and any
  warnings. `--json` emits schema-versioned output for jq/scripts/bug
  reports.
- `runner why <task>` subcommand. Walks the source-selection chain for
  a single task: lists every candidate source with its `(priority,
  depth, display_order, alias)` tuple, names the winner, and renders
  the PM resolution trace when a `package.json` script is picked.
  `--json` available.
- Passthrough-wrapper detection generalized to every supported task
  runner. A `package.json` script like `"build": "just build"` is now
  recognized as a thin wrapper around `just` and deduped from
  completion candidates when the underlying runner exposes a same-
  named task. Recognized runners: turbo, just, make, task (go-task),
  nx, bacon, mise.
- `Ecosystem` enum (`Node`/`Deno`/`Python`/`Rust`/`Go`/`Ruby`/`Php`)
  formalizing the PM-to-ecosystem mapping used by override scoping.

### Changed

- `Task.passthrough_to_turbo: bool` replaced by `Task.passthrough_to:
  Option<TaskRunner>` so wrappers around any runner — not just turbo —
  can be attributed at detection time and used by completion.
- `cmd::run::run` signature now takes a `&ResolutionOverrides` so the
  resolver-chosen PM also flows through the no-task fallback paths
  (`bun test` special case, `npx`-style exec). `--pm npm` against a
  Bun-detected project now correctly suppresses the bun-test fallback.
- Task selection unified into a single sort key `(source_priority,
  source_depth, display_order, alias_last)`. Replaces the Deno-only
  depth path with a generic tiebreak that applies to every source.
  Every non-workspace-aware source now walks ancestors upward when
  computing `source_depth` so the tiebreak actually distinguishes
  nested Makefile/Justfile/Taskfile/bacon.toml configs from
  workspace-root ones.
- `Resolver::resolve_node_pm` returns `Result<ResolvedPm>` so manifest
  `onFail=error` and `--fallback error` can bubble up structured
  errors instead of bailing inside the resolver.

### Fixed

- `package.json` `packageManager: "deno@…"` projects no longer require
  a `deno.json` alongside to be recognized as a Deno project.
- Stale doc comment on `detect::push_package_json_tasks` updated to
  reflect that passthrough detection covers every known runner, not
  just turbo.

## [0.8.1] - 2026-05-12

### Fixed

- `Error: program not found` on Windows when `runner run <script>` dispatches
  through npm / yarn / pnpm (issue #20). Bare-name spawns now walk
  `PATH` × `PATHEXT` so `.cmd` / `.bat` shims (npm.cmd, yarn.cmd, pnpm.cmd)
  resolve the same way they do under cmd.exe / PowerShell. Same fix covers
  every other Windows-shimmed tool runner dispatches: turbo, deno, make,
  task, just, composer, poetry, pipenv, uv, bundle, go, bacon, and the
  `runner run <bin>` arbitrary-target fallback. Non-Windows targets are
  unchanged.

## [0.8.0] - 2026-05-10

### Added

- `bacon.toml` as a `runner` task source. Jobs surface in `runner list` /
  `runner info`, dispatch via `runner run <job>` / `run <job>`, and resolve
  under the `bacon.toml:<job>` qualified syntax. Bacon is detected as a
  task runner alongside just / make / go-task. Jobs whose names start with
  `_` are treated as private and hidden. When the `bacon` CLI is on
  `PATH`, extraction shells out to `bacon --list-jobs` so bacon's built-in
  jobs (`check`, `clippy`, `test`, …) merge into the listing alongside
  whatever `bacon.toml` declares — same view bacon itself presents. Falls
  back to TOML parsing when bacon isn't installed. Job arguments forward
  through bacon's `--` separator (`runner run test -- --ignored` →
  `bacon test -- --ignored`) so they reach the underlying job intact.
- Project-local `bacon.toml` defining `lint`, `test-all`, and `bins` jobs
  for the `runner` crate, mirroring the `cargo l` / `cargo t` aliases.
- `cargo binstall runner-run` support via `[package.metadata.binstall]` in
  `Cargo.toml`. cargo-binstall now downloads the prebuilt binary from the
  matching GitHub release asset (`runner-v{version}-{target}.tar.gz`)
  instead of building from source — same archives
  `taiki-e/upload-rust-binary-action` uploads from `release.yml`. Both
  `runner` and `run` install side by side, no toolchain required.

### Changed

- Release pipeline reordered. `crates-release` now triggers on `push:
  tags: ['v*']` instead of `release: published`, so `cargo publish`
  fires in parallel with binary builds and no longer waits on the npm
  publish chain to complete first. `release.yml` gains a final
  `publish-release` job that flips the draft GitHub release to
  published once binaries and the `npm-dist` artifact land — this is
  now the natural pivot of the release lifecycle and drives
  `npm-release.yml` via `release: published`. `npm-release.yml`
  drops its `workflow_run` trigger (and the draft-flip side job that
  was hidden in it), resolving the build run-id for cross-workflow
  artifact download via `gh run list` instead. Net effect: tag push
  alone ships crates.io immediately, and the GH release auto-publishes
  once binaries are ready — no more manual draft-flipping.
- `npm/facade/README.md` updates the install fallback instructions to
  `cargo install runner-run` (crates.io) instead of the git-source
  form, matching the 0.7.1 README/landing-page change.
- `npm/facade/package.json` template no longer carries a `version`
  field. The build script (`npm/scripts/build-packages.ts`) injects
  the version from `cargo metadata` at build time and the template
  value was always overwritten. Single source of truth is now
  `Cargo.toml`.

### Fixed

- `runner completions $SHELL` no longer fails when `$SHELL` expands to a
  full path (e.g. `/usr/bin/zsh`). The explicit `<shell>` arg now accepts
  either a bare name (`zsh`) or a full path, mirroring the bare-arg
  fallback that already file-stems `$SHELL`. Previously the stock clap
  `ValueEnum` parser rejected anything but the bare names, making the
  explicit and implicit paths inconsistent.
- `pwsh` surfaces in `runner completions --help` and the rejection error
  alongside `powershell`. The internal mapping has accepted both since
  `shell_from_path` was written; only the user-facing message lagged.

## [0.7.1] - 2026-05-10

### Fixed

- Turborepo configs resolve under both `turbo.json` and `turbo.jsonc`
  filenames, and either is parsed as JSONC (line/block comments and
  trailing commas), matching Turborepo v2's own parser. Detection
  previously hardcoded `turbo.json` and the parser was strict JSON, so
  `.jsonc` files were invisible to detection and any JSONC syntax
  surfaced parse errors. The qualified-task syntax also accepts
  `turbo.jsonc:task` (and `deno.jsonc:task`, fixed in the same line for
  parity). Fixes #10.
- Root Tasks in `turbo.json` — entries written with the `//#name`
  prefix, invoked via `turbo run name` against the workspace root —
  now surface in `runner list` under their bare name. Workspace-scoped
  entries (`pkg#task`) remain filtered, and the result set is
  deduplicated when both `name` and `//#name` are defined. Fixes #11.

### Changed

- `crates-release` CI workflow uses crates.io trusted publishing via
  OIDC. Replaces the long-lived `CARGO_REGISTRY_TOKEN` secret with a
  short-lived token minted per run by `rust-lang/crates-io-auth-action`;
  the secret-presence preflight is gone.
- README and the landing page promote `cargo install runner-run` (from
  crates.io) as the primary cargo install command, with the git and
  local-checkout forms remaining as fallbacks for unreleased commits
  and development work.
- README adds a crates.io shields badge alongside the existing npm
  one; the landing page footer adds crates.io and npm registry links
  next to the source and changelog references.

## [0.7.0] - 2026-05-10

### Added

- crates.io publishing: new `crates-release` workflow publishes the
  crate to crates.io when a maintainer publishes the GitHub release
  that `release.yml` cuts as a draft. Verifies the tag matches
  `Cargo.toml`, runs `cargo publish --locked --dry-run` first, then
  publishes via `CARGO_REGISTRY_TOKEN` under the `crates-io`
  environment. `workflow_dispatch` is preserved for manual republishes.

### Changed

- Renamed the published package from `runner` to `runner-run` (the
  bare `runner` name is taken on crates.io). The library crate name
  is pinned to `runner` via `[lib]` so existing `runner::…` imports
  in `src/main.rs` and `src/bin/run.rs` keep working unchanged.
- GitHub composite action: `uses: kjanat/runner@vX.Y.Z` installs the
  `runner` and `run` binaries on PATH in CI. Pinned tag refs make zero
  API calls (tag resolved from `github.action_ref`); `version: latest`
  triggers a single `releases/latest` lookup. Parallel `curl` of
  archive + sha256 with `--retry-all-errors --retry 5`, sha256
  verification before extract, `actions/cache@v4` keyed on `tag +
  triple` (cache hits ~250 ms, cold install 600–900 ms), and a
  `runner --version` smoke test on every install so a bad cache or
  missing asset surfaces here, not at the consumer's first task.
- Cargo aliases as a `runner` task source. The hierarchical
  `.cargo/config.toml` chain (cwd up to filesystem root, then
  `$CARGO_HOME/config{,.toml}`) is merged with cargo's precedence
  rules, recursive alias chains are expanded so `runner list` shows
  the fully-resolved command (`l → clippy --all-targets --all-features
  -- -D warnings`, `recursive_example → run --release --example
  recursions`), and built-ins (`b/c/d/t/r/rm`) always surface even in
  projects without a user config. User attempts to redefine a built-in
  are silently ignored to match cargo's own rule. `runner run <alias>`
  shells out to `cargo <alias> <args...>` so cargo's runtime resolution
  stays authoritative.
- Landing page at <https://runner.kjanat.com>, deployed to Cloudflare
  Workers Assets from `site/`. Single static page, dark mode via
  `prefers-color-scheme`, click-to-copy install commands with polite
  ARIA live-region announcements, tab-completion section, custom 404
  styled as a fake `runner <path>` error line. Page weight squeezed
  under TCP IW10 (~14.5 KB uncompressed, ~3.6 KB brotli) so the whole
  first response lands in a single round-trip; `_headers` ships
  strict CSP, HSTS, and edge cache.
- Templated site build (`site/build.ts` + `site/dev.ts`): Bun bundles
  `index.html` / `404.html` from `src/` into `dist/` and substitutes
  `{{version}}`, `{{repo}}`, `{{authorName}}` from `Cargo.toml`, so
  the site and the crate share one source of truth for metadata.
  `dev.ts` serves `dist/`, watches `src/` / `public/` / `Cargo.toml`
  with an 80 ms debounce, and injects a WebSocket live-reload snippet
  into served HTML.
- `site/build.ts` returns the emitted file list with bytes; the CLI
  prints a sorted `raw / gzip / br` table at quality 9/11 to mirror
  what a CDN actually serves, surfacing budget regressions next to
  the build. HTML post-processing reads from `Bun.build` outputs in
  memory instead of re-reading `dist/`, and `copyTree` returns the
  bytes it copied so `public/` files land in the same `DistFile[]`
  the summary walks.
- `BuildOptions.dir` (`"relative" | "full"`) toggles the size-summary
  path column between `dist/`-relative and absolute, surfaced via the
  `FULL` env in the local build script. Public files respect the
  toggle too so the rendered table is consistent across sources.
- GitHub Pages-aware `publicPath`: under GitHub Actions the asset
  prefix derives to `https://<owner>.github.io/<repo>/` from
  `GITHUB_REPOSITORY`, so PR previews of forks load their own assets
  without a config flag. `PUBLIC_PATH` env still overrides everything.
- `CF_BEACON_TOKEN` env overrides the inlined Cloudflare Web
  Analytics token; the literal stays as a fallback so production
  builds without the env still report correctly.
- External sourcemaps emit when `SENTRY_DSN` is set; otherwise the
  build skips them entirely so the published site stays
  one-round-trip-sized for end users.
- README links the landing page and npm package, and adds shields.io
  badges for the `runner-run` npm version and the MIT licence.

### Changed

- Cloudflare Web Analytics beacon now only injects in CI / GitHub
  Actions builds. Local `bun run build` and library imports leave the
  snippet out so dev previews don't phone home, and a missing
  `</body>` warns instead of throwing so partial HTML fragments don't
  fail the build.
- `npm/scripts/build-packages.ts` swaps the deprecated
  `cargo read-manifest` for
  `cargo metadata --no-deps --format-version 1`, picking the
  workspace's `workspace_default_members[0]` by id (falls back to the
  first package for non-virtual single-member workspaces). `maxBuffer`
  bumped to 64 MiB so large workspaces don't trip Node's 1 MiB
  default.
- `.github/scripts/publish/npm.sh` derives `REQUIRED_PLATFORMS` and
  `OPTIONAL_PLATFORMS` from `npm/targets.json` at runtime via `jq`
  instead of hardcoding two parallel lists; `npm-release.yml`
  sparse-checkout adds `npm/targets.json` so the script can read it.
  Optional == `experimental: true`, matching the workflow's existing
  `continue-on-error` semantic. One source of truth for the platform
  matrix.
- Drop the unused `NODE_AUTH_TOKEN` env from the npm publish step;
  auth flows through the OIDC token via `id-token: write` and
  `npm publish --provenance`, not a long-lived `NPM_TOKEN`.
- Move the landing-page primary domain from `runner.kjanat.com` to
  `runner.kjanat.dev` across the root README, site docs, site package
  metadata, and the page canonical URL so published links and metadata
  point at the new host.
- Add `runner.kjanat.dev` as a Cloudflare custom-domain route while
  keeping the existing `.com` route active during the transition.
- Reflow the npm facade README intro for cleaner package-page rendering.
- Set `Cargo.toml` `[package].homepage` and `npm/facade/package.json`
  `homepage` to `https://runner.kjanat.dev` (was the GitHub README
  URL), so the published crate and npm package surface the landing
  page on registry pages.

### Fixed

- `site/build.ts` `publicPath` precedence: the original
  `env["PUBLIC_PATH"] || isCI ? X : Y` parsed as
  `(... || ...) ? X : Y`, so a literal `PUBLIC_PATH` value never
  reached `Bun.build` — it acted as a boolean toggle. The hardcoded
  `runner.kjanat.com/` fallback also leaked into Cloudflare Workers
  preview deploys (`*.workers.dev`) and tripped CSP `'self'`,
  blocking every asset on every PR preview. Replaced with
  `env["PUBLIC_PATH"] || githubPagesUrl() || "/"`: explicit override
  wins, GitHub Pages still gets its `/<repo>/` prefix, everything
  else stays same-origin.
- `public/_headers` drops the dead `/favicon.ico` Content-Type
  override block; the icon ships as an SVG via `<link rel="icon">`,
  so nothing routes through `/favicon.ico` to need it.
- `.github/scripts/build/package-release-asset.sh` writes checksum
  files as `<basename>.sha256` (not `<basename>.tar.gz.sha256`),
  matching `taiki-e/upload-rust-binary-action`'s convention and what
  `verify-checksum.sh` enforces — the previous mismatch would have
  broken the npm pipeline's checksum verification on release.
- `npm/scripts/build-packages.ts`: `Target.build` union now covers
  all five schema enum values (previously only `cargo` | `cross`,
  missing the three variants added when the BSD build paths landed)
  so type narrowing matches reality and a stray build-tool name
  fails the build instead of silently shipping.
- Remove the stale `openbsd-x64` entry from `npm.sh`'s
  `OPTIONAL_PLATFORMS` and the matching matrix-gen comments in
  `release.yml` / `release-dryrun.yml`; the openbsd build path was
  scrapped earlier and the residue would have failed
  `optionalDependencies` validation if the platform was ever
  re-expected.
- Tab completion for turborepo monorepos no longer triple-emits
  the same task. A `package.json` script is classified as a turbo
  passthrough at detection time when its command body literally
  invokes `turbo run <name>` (or the shorthand `turbo <name>`) for
  a same-named target, optionally followed by flag tokens
  (`--filter web`, `--concurrency=4`) or — after a bare `--`
  end-of-options separator (POSIX/getopt convention) — args
  forwarded to the underlying task; the full bash control set
  (`&&`, `||`, `;`, `;;`, `;&`, `;;&`, `|`, `|&`, `&`, `!`, `{`,
  `}`, `(`, `)`), fd-style redirects (bare `>`/`<`/`>>`/`<<<`,
  combined-fd `&>`/`>&`, fd-prefixed `2>`, composite `2>&1`,
  `2>/dev/null`, `&>file.log`), shell expansion (parameter
  `$X`/`${X}`/`${X:-def}`/`${X//a/b}`, special vars `$@`/`$*`/
  `$#`/`$?`, command substitution `$(cmd)` and backtick
  `` `cmd` ``, arithmetic `$((expr))`, double-quoted forms with
  embedded expansion `"${X}"`) — including those positioned after
  a value-expecting flag or after `--` — and extra positional
  targets all reject the match so scripts that do real work
  beyond dispatching to turbo stay visible. Only thin passthroughs
  are dropped from completion when a same-named `turbo.json` task
  also exists. Real scripts like `"build": "vite build"` keep
  their qualified form even when they happen to share a name with
  a turbo task. `runner list` still surfaces both sources for
  transparency, `runner build` already dispatched through turbo
  per `source_priority`, and a third source (e.g. Makefile) keeps
  its qualified form alongside `turbo.json:build` for
  disambiguation.

## [0.6.1] - 2026-05-08

### Added

- GitHub composite action: `uses: kjanat/runner@vX.Y.Z` installs the
  `runner` and `run` binaries on PATH in CI. Pinned tag refs make zero
  API calls (tag resolved from `github.action_ref`); `version: latest`
  triggers a single `releases/latest` lookup. Parallel `curl` of
  archive + sha256 with `--retry-all-errors --retry 5`, sha256
  verification before extract, `actions/cache@v4` keyed on `tag +
  triple` (cache hits ~250 ms, cold install 600–900 ms), and a
  `runner --version` smoke test on every install so a bad cache or
  missing asset surfaces here, not at the consumer's first task.
- Cargo aliases as a `runner` task source. The hierarchical
  `.cargo/config.toml` chain (cwd up to filesystem root, then
  `$CARGO_HOME/config{,.toml}`) is merged with cargo's precedence
  rules, recursive alias chains are expanded so `runner list` shows
  the fully-resolved command (`l → clippy --all-targets --all-features
  -- -D warnings`, `recursive_example → run --release --example
  recursions`), and built-ins (`b/c/d/t/r/rm`) always surface even in
  projects without a user config. User attempts to redefine a built-in
  are silently ignored to match cargo's own rule. `runner run <alias>`
  shells out to `cargo <alias> <args...>` so cargo's runtime resolution
  stays authoritative.
- Landing page at <https://runner.kjanat.com>, deployed to Cloudflare
  Workers Assets from `site/`. Single static page, dark mode via
  `prefers-color-scheme`, click-to-copy install commands with polite
  ARIA live-region announcements, tab-completion section, custom 404
  styled as a fake `runner <path>` error line. Page weight squeezed
  under TCP IW10 (~14.5 KB uncompressed, ~3.6 KB brotli) so the whole
  first response lands in a single round-trip; `_headers` ships
  strict CSP, HSTS, and edge cache.
- Templated site build (`site/build.ts` + `site/dev.ts`): Bun bundles
  `index.html` / `404.html` from `src/` into `dist/` and substitutes
  `{{version}}`, `{{repo}}`, `{{authorName}}` from `Cargo.toml`, so
  the site and the crate share one source of truth for metadata.
  `dev.ts` serves `dist/`, watches `src/` / `public/` / `Cargo.toml`
  with an 80 ms debounce, and injects a WebSocket live-reload snippet
  into served HTML.
- `site/build.ts` returns the emitted file list with bytes; the CLI
  prints a sorted `raw / gzip / br` table at quality 9/11 to mirror
  what a CDN actually serves, surfacing budget regressions next to
  the build. HTML post-processing reads from `Bun.build` outputs in
  memory instead of re-reading `dist/`, and `copyTree` returns the
  bytes it copied so `public/` files land in the same `DistFile[]`
  the summary walks.
- `BuildOptions.dir` (`"relative" | "full"`) toggles the size-summary
  path column between `dist/`-relative and absolute, surfaced via the
  `FULL` env in the local build script. Public files respect the
  toggle too so the rendered table is consistent across sources.
- GitHub Pages-aware `publicPath`: under GitHub Actions the asset
  prefix derives to `https://<owner>.github.io/<repo>/` from
  `GITHUB_REPOSITORY`, so PR previews of forks load their own assets
  without a config flag. `PUBLIC_PATH` env still overrides everything.
- `CF_BEACON_TOKEN` env overrides the inlined Cloudflare Web
  Analytics token; the literal stays as a fallback so production
  builds without the env still report correctly.
- External sourcemaps emit when `SENTRY_DSN` is set; otherwise the
  build skips them entirely so the published site stays
  one-round-trip-sized for end users.
- README links the landing page and npm package, and adds shields.io
  badges for the `runner-run` npm version and the MIT licence.

### Changed

- Cloudflare Web Analytics beacon now only injects in CI / GitHub
  Actions builds. Local `bun run build` and library imports leave the
  snippet out so dev previews don't phone home, and a missing
  `</body>` warns instead of throwing so partial HTML fragments don't
  fail the build.
- `npm/scripts/build-packages.ts` swaps the deprecated
  `cargo read-manifest` for
  `cargo metadata --no-deps --format-version 1`, picking the
  workspace's `workspace_default_members[0]` by id (falls back to the
  first package for non-virtual single-member workspaces). `maxBuffer`
  bumped to 64 MiB so large workspaces don't trip Node's 1 MiB
  default.
- `.github/scripts/publish/npm.sh` derives `REQUIRED_PLATFORMS` and
  `OPTIONAL_PLATFORMS` from `npm/targets.json` at runtime via `jq`
  instead of hardcoding two parallel lists; `npm-release.yml`
  sparse-checkout adds `npm/targets.json` so the script can read it.
  Optional == `experimental: true`, matching the workflow's existing
  `continue-on-error` semantic. One source of truth for the platform
  matrix.
- Drop the unused `NODE_AUTH_TOKEN` env from the npm publish step;
  auth flows through the OIDC token via `id-token: write` and
  `npm publish --provenance`, not a long-lived `NPM_TOKEN`.

### Fixed

- `site/build.ts` `publicPath` precedence: the original
  `env["PUBLIC_PATH"] || isCI ? X : Y` parsed as
  `(... || ...) ? X : Y`, so a literal `PUBLIC_PATH` value never
  reached `Bun.build` — it acted as a boolean toggle. The hardcoded
  `runner.kjanat.com/` fallback also leaked into Cloudflare Workers
  preview deploys (`*.workers.dev`) and tripped CSP `'self'`,
  blocking every asset on every PR preview. Replaced with
  `env["PUBLIC_PATH"] || githubPagesUrl() || "/"`: explicit override
  wins, GitHub Pages still gets its `/<repo>/` prefix, everything
  else stays same-origin.
- `public/_headers` drops the dead `/favicon.ico` Content-Type
  override block; the icon ships as an SVG via `<link rel="icon">`,
  so nothing routes through `/favicon.ico` to need it.
- `.github/scripts/build/package-release-asset.sh` writes checksum
  files as `<basename>.sha256` (not `<basename>.tar.gz.sha256`),
  matching `taiki-e/upload-rust-binary-action`'s convention and what
  `verify-checksum.sh` enforces — the previous mismatch would have
  broken the npm pipeline's checksum verification on release.
- `npm/scripts/build-packages.ts`: `Target.build` union now covers
  all five schema enum values (previously only `cargo` | `cross`,
  missing the three variants added when the BSD build paths landed)
  so type narrowing matches reality and a stray build-tool name
  fails the build instead of silently shipping.
- Remove the stale `openbsd-x64` entry from `npm.sh`'s
  `OPTIONAL_PLATFORMS` and the matching matrix-gen comments in
  `release.yml` / `release-dryrun.yml`; the openbsd build path was
  scrapped earlier and the residue would have failed
  `optionalDependencies` validation if the platform was ever
  re-expected.
- Tab completion for turborepo monorepos no longer triple-emits
  the same task. A `package.json` script is classified as a turbo
  passthrough at detection time when its command body literally
  invokes `turbo run <name>` (or the shorthand `turbo <name>`) for
  a same-named target, optionally followed by flag tokens
  (`--filter web`, `--concurrency=4`) or — after a bare `--`
  end-of-options separator (POSIX/getopt convention) — args
  forwarded to the underlying task; the full bash control set
  (`&&`, `||`, `;`, `;;`, `;&`, `;;&`, `|`, `|&`, `&`, `!`, `{`,
  `}`, `(`, `)`), fd-style redirects (bare `>`/`<`/`>>`/`<<<`,
  combined-fd `&>`/`>&`, fd-prefixed `2>`, composite `2>&1`,
  `2>/dev/null`, `&>file.log`), shell expansion (parameter
  `$X`/`${X}`/`${X:-def}`/`${X//a/b}`, special vars `$@`/`$*`/
  `$#`/`$?`, command substitution `$(cmd)` and backtick
  `` `cmd` ``, arithmetic `$((expr))`, double-quoted forms with
  embedded expansion `"${X}"`) — including those positioned after
  a value-expecting flag or after `--` — and extra positional
  targets all reject the match so scripts that do real work
  beyond dispatching to turbo stay visible. Only thin passthroughs
  are dropped from completion when a same-named `turbo.json` task
  also exists. Real scripts like `"build": "vite build"` keep
  their qualified form even when they happen to share a name with
  a turbo task. `runner list` still surfaces both sources for
  transparency, `runner build` already dispatched through turbo
  per `source_priority`, and a third source (e.g. Makefile) keeps
  its qualified form alongside `turbo.json:build` for
  disambiguation.

## [0.6.0] - 2026-05-05

### Added

- npm distribution: install prebuilt binaries via `npm install -g
  runner-run` (or `pnpm`/`yarn`/`bun`). The façade package
  (`runner-run`) declares one `@runner-run/<platform>-<arch>[-<libc>]`
  package per supported target in `optionalDependencies`; npm/pnpm/yarn
  filter at install time using each sub-package's `os` / `cpu` /
  `libc` fields, so only the matching binary is fetched. No
  `postinstall` script and no network access during install. Façade
  shims (`bin/runner.cjs`, `bin/run.cjs`) resolve the platform
  sub-package via `require.resolve()` at runtime through
  `lib/resolve.cjs` and `lib/launch.cjs`, with helpful diagnostics when
  no matching sub-package is installed.
- Release matrix expanded from Linux musl x86_64/aarch64 to 13 targets
  across Linux (gnu/musl × x64/arm64 + armv7), macOS (x64, arm64),
  Windows (x64, arm64, ia32), FreeBSD (x64, arm64), and NetBSD x64.
  Tier-3 BSD targets are marked `experimental: true` and do not block
  the release. Per-target runner / build-tool selection is data-driven
  from `npm/targets.json` (validated by `npm/targets.schema.json`).
- New `npm-release` workflow downloads the GitHub Release tarballs,
  verifies SHA-256 checksums, generates per-platform packages from
  `npm/targets.json`, and publishes to npm with provenance, optional
  dry-run, and configurable dist-tag.
- `build.rs` build script reads `[[package.metadata.authors]]` from
  `Cargo.toml` and exposes the primary author as compile-time env vars
  `RUNNER_AUTHOR_NAME` (always) and `RUNNER_AUTHOR_EMAIL` (when set),
  consumed by the help byline via `env!` / `option_env!`.
- Pin Rust toolchain via `rust-toolchain.toml` (channel `1.95`,
  components `rustfmt` / `clippy` / `rust-analyzer`, profile
  `minimal`).
- Add `justfile` with developer recipes for building both bins,
  generating per-target npm packages (`build-packages`), and
  end-to-end-testing the façade resolution against the host triple
  (`test-release`).
- README documents the npm install path and the façade pattern
  (per-platform sub-package via `optionalDependencies`, install-time
  filtering, no postinstall, no network).
- `.github/scripts/build/` helpers: `build-npm-packages.sh`,
  `derive-dist-dry.sh`, `download-release-archives.sh`,
  `verify-checksum.sh`, plus `.github/scripts/publish/npm.sh` for the
  publish path.

### Changed

- Move authors metadata from `package.authors` to a structured
  `[[package.metadata.authors]]` table (with `name` / `email` fields)
  consumed by the new build script; `src/lib.rs` drops the runtime
  `primary_author` / `authors` regex-style parser in favour of
  `env!("RUNNER_AUTHOR_NAME")` / `option_env!("RUNNER_AUTHOR_EMAIL")`.
  `help_byline(stdout_is_terminal: bool) -> String` replaces the prior
  `Option<String>`-returning helper and is now part of the public API
  along with `requests_version`.
- **Breaking (Cargo features):** rename feature `run-alias` → `run`;
  the `run` binary's `required-features` follows. Builds passing
  `--features run-alias` no longer enable the alias.
- Bump MSRV from `1.88` to `1.95` (matches the pinned toolchain).
- Add `build = "build.rs"` and a `[package.metadata.npm]` block
  (`name`, `subpkgscope`, `bugs`, `repository`, `engines`) consumed by
  the npm build pipeline so target naming / scope live in one place.
- Promote `colored`, `json5`, `serde_json`, `shlex`, and `yaml-rust2`
  into a single `[dependencies]` table; add `serde` + `toml` as
  `[build-dependencies]` for the build script.
- Migrate npm build/publish scripts from `.mjs` to TypeScript
  (`npm/scripts/build-packages.ts`, `npm/scripts/publish.ts`); add
  `tsconfig.json`. Build-script type narrowing (`narrowRepository` /
  `narrowBugs` / `narrowAuthor`) now throws on wrong-typed optional
  fields instead of silently dropping them, so Cargo metadata drift
  fails the build instead of shipping a façade with missing fields.
- Release workflow archive layout updated to package per-target
  artifacts consumable by the npm build, and adds an experimental flag
  so tier-3 BSD targets do not block a release.
- Editor / formatting config: `.dprint.json` plugin update,
  `.gitattributes` added for line-ending consistency, `.gitignore`
  ignores generated npm artefacts, `.zed/settings.json` checked in.

### Fixed

- Help-byline rendering no longer depends on parsing
  `clap::crate_authors!()` at runtime; the email-aware OSC-8 hyperlink
  path is driven by `option_env!("RUNNER_AUTHOR_EMAIL")` set at compile
  time, removing the runtime string-split fallback.

### Security

- Harden the npm release pipeline with five fail-loud input-validation
  guards (commit `b404098`):
  - `derive-dist-dry.sh` validates `INPUT_DIST_TAG` against
    `^[A-Za-z][A-Za-z0-9._-]*$` so a malformed override cannot smuggle
    flag-like or whitespace values into `npm publish --tag`, and
    normalises `INPUT_DRY_RUN` to strict `true` / `false` (previously
    `True` / `1` / `yes` fell through as `dry_run=false`, i.e. a real
    publish disguised as a dry run).
  - `release.yml` smoke-tests the packaged `linux-x64-gnu` binary with
    `--version` before uploading the `npm/dist` artefact, catching a
    broken bin at build time instead of as `ENOENT` post-publish.
  - `npm.sh` validates `optionalDependencies` in `publish_allowed`:
    the façade must list every required platform under the scope at
    exactly `EXPECTED_VERSION`, and platform sub-packages must declare
    none — closing a vector where a tampered platform package could
    smuggle attacker-controlled transitive deps.
  - `npm view` and `npm publish` are wrapped in `timeout 120s` with
    explicit `124` handling so a hung registry cannot burn the full
    job budget.

## [0.5.0] - 2026-04-21

### Added

- Detect justfile aliases (`alias b := build`) and surface them as
  first-class tasks in `runner list` and `runner <alias>`. Private
  aliases (prefixed `_`, tagged `[private]`, or pointing at a private
  recipe) are hidden. Works via `just --dump-format json` when `just`
  is on PATH and via the regex fallback parser otherwise.
- Render justfile aliases distinctly from recipes in `runner list` and
  shell tab completions: aliases appear under a dedicated
  `justfile (aliases)` group with `name → target` annotations instead
  of duplicating the target recipe's description.

## [0.4.1] - 2026-04-21

### Fixed

- Stop zsh completion from leaking unmatched glob patterns into the
  user's prompt. The completer function now scopes `NULL_GLOB` via
  `emulate -L zsh -o NULL_GLOB`, so globs evaluated by `_files`
  internals or user zstyles (e.g. specs tagged `globbed-files`)
  silently drop when they match nothing — fixing both the
  `no matches found: *:globbed-files` error under the default
  `NOMATCH`, and the subsequent `*(/)` / `*(-/)` residue that would
  otherwise appear on the command line under `NO_NOMATCH` when
  completing a directory-typed flag in a directory with no subdirs.
- Actually stop the `*(-/)` / `*(/)` glob-pattern residue that `--dir
  <TAB>` in an empty directory would type into the prompt. The
  initial switch to `NULL_GLOB` was the right idea but was defeated
  by a function-scoped `setopt noglob` sitting on top of the `_files`
  call: that option disabled globbing inside `_path_files` as well,
  so its internal `tmp1=( $~tmp1 )` never expanded `*(-/)` and the
  literal pattern was handed to `compadd` as a candidate. Replace
  the option with a `noglob` *precommand modifier* on the `_files`
  call so globs like `*(*)` still reach `_files` unexpanded while
  its internals run with `NULL_GLOB` semantics as intended.
- Also enable `EXTENDED_GLOB` in the completer's `emulate -L zsh`
  scope. zsh's own `_files` builds qualifier patterns like `*(#q-/)`
  and uses `(#b)` backreferences internally; `emulate -L zsh` resets
  to plain-zsh defaults (extended glob off), so `_files -/` would
  emit `bad pattern: *(#q-/):globbed-files` on every TAB even once
  the residue bug above was fixed.

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

[Unreleased]: https://github.com/kjanat/runner/compare/v0.12.0...HEAD
[0.12.0]: https://github.com/kjanat/runner/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/kjanat/runner/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/kjanat/runner/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/kjanat/runner/compare/v0.8.1...v0.9.0
[0.8.1]: https://github.com/kjanat/runner/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/kjanat/runner/compare/v0.7.1...v0.8.0
[0.7.1]: https://github.com/kjanat/runner/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/kjanat/runner/compare/v0.6.1...v0.7.0
[0.6.1]: https://github.com/kjanat/runner/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/kjanat/runner/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/kjanat/runner/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/kjanat/runner/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/kjanat/runner/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/kjanat/runner/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/kjanat/runner/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/kjanat/runner/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/kjanat/runner/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/kjanat/runner/releases/tag/v0.1.0

<!-- markdownlint-disable-file no-duplicate-heading MD034 -->
<!-- rumdl-disable-file MD013 MD024 MD034 -->
