# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog], and this project adheres to [Semantic Versioning].

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
[Semantic Versioning]: https://semver.org/spec/v2.0.0.html

## [Unreleased]

### Post-release checklist

- [ ] Move completed `Unreleased` items into a new version section.
- [ ] Update the `[Unreleased]` compare link to the new tag.
- [ ] Create and push a signed `vX.Y.Z` tag from `master`.

### Added

- Chain mode now reports per-task wall-clock duration on completion. Sequential
  and live (`-p`) parallel runs print a concise `· <task> finished in 1.2s
  (exit 0)` line to stderr after each task; grouped parallel output folds the
  same summary into each task's block footer (inside the GitHub Actions
  `::group::` so it stays attached). Durations format compactly (`342ms`,
  `1.2s`, `1m 04s`); the band is chosen from the rounded value, so a duration
  that rounds up to a full minute (e.g. `59.95s`) prints `1m 00s`, never an
  out-of-band `60.0s`. The synthetic install head of an `install` chain is
  timed the same way in both `-s` and `-p` modes. Timing is diagnostic
  meta-output, so `--quiet` (`RUNNER_QUIET`) and `--no-warnings`
  (`RUNNER_NO_WARNINGS`) suppress it.
- `runner install -p <TASK> <TASK>` runs the post-install tasks in parallel
  (`-s` stays the default sequential). Install always runs first as the
  prerequisite — never as a parallel sibling — then the tasks fan out. A
  failed install still aborts the tasks unless `-k`; `-K` (kill siblings) now
  bites for the parallel post-install phase.

### Fixed

- GitHub Actions log groups no longer nest when one `runner`/`run` invokes
  another (e.g. `runner` → an `npm`/`postinstall` script → `run -p A B C`). A
  parent that opens a group marks its descendants (`RUNNER_GROUP_ACTIVE`), so
  a nested runner detects the open group and stays silent instead of emitting
  a second `::group::` that would close the parent's fold early. Inherited
  through intermediate processes, so the whole chain collapses to one group.
- Under GitHub Actions, a child command that emits its own `::group::` /
  `::endgroup::` workflow commands (e.g. some test runners) no longer corrupts
  runner's grouped (`-p`) output: during grouped replay the group title is
  surfaced as plain text and the stray `::endgroup::` is dropped, while
  `::warning::`/`::error::`/`::notice::` annotations pass through untouched.
- A leading `~`/`~/` in `--dir` (or `RUNNER_DIR`) is now expanded to the
  user's home directory before the project directory is resolved. Shells
  only expand an unquoted tilde at the start of a word, so `--dir=~/foo`
  reached `runner` verbatim and was treated as relative — joined onto the
  cwd to produce a bogus `<cwd>/~/foo` that never exists. Unsupported forms
  such as `~user` are left untouched, and the path passes through unchanged
  when no home directory is set.

## [0.14.3] - 2026-06-26

### Added

- `[install].pms` config + `RUNNER_INSTALL_PMS` env restrict which detected
  package managers `runner install` runs. In a polyglot repo where, e.g.,
  both `bun` and `deno` would write `node_modules`, `pms = ["bun"]` keeps
  install to one. A listed-but-undetected PM errors. `--pm`/`RUNNER_PM` still
  takes precedence; `[pm]` continues to scope only script dispatch.
- `runner install` and `doctor` now warn when two detected package managers
  would install into the same directory — today `node_modules` (a node PM
  plus a `nodeModulesDir`-enabled Deno). The warning points at `[install].pms`
  and is suppressed once the allowlist narrows install to a single writer.

### Changed

- Published `@runner-run/*` platform packages now carry `keywords`, a
  descriptive `description`, and a full README (instead of a thin stub).
  These are the binary sub-packages npm selects via `optionalDependencies`;
  the richer metadata raises their Socket.dev Quality score and explains the
  facade-resolution mechanism to anyone landing on them directly.

### Fixed

- `runner.toml` parsing is now forward-compatible: an unrecognized section or
  field (a typo, or a key written by a newer `runner`) is ignored with a
  warning instead of aborting the command. Previously an unknown key was a
  hard parse error, so a config written by one version could brick task
  dispatch — including postinstall `run` hooks — under another. Genuine
  errors (unreadable file, malformed TOML, wrong type on a known field) still
  fail. The JSON Schema stays strict (`additionalProperties: false`) so
  editors keep flagging typos inline.

