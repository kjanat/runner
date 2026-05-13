# Task Chaining + `runner install <tasks>` — Design Spec

**Status:** draft v1, awaiting user review
**Date:** 2026-05-13
**Scope:** `runner` CLI (Rust). Adds chain-mode dispatch to `run`/`runner run`
and a task-list form to `runner install`. Preserves all existing single-task
behavior.

---

## 1. Context

`runner` is a single-binary universal task dispatcher: it detects the
project's package managers and task sources, resolves a task through the
8-step chain, and invokes the right runner. Today it dispatches **one** task
per invocation. Users have expressed two adjacent needs:

1. **Multi-task chains** — running `lint`, `test`, `build` back-to-back
   (sequential) or in concert (parallel), without dropping to shell `&&` /
   `&` or pulling in a real orchestrator.
2. **Install + chain** — the common CI shape: `install deps → build → test`,
   expressed as one command.

This spec defines a v1 that satisfies both without becoming a build
orchestrator (no DAG, no caching, no watch mode). It also reserves design
space for a later v2 that adds per-task argument forwarding via quoted
bundles.

## 2. CLI surface

### 2.1 New flags on `run` / `runner run`

```text
-s, --sequential        chain mode: run tasks in order, stop on first failure
-p, --parallel          chain mode: run tasks concurrently
-k, --keep-going        run all tasks to completion regardless of failures
    --kill-on-fail      parallel only: SIGTERM running siblings on first failure
```

`-s` and `-p` are mutually exclusive (clap `conflicts_with`). When either is
present, the command enters **chain mode**; otherwise the existing
single-task behavior is unchanged.

`-k` and `--kill-on-fail` are mutually exclusive (clap `conflicts_with`).

In chain mode, **all positionals are task names**. Any positional containing
whitespace or starting with `-` is a hard parse error in v1 (reserved for
the v2 quoted-bundle parser).

### 2.2 New positional list on `runner install`

```text
runner install                              # unchanged: PM install only
runner install foo bar                      # install step → foo → bar  (Sequential)
runner install -k foo bar                   # same, fail-fast disabled
runner install -p foo bar                   # REJECTED: -p invalid on install
runner install -s foo bar                   # accepted but redundant (install is always sequential)
runner install --kill-on-fail foo bar       # accepted but redundant (install is always sequential)
```

Install treats the PM-install step as a synthetic first task in a sequential
chain. Same flag set as `run` chain mode (`-k` honored;
`--kill-on-fail` rejected because install is always sequential).

### 2.3 Examples — backward-compat boundary

| Command                     | Mode          | Behavior                                             |
| --------------------------- | ------------- | ---------------------------------------------------- |
| `run test`                  | single-task   | today's behavior: resolve `test`, dispatch           |
| `run test --watch`          | single-task   | `--watch` forwarded to resolved task                 |
| `run -s build test`         | chain (Seq)   | resolve `build`, run; on success resolve `test`, run |
| `run -p build test`         | chain (Par)   | spawn both, line-prefix output, wait                 |
| `run -s build --watch test` | **error**     | `--watch` is not a task name in chain mode           |
| `run -s -p a b`             | **error**     | mode flags conflict                                  |
| `run -k --kill-on-fail a b` | **error**     | failure flags conflict (clap `conflicts_with`)       |
| `runner install`            | unchanged     | PM install only                                      |
| `runner install build test` | install-chain | install → build → test, sequential                   |

### 2.4 Mutual exclusion summary

| Pair                     | Reason                       |
| ------------------------ | ---------------------------- |
| `-s` vs `-p`             | mode conflict                |
| `-k` vs `--kill-on-fail` | failure-policy conflict      |
| `-p` on `runner install` | install is always sequential |

## 3. Internal model

New module `src/chain/`:

```rust
// src/chain/mod.rs
pub(crate) struct Chain {
    pub mode: ChainMode,
    pub items: Vec<ChainItem>,
    pub failure: FailurePolicy,
}

pub(crate) enum ChainMode {
    Sequential,
    Parallel,
}

pub(crate) struct ChainItem {
    pub kind: ChainItemKind,
    /// Empty in v1. Populated by the v2 quoted-bundle parser.
    pub args: Vec<String>,
}

pub(crate) enum ChainItemKind {
    /// User-supplied task name (resolved per-item via the existing 8-step chain).
    Task(String),
    /// Synthetic head: dispatches the detected PM's install command.
    Install,
}

pub(crate) enum FailurePolicy {
    FailFast,   // default
    KeepGoing,  // -k / --keep-going
    KillOnFail, // --kill-on-fail (parallel only)
}
```