## [0.14.2] - 2026-06-25

### Added

- `runner config` subcommand to manage `runner.toml`: `init` scaffolds a
  fully-commented starter file (`--force` to overwrite), `show` prints the
  effective config (`--json` for machine output), `validate` parses and
  checks it (exit 2 on error), and `path` prints the resolved file path. The
  scaffold's line 1 is a `#:schema` directive pointing at the committed JSON
  Schema, so tombi/taplo give autocompletion in any project with no setup.
- `runner.toml` is now documented in the README with a `## Configuration`
  section covering every section and the override precedence chain.

### Changed

- `config validate` rejects a `[chain]` that sets both `keep_going` and
  `kill_on_fail` true — the resolver already errored on this combination at
  dispatch time; validation now catches it statically against the file
  alone.
- JSON Schema URLs rehosted from `https://kjanat.github.io/schemas/…` to
  `https://kjanat.github.io/runner/schemas/…`. Changes the `$id` of every
  committed schema and the `$schema` field emitted by `doctor`/`list`/`why`
  `--json`. The base is now sourced from `[package.metadata].schema-base`.

## [0.14.1] - 2026-06-25

### Added

- The rendered API docs (rustdoc / docs.rs) now display the project logo
  and favicon, set via `#![doc(html_logo_url, html_favicon_url)]` pinned to
  `branding/icon.svg`. See https://github.com/kjanat/runner/pull/59
- `-q`/`--quiet` (and a truthy `RUNNER_QUIET`) suppress the `→` dispatch
  line on stderr plus the dispatch-time `--explain` trace, for clean output
  when `runner` wraps another command. See
  https://github.com/kjanat/runner/pull/56
- Short flags `-K` (`--kill-on-fail`) and `-f` (install `--frozen`), plus a
  stable `--help` flag ordering via display-order bands.

### Changed

- `--help` polish: colorize inline flag tokens instead of rendering literal
  backticks, hide the `bundle`/`go-task` aliases from the `--pm`/`--runner`
  lists, terser `help`/`schema`/quiet descriptions, and reorder commands so
  `list` sits with `run` and `why` precedes `doctor`.

### Fixed

- `cargo doc` (and the docs.rs build) no longer fail under the crate's
  `deny`-level rustdoc lints: broken intra-doc links (`cmd::run`,
  `argv[0]`) are repaired. See https://github.com/kjanat/runner/pull/59
- mise tasks with a whitespace-only `description` now fall back to the run
  command instead of rendering a blank description.

## [0.14.0] - 2026-06-22

### Changed

- Built-in verb dispatch is split between the two surfaces. The explicit
  `runner <verb>` subcommand — `install`, `clean`, `list`, `info`,
  `completions` — is now **always** the built-in and is never shadowed by a
  same-named project task. The run path (`run <verb>` / `runner run <verb>`)
  runs a same-named task when one exists, and otherwise falls back to that
  built-in's default form instead of the package-manager exec path (so
  `run install` with no `install` task installs dependencies rather than
  attempting `bunx install`). Previously the precedence was reversed:
  `runner install` deferred to a task named `install` (e.g. a `Makefile`
  `install` target), which surprised projects whose `install` means "install
  the built artifact" rather than "install dependencies". Reach a same-named
  task with `run install` / `runner run install`; the built-in default for
  `info` on the run path is a plain task list (no deprecation warning, which
  remains specific to the explicit `runner info` subcommand). See
  https://github.com/kjanat/runner/pull/55
- The `run` alias now forwards `--help`/`-h` and `--version`/`-V` to the
  task when they follow a task name: `run <task> --help` reaches the
  task's own help instead of printing `run`'s (previously `run <task> --`
  was required). `run --help`/`--version` with no task — including after
  global flags like `run --pm npm --help` — still print this binary's own
  help/version, and `run <task> -- --help` still forwards literally. The
  `runner run` subcommand is unchanged. Because `-h`/`--help`/`-V`/
  `--version` are no longer clap arguments on the alias, they are
  documented in the help footer rather than the options list.