Notes:

- `args: Vec<String>` is reserved storage. The v1 parser always writes
  `vec![]`. The v2 parser splits each positional on whitespace into
  `name + args`. Type shape and downstream executor signatures don't
  change — only the parser is swapped.
- `ChainItemKind::Install` carries no args by design. Install's command line
  is fully determined by the detected PM.

## 4. Execution semantics

### 4.1 Sequential

Iterate `items` in order. For each item:

1. Resolve through the existing 8-step chain (`Task`) or call the existing
   install pipeline (`Install`).
2. Print the banner: `→ <source> <task>` (existing format from
   `cmd::run::dispatch_task`).
3. Spawn with `Stdio::inherit()` — zero output infrastructure, raw
   pass-through.
4. Wait for exit. If non-zero:
   - `FailFast`: abort the rest of the chain. Chain exit = this task's exit code.
   - `KeepGoing`: continue. Remember the first non-zero exit code.

Returns: exit code of the **first** failing task, or 0 if all succeeded.

### 4.2 Parallel

Use `std::thread::scope` (no async runtime — `runner` stays sync). For each
item:

1. Resolve and spawn the child with `Stdio::piped()` on stdout + stderr.
2. Per child, spawn two reader threads (stdout, stderr) that `read_line` and
   forward `(task_name, line, FdKind)` to a shared `mpsc::channel`.
3. The main thread waits on all children.

A single **writer thread** drains the channel and writes each line to the
parent's stdout/stderr with a `[<task>]` prefix (see §5).

Failure handling:

- `FailFast` (default for parallel): on first non-zero exit, **stop
  spawning** (no-op here since all spawned upfront) and let running
  siblings finish. Final chain exit = first failing task's exit code.
- `KeepGoing`: ignore per-task failures during execution. Final chain exit
  = first failing task's exit code (chosen over max for diagnostic clarity).
- `KillOnFail`: on first non-zero exit, send SIGTERM to all sibling child
  processes. Reap them, collect exit codes. Final chain exit = first
  failing task's exit code.

SIGTERM-then-wait pattern (no SIGKILL escalation in v1; tasks should
respond to SIGTERM. If they don't, user can re-invoke without
`--kill-on-fail`).

### 4.3 Resolver warning dedup

Each task resolves independently; warnings (`PmMismatch`,
`PathProbeFallback`, `UnparseablePackageManager`, etc.) would repeat per
task in a chain. Solution:

- Both modes accumulate warnings into a `HashSet<DetectionWarning>` during
  resolution.
- Print the deduped set **once** before the first task runs in sequential
  mode, or before the prefix-muxer starts in parallel mode.
- `--no-warnings` / `RUNNER_NO_WARNINGS` still suppresses entirely.

Verify `DetectionWarning` already implements `Hash` + `Eq`; if not, add the
derives (free — variants are simple).

## 5. Output rendering (parallel mode)

Sequential mode uses `Stdio::inherit()` and needs no muxer. The rest of
this section applies to parallel mode only.

### 5.1 Prefix format

```text
[<task-name-padded>] <line>
```

`<task-name-padded>` is right-padded to the width of the longest task name
in the chain, so the prefixes align visually.

Example (3 tasks: `build`, `test`, `format`):

```text
[build ] compiling crate runner
[test  ] running 47 tests
[format] checking style
[build ] error: expected ';'
[test  ] all tests passed
[format] done
```

### 5.2 Color

Each task name gets a deterministic color from a fixed 8-color ANSI palette:

```rust
fn color_for(name: &str) -> AnsiColor {
    const PALETTE: [AnsiColor; 8] = [
        Cyan,
        Magenta,
        Yellow,
        Green,
        Blue,
        Red,
        BrightCyan,
        BrightMagenta,
    ];
    let hash = name
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    PALETTE[hash as usize % PALETTE.len()]
}
```