## [0.13.1] - 2026-06-14

### Added

- `runner doctor --json` schema **v3** (now the default for `doctor`):
  the flat detection dump becomes a structured diagnostic inventory —
  `invocation`/`environment`/`runner` provenance, per-`ecosystems`
  decisions with a `confidence` grade derived from the resolution step
  (override/manifest/lockfile → high, PATH probe → medium, legacy npm
  fallback → low, failure → none), task `sources` as first-class objects,
  `fqn`-keyed `tasks` with effective `resolved` commands, PATH-probed
  `tools`, duplicate-task-name `conflicts` (which task wins, which are
  shadowed, and why), flattened `diagnostics`, and a self-describing
  `resolution` policy block. Implements the former `doctor.v3-draft`
  schema; the real output validates against both the committed
  `doctor.v3.schema.json` and the original draft. Draft shapes nothing
  can emit yet (rich dependency edges, workspace identity, probe errors)
  are deferred, not declared. v1/v2 remain available via
  `--schema-version`; human output is unchanged.
- `runner why --json` schema **v3** (now the default for `why`): the
  report is restructured around `{task, match}` candidate pairs plus a
  `decision` block. Each task carries a stable identity
  (`fqn` = `root:<kind>#<name>`, `provider`, `kind` — cargo aliases are
  now labeled `cargo-alias`), its origin (`source` file,
  `source_pointer` key path), and resolution data (`definition`,
  `resolved` command preview, `cwd`, sibling `aliases`,
  `dependencies`). The `match` half exposes the exact run-time selection
  key (`source_priority`, `depth`, `display_order`, alias-last), and
  `decision.strategy` names the branch taken (`single-candidate`,
  `ranked`, `filtered`, `exec-fallback`). Implements the former
  `why.v3-draft` example, which the real output now reproduces verbatim;
  v1/v2 stay available via `--schema-version`. `list` remains at v2 — its
  v3 draft is still under review, and it rejects `--schema-version 3`
  rather than mislabel output. `schema --all` emits the committed
  `schemas/why.v3.schema.json`, and the example validates against it.
- Both v3 schemas use the `<scope>:<kind>#<name>` fqn form, with `#`
  separating the structured prefix from the verbatim task name so a name
  containing `:` (e.g. an npm script `fmt:update`) stays unambiguous.
- Deno tasks now run without the `deno` binary. A `deno.json` /
  `deno.jsonc` task whose command is a leaf shell command executes
  in-process via the embedded `deno_task_shell` (deno's own cross-platform
  task shell) when `deno` isn't on `PATH`; with `deno` installed it still
  shells out to `deno task` for full fidelity. The `unstable-deno-exec`
  feature flips the default to self-exec-first. Tasks that invoke `deno`
  themselves or declare `dependencies` still need the binary. The shell
  engine lives in a reusable `tool::shell` so other shell-string task
  sources can build on it later.
- Deno task descriptions. The object form
  (`"build": { "command": "…", "description": "…" }`) is now parsed and
  the description surfaces in `runner list` / `why` / `doctor`, alongside
  the existing bare-string form.
- `runner list` and the bare `runner` view now print a duplicate-name
  conflict footer. When two sources define the same task name (e.g. a
  `just` `run` recipe and `cargo run`), it names the source that
  `runner run <name>` actually dispatches and the ones it shadows — using
  the same precedence as dispatch — so a silently shadowed task no longer
  goes unnoticed.

### Changed

- Cargo built-in aliases now fold under their canonical subcommand in
  `runner list` and the bare `runner` view. `b`/`c`/`d`/`t`/`r`/`rm` are
  shown as aliases of the promoted `build`/`check`/`doc`/`test`/`run`/
  `remove` tasks (e.g. `test (t)`) instead of standing alone; both the
  canonical name and the short form still dispatch. Aliases that carry
  extra arguments (`bb`, `cl`, `rq`, …) keep their own rows. Promoting
  `run`/`remove` can collide with a same-named `just`/other task — that
  collision now surfaces in the conflict footer above rather than hiding.
- `runner doctor --json` (v3) now probes package-manager and task-runner
  versions via `<tool> --version` (previously only the Node runtime
  carried a version), reports a per-task `self_executable` flag (true for
  deno tasks runner can run through the embedded shell), and derives the
  Deno tool's `required` from it. Node is included in `ecosystems` /
  `tools` whenever a resolver or task signal implies it, not only when a
  Node package manager was lockfile-detected.
- The committed v3 schemas (`doctor.v3.schema.json`,
  `why.v3.schema.json`) set `additionalProperties: false` throughout, so
  validation catches stray or misspelled fields in real output instead of
  silently accepting them.

## [0.13.0] - 2026-06-12

### Added

- `runner doctor` (and `info --json`) now classify PATH-probe hits that
  are Volta shims and resolve them to the real provisioned binary via
  `volta which`: the `PATH probe` line shows
  `npm=<shim> -> <real bin> (volta)`, or
  `(volta shim, not provisioned)` when Volta fronts a tool it has no
  version of. JSON gains an additive `signals.node.volta_shims` map
  (omitted on hosts without Volta; no schema bump). Display only —
  execution still spawns the shim, which performs Volta's per-project
  version selection.

### Changed

- `runner install` now honors the `--pm`/`RUNNER_PM` override: when set,
  only that package manager installs (previously the override was
  ignored and every detected PM installed — e.g. a project with both
  `bun.lock` and `deno.json` always ran `deno install` too, writing an
  unwanted `deno.lock`). An override naming a PM that detection did not
  find refuses the install with exit code 2. runner.toml
  `[pm].node`/`[pm].python` continue to scope script dispatch only.
- Invalid `--pm`/`RUNNER_PM`/`--runner`/`RUNNER_RUNNER` values now produce
  a readable error: the message names the source that carried the value,
  escapes control characters (no more raw ANSI codes), truncates long
  garbage, and — when the value contains line breaks — hints that it
  looks like captured command output with the correctly quoted PowerShell
  spelling. (An unquoted `$env:RUNNER_PM=deno` executes deno and assigns
  its REPL banner to the variable.)

### Fixed

- `runner doctor` no longer dies when a `RUNNER_*` override variable
  holds an unparseable value — the condition it exists to diagnose. The
  invalid value is ignored for the report and surfaced as an `env:`
  warning (human output and the `warnings` array of `doctor --json`,
  additively — no schema bump). Every other command, and an explicit bad
  `--pm`/`--runner` flag even on doctor, still fails fast.
- Node version constraints are now evaluated with real range semantics
  (via the `semver` crate) instead of a prefix match that treated
  `>=22.22.2` as `=22.22.2`. Operators (`>=`, `>`, `<=`, `<`, `=`),
  caret/tilde ranges, space-separated AND comparators, `||` unions,
  hyphen ranges, and `x` wildcards all match per node-semver rules, so
  `engines.node: ">=22.22.2"` no longer warns on Node 22.22.3 or 25.9.0.
  Bare versions (`.nvmrc` `20.11`) keep the stricter
  prefix-at-segment-boundary behavior; unevaluable inputs (`lts/*`) fall
  back to the previous prefix match.
- Task dispatch now prepends every existing `node_modules/.bin` between
  the project directory and the filesystem root (nearest first) to the
  child's `PATH`, the way `npm run` / `pnpm run` / `bun run` do for
  `package.json` scripts. Tools that runner spawns directly — `turbo`
  for `turbo.json` tasks, and the bare-binary exec fallback — used to
  inherit the shell's `PATH` unchanged, so a devDependency-only `turbo`
  failed with `Error: No such file or directory (os error 2)` unless it
  was also installed globally. On Windows, bare program names are
  additionally re-resolved against those bin dirs with `PATHEXT`, since
  `CreateProcessW` would never find the `.cmd` shims npm and pnpm
  install there. Local bins now shadow global installs for the spawned
  task and everything it launches, matching Node package-manager
  semantics.
- The no-argument project-info banner no longer leaks the Windows `.exe`
  suffix in its title line (e.g. `run.exe 0.12.2`). It now shows the same
  `run` / `runner` identity as `--version`, `--help`, and the `Usage:`
  line. The banner had its own copy of the arg0-parsing helper that
  skipped the `.exe` stripping done everywhere else; it now reuses the
  canonical `bin_name_from_arg0`.