Colors are applied to the `[<task>]` bracket prefix only. Line content is
emitted unchanged (tasks' own ANSI sequences pass through).

Honors `NO_COLOR` env var and non-TTY parent (no color in either case;
prefix stays).

### 5.3 Stdio routing

Per task: 2 piped streams. Each stream's reader thread sends
`(task_name, line, FdKind::Stdout | FdKind::Stderr)` to a single `mpsc`
channel. The writer thread routes by `FdKind`:

- `Stdout` lines → parent's `stdout`
- `Stderr` lines → parent's `stderr`

Prefix is added to both. Stderr lines may optionally use a dimmer color
variant (deferred to follow-up; not v1).

### 5.4 EOF handling

Reader threads exit when their `BufRead::read_line` returns `Ok(0)`. When
all readers exit, all senders drop, the channel closes, and the writer
thread exits. The main thread joins the writer last.

### 5.5 Known v1 limitations

- **Long lines without newlines** (e.g., progress bars using `\r`) may not
  get prefixed correctly. v1 splits on `\n`; carriage-return progress
  output bunches into one line. Document as known limitation.
- **Binary output** is read as bytes-via-`read_line`. Non-UTF-8 bytes are
  passed through with the system locale; not actively decoded.
- **No interleave guarantees** at sub-line granularity. Lines are atomic;
  intra-line ordering is undefined (matches every line-prefixing tool).

## 6. `runner install <tasks>` extension

### 6.1 Flow

```text
runner install foo bar
  ↓
Chain {
  mode: Sequential,
  items: [
    ChainItem { kind: Install,          args: [] },
    ChainItem { kind: Task("foo"),      args: [] },
    ChainItem { kind: Task("bar"),      args: [] },
  ],
  failure: FailFast,  // -k overrides to KeepGoing
}
  ↓
Sequential executor:
  - Install step:  invoke `cmd::install::run` (existing) — runs `pnpm install` / `cargo build` / etc.
  - Task foo:      resolve via 8-step chain, dispatch
  - Task bar:      resolve via 8-step chain, dispatch
```

### 6.2 Install always sequential, always rerun

- Install is the first item; nothing parallelizes with it.
- No caching: every invocation runs the PM install fresh. (Most PMs cache
  internally by lockfile hash; runner adds no extra layer.)
- `-s` flag on `runner install` is accepted but redundant.
- `-p` flag on `runner install` is rejected at parse time.
- `--kill-on-fail` flag on `runner install` is accepted but redundant
  (install is always sequential — flag is a no-op).

### 6.3 If install fails

- Default (`FailFast`): chain aborts, exit = install's exit code.
- `-k` / `RUNNER_KEEP_GOING`: chain continues despite install failure.
  Tasks that depend on freshly installed deps may then fail too;
  exit = install's exit code (first failure wins).

## 7. Environment variables

```text
RUNNER_KEEP_GOING       1|true|yes  →  --keep-going
RUNNER_KILL_ON_FAIL     1|true|yes  →  --kill-on-fail
```

No env for `-s`/`-p` mode selection — chain mode is per-invocation by
design (a user setting `RUNNER_CHAIN_MODE=parallel` globally would
silently break every single-task invocation).

Parsing: reuse existing `is_env_truthy` helper from `src/resolver/mod.rs`.

## 8. Schema / `runner.toml` routing

### 8.1 New `[chain]` section

```toml
[chain]
keep_going   = false  # default false; true = run all tasks regardless of failures
kill_on_fail = false  # default false; true = SIGTERM siblings on first failure (parallel only)
```

### 8.2 Rust type

```rust
// src/config.rs

#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub(crate) struct ChainSection {
    /// Failure policy: run all tasks to completion regardless of failures.
    /// Mutually exclusive with [chain].kill_on_fail.
    #[serde(default)]
    pub keep_going: Option<bool>,

    /// Parallel-only: terminate sibling tasks on first failure.
    /// Mutually exclusive with [chain].keep_going.
    #[serde(default)]
    pub kill_on_fail: Option<bool>,
}

// added to RunnerConfig:
#[serde(default)]
pub chain: ChainSection,
```

`Option<bool>` (not `bool`) preserves the user's explicit `false` versus
absence, which matters for env-overrides-config layering: if config says
`keep_going = false` and env says `RUNNER_KEEP_GOING=1`, env wins.

### 8.3 Precedence (high → low)

Mirrors existing `--fallback` / `--on-mismatch` layering via the
`SourceValue<T>` pattern in `OverrideSources` (`src/resolver/mod.rs`):

1. CLI flag (`-k` / `--keep-going` / `--kill-on-fail`)
2. Env var (`RUNNER_KEEP_GOING` / `RUNNER_KILL_ON_FAIL`)
3. `[chain]` section in `runner.toml`
4. Built-in default (both `false`)

### 8.4 Schema regeneration

`schemars 1.2` is already wired up via the `schema-gen` feature plus
`examples/gen-schema.rs` and the `just gen-schema` recipe. Adding
`ChainSection` + `RunnerConfig.chain` automatically extends the schema
on regen.

CI drift gate (`just gen-schema && git diff --exit-code schemas/`) catches
forgotten regenerations.

## 9. Validation rules

Mutual exclusion (`keep_going` vs `kill_on_fail`) is enforced at every
source independently and again after layering:

| Source             | Mechanism                                      | Error type                                                            |
| ------------------ | ---------------------------------------------- | --------------------------------------------------------------------- |
| CLI                | clap `conflicts_with`                          | clap-generated error, exit 2                                          |
| Env                | both vars truthy at startup                    | `ResolveError::ConflictingFailurePolicy`                              |
| Config             | both fields `Some(true)` in `runner.toml`      | TOML parse error (manual `Deserialize` check or post-load validation) |
| Effective resolved | layered values yield `(KeepGoing, KillOnFail)` | `ResolveError::ConflictingFailurePolicy`                              |

`--kill-on-fail` is only meaningful in parallel mode. In any sequential
context — `run -s ...`, `runner install ...`, or a bare `run` with no
mode flag — it's accepted silently and unused. (Decision rationale in
§14.) This keeps the validation surface small: only `-s vs -p` and
`-k vs --kill-on-fail` are real conflicts.

## 10. Forward-compat: quoted bundles (v2)

v1 is designed so v2 — per-task argument forwarding via quoted bundles —
ships as a parser change with zero downstream churn:

| v1                                                            | v2                                                                                 |
| ------------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| `run -s build test`                                           | `run -s "build --release" "test --watch"`                                          |
| Parser: positional → `ChainItem { Task(name), args: vec![] }` | Parser: positional → split on whitespace → `ChainItem { Task(first), args: rest }` |
| Validation: reject positional containing whitespace           | Validation: accept whitespace, split on it                                         |
| Validation: reject positional starting with `-`               | Validation: still reject bare leading `-` (use quotes)                             |
| Executor: passes `args` to dispatch                           | Unchanged — already wired up                                                       |

### v1 guarantees that protect this path

- `ChainItem.args: Vec<String>` exists from day 1.
- The executor signature already accepts `args: &[String]` and forwards to
  the dispatch layer.
- Per-task resolver invocation already takes `extra_args` as a separate
  parameter from the task name.
- The mutual-exclusion / validation logic doesn't depend on positional
  syntax.

### v1 rejections that v2 relaxes

- `run -s "build --release" test` — v1 errors on the whitespace-containing
  positional with a message like:

  ```text
  error: per-task arguments are not supported in this version
    note: positional "build --release" contains whitespace
    note: quoted-bundle syntax is reserved for a future runner release
  ```

  v2 accepts it.

- `run -s build --release test` — v1 errors on the bare `-` positional.
  v2 still errors (this is unambiguous orphan flag syntax; quoted bundles
  are the only sanctioned way to forward per-task args).

## 11. Files touched

### New

- `src/chain/mod.rs` — `Chain`, `ChainMode`, `ChainItem`, `ChainItemKind`,
  `FailurePolicy` types + module entrypoint.
- `src/chain/parse.rs` — positional-list → `Vec<ChainItem>` parser with
  v1 validation rules.
- `src/chain/exec.rs` — sequential + parallel executors.
- `src/chain/mux.rs` — line-prefix multiplexer for parallel output.

### Edited

- `src/cli.rs` — add `-s` / `-p` / `-k` / `--kill-on-fail` to `RunAliasCli`
  and the `Cli::Run` subcommand `GlobalOpts`. Add task-list positional and
  `-k` to `Cli::Install`. Reject `-p` on `Cli::Install` at clap level.
- `src/cmd/run.rs` — branch on chain mode at the top of `run`. Existing
  single-task path becomes one arm; chain path delegates to
  `chain::exec::run_chain`.
- `src/cmd/install.rs` — branch on whether tasks were supplied; if yes,
  build `Chain { mode: Sequential, items: [Install, Task, ...], ... }` and
  delegate.
- `src/config.rs` — add `ChainSection`, attach to `RunnerConfig`.
- `src/resolver/mod.rs` — add `failure_policy: SourceValue<FailurePolicy>`
  to `OverrideSources` and `ResolutionOverrides`. Wire CLI / env / config
  precedence. Validate mutual exclusion.
- `src/resolver/error.rs` — add `ResolveError::ConflictingFailurePolicy`
  variant.
- `src/types.rs` — verify `DetectionWarning` has `Hash + Eq` derives; add
  if missing.