- `runner man` now works on Windows under `--features man` builds. The
  subcommand was gated `not(windows)`, so with `external_subcommand` in
  play it silently degraded to task dispatch (`bun man` → "Script not
  found") instead of rendering. Rendering is pure `clap_mangen` with no
  OS-specific code, so the gate bought nothing and is gone.
- `install.sh` runs under any POSIX `sh`. It carried a `#!/usr/bin/env
  bash` shebang, but `curl … | sh` ignores the shebang, so the bash-only
  `set -o pipefail` aborted on line 2 under dash/busybox — the default
  `/bin/sh` on the `-musl` targets. Rewritten POSIX-clean. It also picks
  the install dir more intelligently now: reuse an already-installed
  runner's directory (verified by its `-V` banner, so a system `run`/
  `runner` is never clobbered), otherwise prefer `~/bin` or
  `~/.local/bin` already on `PATH` (then one that exists), falling back
  to `~/.local/bin`.

## [0.12.2] - 2026-06-10

### Fixed

- `runner completions` now detects PowerShell when `$SHELL` is unset or
  unrecognized by falling back to the presence of `$PSModulePath`, which
  pwsh exports on every platform (it never sets `$SHELL`). A recognized
  `$SHELL` still takes precedence, so a pwsh session launched from bash
  keeps completing for the login shell. Previously bare
  `runner completions` always errored under pwsh.

## [0.12.1] - 2026-06-04

### Added

- `pyproject.toml` `[project.scripts]` entry points (PEP 621 console
  scripts) are now extracted as runnable tasks for Python projects. They
  surface under the `pyproject.toml` source in `runner list` (with the
  entry-point target shown as the description) and dispatch via the
  detected Python package manager's `run` subcommand — `uv run <name>`,
  `poetry run <name>`, or `pipenv run <name>`. Previously a uv/poetry
  project's declared scripts were invisible to `runner`, which detected
  the package manager but listed no tasks.
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
- `persist-credentials: false` added to every `actions/checkout` step
  in `release.yml`, so `GITHUB_TOKEN` is not persisted into git config.
- Release verification no longer saves Rust caches from pull request
  runs, preventing untrusted PRs from persisting cache contents.

### Fixed

- `runner list`, `runner run`, and `runner why` now find
  `pyproject.toml` scripts and Python package-manager signals from
  nested directories (bounded by the containing VCS root), so running
  from `src/` inside a uv/poetry/pipenv project still surfaces and
  dispatches `[project.scripts]` tasks.
- `runner why` now reports Python package-manager resolution for
  `pyproject.toml` tasks, including `--pm` and `[pm].python`
  overrides, matching the actual `runner run` dispatch path.
- `runner list --source` invalid-label help now includes
  `pyproject.toml` in the accepted source list.
- Restore the `multiple_crate_versions` Clippy allow so CI accepts the
  current unavoidable duplicate transitive crate versions while keeping
  the broader `clippy::cargo` deny group enabled.
- Hide the feature-only `runner man` generator from shipped `runner.1`
  output, so installed man pages no longer document an unavailable
  subcommand.

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

[Unreleased]: https://github.com/kjanat/runner/compare/v0.14.3...HEAD
[0.14.3]: https://github.com/kjanat/runner/compare/v0.14.2...v0.14.3
[0.14.2]: https://github.com/kjanat/runner/compare/v0.14.1...v0.14.2
[0.14.1]: https://github.com/kjanat/runner/compare/v0.14.0...v0.14.1
[0.14.0]: https://github.com/kjanat/runner/compare/v0.13.1...v0.14.0
[0.13.1]: https://github.com/kjanat/runner/compare/v0.13.0...v0.13.1
[0.13.0]: https://github.com/kjanat/runner/compare/v0.12.2...v0.13.0
[0.12.2]: https://github.com/kjanat/runner/compare/v0.12.1...v0.12.2
[0.12.1]: https://github.com/kjanat/runner/compare/v0.12.0...v0.12.1
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