- `schemas/runner.toml.schema.json` — regenerated via `just gen-schema`.

### Maybe touched

- `Cargo.toml` — no new deps anticipated. Color palette uses existing
  `anstyle` (or `colored` if already in use); confirm during
  implementation.

## 12. Testing

### Unit tests

- `chain::parse`:
  - rejects positional with whitespace (v1 reserved)
  - rejects positional starting with `-`
  - accepts ≥1 task name
  - chain mode requires at least 1 task
- `chain::exec` sequential:
  - all-pass → exit 0
  - middle failure with `FailFast` → exit = failing task's code, later tasks not run
  - middle failure with `KeepGoing` → all tasks run, exit = first failure's code
- `chain::exec` parallel:
  - all-pass → exit 0
  - one failure with `FailFast` → other tasks finish, exit = failing task's code
  - one failure with `KillOnFail` → SIGTERM sent to siblings, exit = failing task's code
  - one failure with `KeepGoing` → all complete, exit = first failure's code
- `chain::mux`:
  - lines from concurrent tasks get correct prefix
  - prefix padding matches longest task name
  - `NO_COLOR` strips color from prefix
  - stderr lines route to parent stderr
- `resolver::overrides`:
  - CLI `--keep-going` wins over env
  - env wins over `runner.toml [chain]`
  - both `keep_going` and `kill_on_fail` set in any single source → error
- `config`:
  - `[chain] keep_going = true, kill_on_fail = true` → load error

### Integration

- `tests/fixtures/chain-sequential/`: 3 tasks, run `-s a b c`, assert order
  and exit code.
- `tests/fixtures/chain-parallel-failfast/`: 3 tasks, middle one fails with
  exit 7, run `-p`, assert sibling completion + exit 7.
- `tests/fixtures/chain-parallel-killonfail/`: spawn long-runners + fast
  failer, run `-p --kill-on-fail`, assert siblings receive SIGTERM.
- `tests/fixtures/install-then-tasks/`: `runner install foo bar` in a fresh
  workspace; assert install ran and tasks dispatched.
- `tests/fixtures/install-fail-tasks-skipped/`: install fails (bogus
  manifest); assert tasks not run.
- `tests/fixtures/install-fail-keep-going/`: same + `-k`; assert tasks
  attempted after install failure.

### Smoke

- `runner run -s build test` in this repo (it has a `Cargo.toml` and a
  `justfile`) → both tasks dispatched in order.
- `RUNNER_KEEP_GOING=1 runner install nonexistent-task` → install runs,
  bogus task fails clean.
- `NO_COLOR=1 runner run -p a b c` → no ANSI in prefixes.

### Regression gates

- `cargo test --workspace` — all existing tests pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — no new lints.
- `just gen-schema && git diff --exit-code schemas/` — schema in sync.
- `rg "#\[allow\(dead_code"` — still zero matches.

## 13. Out of scope for v1

- **Per-task args / quoted bundles.** Reserved; v2 parser swap (see §10).
- **DAG of tasks** with dependencies. `runner` is a dispatcher, not Turbo.
- **Output caching / `--since` / incremental.** Out of scope.
- **Watch mode in chains.** Out of scope.
- **TTY allocation per task** for `-p`. v1 pipes are non-TTY; tasks that
  detect TTY may behave differently. Document.
- **Carriage-return progress output.** v1 muxer is line-based; tasks that
  emit `\r` progress bars will bunch output. Follow-up.
- **`--max-parallel N`.** v1 spawns all parallel tasks at once. Follow-up.
- **Custom prefix format / color palette.** Hard-coded in v1.
- **Chains in `runner why` / `runner doctor`.** Both stay single-task-aware;
  if user wants chain explanation, they get one resolution trace per task
  via repeat invocations.

## 14. Open / deferred questions

- **Should sequential mode also accept `--kill-on-fail`?** Currently
  accepted silently as no-op. Could reject for cleanliness, at the cost of
  one more validation rule. **Decision: accept silently in v1** — fewer
  error paths.
- **Color palette size 8 vs 16.** 8 is enough for typical 3–5 task chains;
  16 supports larger but adds dim variants. **Decision: 8 for v1**, expand
  if users actually chain large counts.
- **SIGKILL escalation** if a child ignores SIGTERM with `--kill-on-fail`.
  v1 sends SIGTERM and waits indefinitely; if a task hangs, user must
  Ctrl-C. Follow-up if it becomes a pain point.
