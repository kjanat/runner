# Task Chaining + `runner install <tasks>` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add chain-mode dispatch to `run` / `runner run` (`-s` / `-p` flags accept N task names) and a task-list form to `runner install` (PM install becomes the first sequential task), preserving every existing single-task code path.

**Architecture:** New `src/chain/` module owns the `Chain` type, the sequential + parallel executor, and the parallel line-prefix multiplexer. `cmd::run` and `cmd::install` branch into it when chain mode is requested. Failure-policy precedence (CLI > env > `[chain]` config > default) reuses the existing `SourceValue` / `resolve_*_policy` pattern from `--fallback` / `--on-mismatch`. `ChainItem.args: Vec<String>` is reserved storage that v1 always populates with `vec![]`; v2 quoted-bundle parser will populate it without touching downstream code.

**Tech Stack:** Rust edition 2024 (current crate is `runner-run`), `clap 4` derive macros (`Args` / `Subcommand` / `Parser`), `serde` + `toml` for config, `schemars 1.2` (behind `schema-gen` feature) for JSON schema, `std::thread::scope` + `std::sync::mpsc` for parallel execution (no async runtime), `colored` crate (already a dep) for ANSI prefixes.

**Reference spec:** `docs/superpowers/specs/2026-05-13-task-chaining-design.md`. Read it first — every task below cites a section.

---

## File Structure

### New files

| Path                                                 | Responsibility                                                                                                                                                               |
| ---------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `src/chain/mod.rs`                                   | Module root; declares `Chain`, `ChainMode`, `ChainItem`, `ChainItemKind`, `FailurePolicy` types and `pub(crate) use` re-exports.                                             |
| `src/chain/parse.rs`                                 | Positional list → `Vec<ChainItem>` parser. Owns v1 validation rules (reject whitespace / leading `-`).                                                                       |
| `src/chain/exec.rs`                                  | `run_chain(ctx, overrides, chain) -> Result<i32>`. Sequential path uses `Stdio::inherit()`; parallel path uses piped stdio + the muxer. Failure-policy branching lives here. |
| `src/chain/mux.rs`                                   | Line-prefix multiplexer for parallel mode. Per-task reader threads → single mpsc channel → writer thread that prefixes `[<name-padded>]` and writes to parent stdout/stderr. |
| `docs/superpowers/plans/2026-05-13-task-chaining.md` | This file.                                                                                                                                                                   |

### Edited files

| Path                              | Change                                                                                                                                                                                                                                                                                                                 |
| --------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `src/cli.rs`                      | Add `-s`/`-p`/`-k`/`--kill-on-fail` to both `Cli::Run` and `RunAliasCli`. Extend `Cli::Install` with task-list positional + `-k`. Reject `-p` / `--kill-on-fail` on install via clap `conflicts_with`.                                                                                                                 |
| `src/cmd/run.rs`                  | At the top of `pub(crate) fn run`, branch on chain-mode presence. Existing single-task body becomes one arm; chain arm calls `crate::chain::exec::run_chain`.                                                                                                                                                          |
| `src/cmd/install.rs`              | Split `install()` body so the install-only path stays a one-liner over `install_pms(ctx, frozen) -> Result<i32>`; new entry `install_with_tasks(ctx, overrides, frozen, tasks, failure)` builds and runs a chain.                                                                                                      |
| `src/config.rs`                   | Add `ChainSection` (with `keep_going` / `kill_on_fail` `Option<bool>` fields) to `RunnerConfig`.                                                                                                                                                                                                                       |
| `src/resolver/mod.rs`             | Add `failure_policy: FailurePolicy` to `ResolutionOverrides`. Add `keep_going` / `kill_on_fail` `ExplainSource`s to `OverrideSources`. Wire `from_cli_and_env` and `from_sources` to consult CLI / env / `[chain]` config / default in that order. Validate mutual exclusion at every source and again after layering. |
| `src/resolver/error.rs`           | Add `ResolveError::ConflictingFailurePolicy { source: &'static str }` variant. Extend `Display` impl.                                                                                                                                                                                                                  |
| `src/types.rs`                    | Add `Hash, Eq, PartialEq` derives to `DetectionWarning` so chain warning-dedup can hash into a `HashSet`.                                                                                                                                                                                                              |
| `schemas/runner.toml.schema.json` | Regenerated via `just gen-schema` after `ChainSection` lands.                                                                                                                                                                                                                                                          |

### Why this split

- `chain::parse`, `chain::exec`, `chain::mux` each have one responsibility and a clean interface. The muxer in particular is self-contained (it knows nothing about resolution or tasks) and is easy to unit-test in isolation.
- `cmd::run` and `cmd::install` only learn the chain-mode entry condition; the existing single-task / single-PM code paths are untouched.
- Resolver plumbing follows the existing `--fallback` / `--on-mismatch` pattern (`SourceValue` + `resolve_*_policy(cli, env, config)`); new fields slot in alongside the old ones.
- `DetectionWarning` derive additions are zero-cost: all variant payloads (`PackageManager`, `Ecosystem`, `String`, `&'static str`, `Vec<PackageManager>`) already implement `Hash + Eq`.

---

## Spec coverage map

Quick check that every section of the spec has a home in this plan:

| Spec §                               | Plan task(s)       |
| ------------------------------------ | ------------------ |
| §2.1 `run` chain flags               | Task 6             |
| §2.2 `runner install <tasks>` flags  | Task 7             |
| §2.3–2.4 examples + mutual exclusion | Tasks 6, 7, 11, 13 |
| §3 internal model                    | Task 2             |
| §4.1 sequential exec                 | Task 10            |
| §4.2 parallel exec + failure modes   | Task 11            |
| §4.3 warning dedup                   | Task 14            |
| §5 parallel output rendering         | Task 9             |
| §6 install-with-tasks flow           | Task 13            |
| §7 environment variables             | Task 4             |
| §8 schema/config routing             | Tasks 3, 4, 16     |
| §9 validation rules                  | Tasks 4, 5, 6, 7   |
| §10 forward-compat                   | Task 2, Task 8     |
| §11 files touched                    | covered by tasks   |
| §12 testing                          | Tasks 14 + 15      |

---

## Task 1: Add `Hash + Eq + PartialEq` derives to `DetectionWarning`

Required so chain warning-dedup can hash variants into a `HashSet`. All variant payload types (`PackageManager`, `Ecosystem`, `String`, `&'static str`, `Vec<PackageManager>`) already derive `Hash + Eq` (see `src/types.rs:11`, `47`).

**Files:**

- Modify: `src/types.rs:159` (the `DetectionWarning` enum declaration)
- Test: `src/types.rs` (`#[cfg(test)] mod tests` block at the bottom, if absent add one)

- [ ] **Step 1: Write the failing test**

Append to the test module in `src/types.rs`:

```rust
#[test]
fn detection_warning_can_be_hashed() {
    use std::collections::HashSet;

    let a = DetectionWarning::DevEnginesBinaryMissing {
        pm: PackageManager::Pnpm,
    };
    let b = DetectionWarning::DevEnginesBinaryMissing {
        pm: PackageManager::Pnpm,
    };
    let c = DetectionWarning::DevEnginesBinaryMissing {
        pm: PackageManager::Yarn,
    };

    let mut set = HashSet::new();
    set.insert(a);
    set.insert(b);
    set.insert(c);

    assert_eq!(set.len(), 2, "equal variants should dedup");
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test detection_warning_can_be_hashed
```

Expected: compile error — `the trait bound DetectionWarning: Hash is not satisfied`.

- [ ] **Step 3: Add the derives**

Edit `src/types.rs:159` from:

```rust
#[derive(Debug, Clone)]
pub(crate) enum DetectionWarning {
```

to:

```rust
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) enum DetectionWarning {
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test detection_warning_can_be_hashed
```

Expected: PASS. Also run `cargo test --lib types` to confirm nothing else broke.

- [ ] **Step 5: Commit**

```bash
git add src/types.rs
git commit -m "$(cat <<'EOF'
derive Hash + Eq on DetectionWarning for chain dedup

The chain executor needs to dedup resolver warnings across N
per-task resolutions before emitting them. All variant payload
types already derive Hash + Eq, so the extra derives are free.
EOF
)"
```

---

## Task 2: Chain core types in `src/chain/mod.rs`

Implements §3 of the spec. Pure data — no execution logic. `ChainItem.args` is the forward-compat slot for v2 quoted bundles (§10).

**Files:**

- Create: `src/chain/mod.rs`
- Modify: `src/lib.rs` (add `pub(crate) mod chain;` near the other module declarations)
- Test: `src/chain/mod.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Wire the module into the crate**

Add to `src/lib.rs`, alphabetically near the other module declarations (next to `mod cli;`):

```rust
pub(crate) mod chain;
```

- [ ] **Step 2: Write the failing test**

Create `src/chain/mod.rs` with the test scaffolding:

```rust
//! Chain types and execution. v1 supports sequential + parallel chains
//! of task names plus the synthetic `Install` head used by
//! `runner install <tasks>`. v2 (out of scope here) will populate
//! `ChainItem.args` from a quoted-bundle parser.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_item_carries_empty_args_in_v1() {
        let item = ChainItem::task("build");
        assert_eq!(item.args.len(), 0);
        assert!(matches!(item.kind, ChainItemKind::Task(ref n) if n == "build"));
    }

    #[test]
    fn install_head_has_no_args() {
        let item = ChainItem::install();
        assert!(item.args.is_empty());
        assert!(matches!(item.kind, ChainItemKind::Install));
    }

    #[test]
    fn failure_policy_default_is_fail_fast() {
        assert_eq!(FailurePolicy::default(), FailurePolicy::FailFast);
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

```bash
cargo test --lib chain::tests
```

Expected: compile error — types not defined.

- [ ] **Step 4: Add the types**

Replace the body of `src/chain/mod.rs` (keeping the file docstring and tests):

```rust
//! Chain types and execution. v1 supports sequential + parallel chains
//! of task names plus the synthetic `Install` head used by
//! `runner install <tasks>`. v2 (out of scope here) will populate
//! `ChainItem.args` from a quoted-bundle parser.

pub(crate) mod exec;
pub(crate) mod mux;
pub(crate) mod parse;

/// A user-requested chain of tasks plus the policy that governs how
/// the chain reacts to per-task failures.
#[derive(Debug, Clone)]
pub(crate) struct Chain {
    pub mode: ChainMode,
    pub items: Vec<ChainItem>,
    pub failure: FailurePolicy,
}

/// Execution mode for the chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChainMode {
    Sequential,
    Parallel,
}

/// A single entry in a chain. v1 always sets `args` to an empty vec;
/// v2 quoted-bundle support will populate it from the parser.
#[derive(Debug, Clone)]
pub(crate) struct ChainItem {
    pub kind: ChainItemKind,
    pub args: Vec<String>,
}

impl ChainItem {
    /// Construct a chain item that dispatches the user-supplied task name.
    pub(crate) fn task(name: impl Into<String>) -> Self {
        Self {
            kind: ChainItemKind::Task(name.into()),
            args: Vec::new(),
        }
    }

    /// Construct the synthetic install-head used by `runner install <tasks>`.
    pub(crate) fn install() -> Self {
        Self {
            kind: ChainItemKind::Install,
            args: Vec::new(),
        }
    }

    /// Human-readable label for prefix-muxer output and error messages.
    pub(crate) fn display_name(&self) -> &str {
        match &self.kind {
            ChainItemKind::Task(name) => name.as_str(),
            ChainItemKind::Install => "install",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ChainItemKind {
    /// User-supplied task name, resolved per-item via the existing 8-step chain.
    Task(String),
    /// Synthetic head used by `runner install <tasks>`. Dispatches the detected
    /// PM's install command.
    Install,
}

/// Failure policy for a chain. `FailFast` is the default and matches
/// `make -j` semantics in parallel mode (let running siblings finish,
/// don't start new ones).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum FailurePolicy {
    /// Stop the chain on the first failing task. In parallel mode,
    /// already-running siblings complete naturally.
    #[default]
    FailFast,
    /// Run every task to completion regardless of failures. Final exit
    /// code reflects the first failure.
    KeepGoing,
    /// Parallel only: SIGTERM siblings on first failure. Sequential
    /// callers accept this silently (no-op).
    KillOnFail,
}
```

Update the existing test stub to also exercise `display_name`:

```rust
#[test]
fn display_name_is_install_for_install_head() {
    assert_eq!(ChainItem::install().display_name(), "install");
}
```

- [ ] **Step 5: Run tests**

```bash
cargo test --lib chain::tests
```

Expected: all four tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/lib.rs src/chain/mod.rs
git commit -m "$(cat <<'EOF'
add chain core types

`Chain` / `ChainMode` / `ChainItem` / `FailurePolicy` types for the
new `run -s/-p` mode + `runner install <tasks>` form. v1 leaves
`ChainItem.args` empty; v2 quoted-bundle parser will populate it
without touching executor or muxer signatures.

Stub `chain::{exec,mux,parse}` submodules wired up but unimplemented;
follow-up tasks fill them in.
EOF
)"
```

Note: `cargo test` will report unresolved imports for the stub submodules referenced in the `pub(crate) mod` lines. That's expected — those submodules ship in Tasks 8–11. If you want a green build between tasks, replace the `pub(crate) mod` lines with `// pub(crate) mod exec; etc.` placeholders and uncomment as each lands.

---

## Task 3: Add `[chain]` section to `RunnerConfig`

Implements spec §8. Schema regeneration is deferred to Task 16 to avoid churning the committed schema between intermediate commits.

**Files:**

- Modify: `src/config.rs:53` (the `RunnerConfig` struct) and below (add `ChainSection`)
- Test: `src/config.rs` (the existing `#[cfg(test)] mod tests` block)

- [ ] **Step 1: Write the failing test**

Add to `src/config.rs`'s test module:

```rust
#[test]
fn load_parses_chain_section() {
    let dir = TempDir::new("config-chain");
    fs::write(
        dir.path().join(CONFIG_FILENAME),
        "[chain]\nkeep_going = true\nkill_on_fail = false\n",
    )
    .expect("config should be written");

    let loaded = load(dir.path())
        .expect("config should parse")
        .expect("config should be present");

    assert_eq!(loaded.config.chain.keep_going, Some(true));
    assert_eq!(loaded.config.chain.kill_on_fail, Some(false));
}

#[test]
fn load_rejects_unknown_chain_key() {
    let dir = TempDir::new("config-unknown-chain-key");
    fs::write(dir.path().join(CONFIG_FILENAME), "[chain]\nfast = true\n")
        .expect("config should be written");

    let err = load(dir.path()).expect_err("unknown [chain] key should error");
    let msg = format!("{err:#}");
    assert!(msg.contains("failed to parse"));
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --lib config::tests::load_parses_chain_section config::tests::load_rejects_unknown_chain_key
```

Expected: compile error — `no field 'chain' on RunnerConfig`.

- [ ] **Step 3: Add `ChainSection` and attach it to `RunnerConfig`**

Insert a new section definition in `src/config.rs` just below `ResolutionSection` (around line 123):

```rust
/// `[chain]` section — failure policy for `run -s/-p` chains and
/// `runner install <tasks>`.
///
/// `Option<bool>` rather than `bool` so the resolver can distinguish
/// "user explicitly set false" from "user didn't say": env-overrides-
/// config layering means `[chain].keep_going = false` plus
/// `RUNNER_KEEP_GOING=1` resolves to `true`.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub(crate) struct ChainSection {
    /// Run every task in the chain to completion regardless of failures.
    /// Mutually exclusive with `kill_on_fail`. Equivalent to `-k` /
    /// `RUNNER_KEEP_GOING`.
    #[serde(default)]
    pub keep_going: Option<bool>,

    /// Parallel only: SIGTERM sibling tasks on first failure. Mutually
    /// exclusive with `keep_going`. Equivalent to `--kill-on-fail` /
    /// `RUNNER_KILL_ON_FAIL`. Ignored in sequential contexts.
    #[serde(default)]
    pub kill_on_fail: Option<bool>,
}
```

Add the field on `RunnerConfig` (after `resolution`):

```rust
/// `[chain]` — failure policy for multi-task chains.
#[serde(default)]
pub chain: ChainSection,
```

- [ ] **Step 4: Run tests**

```bash
cargo test --lib config::tests
```

Expected: both new tests pass; all existing config tests still pass.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "$(cat <<'EOF'
add `[chain]` section to runner.toml

Houses chain failure policy (`keep_going`, `kill_on_fail`) for the
new `run -s/-p` mode + `runner install <tasks>` form. `Option<bool>`
preserves explicit-false-vs-absent so env overrides config correctly.
Schema regen lives in the final task to avoid churn between commits.
EOF
)"
```

---

## Task 4: Failure-policy plumbing in `ResolutionOverrides`

Wires CLI / env / config / default precedence for `keep_going` and `kill_on_fail`, resolves them into a single `FailurePolicy`, and validates mutual exclusion. Mirrors the `--fallback` / `--on-mismatch` shape (see `src/resolver/mod.rs:788`).

The mutual-exclusion validation handles both the "two fields true in same source" case (caught at parse) and the cross-source case (env says keep-going, config says kill-on-fail — caught after layering).

**Files:**

- Modify: `src/resolver/mod.rs` — `ResolutionOverrides` (line 54), `OverrideSources` (line 580), `from_cli_and_env` (line 634), `from_sources` (line 693), and add helper functions
- Modify: `src/resolver/error.rs` — adding `ConflictingFailurePolicy` variant (covered in Task 5; this task references it)
- Test: `src/resolver/mod.rs` (existing test module)

> Task 5 introduces the error variant. If you're executing tasks strictly in order, treat the `ResolveError::ConflictingFailurePolicy` references below as compile-time forward references that resolve once Task 5 lands; or execute Tasks 4 and 5 as a paired commit.

- [ ] **Step 1: Write the failing test**

Append to the resolver test module in `src/resolver/mod.rs`:

```rust
#[test]
fn from_sources_resolves_cli_keep_going() {
    let overrides = ResolutionOverrides::from_sources(OverrideSources {
        keep_going: ExplainSource {
            cli: true,
            env: None,
        },
        ..OverrideSources::default()
    })
    .expect("resolves");
    assert_eq!(overrides.failure_policy, FailurePolicy::KeepGoing);
}

#[test]
fn from_sources_env_overrides_config_for_failure_policy() {
    let loaded = test_loaded_config_with_chain(
        Some(false), // keep_going in config
        None,
    );
    let overrides = ResolutionOverrides::from_sources(OverrideSources {
        keep_going: ExplainSource {
            cli: false,
            env: Some("1"),
        },
        config: Some(&loaded),
        ..OverrideSources::default()
    })
    .expect("resolves");
    assert_eq!(overrides.failure_policy, FailurePolicy::KeepGoing);
}

#[test]
fn from_sources_rejects_both_keep_going_and_kill_on_fail() {
    let err = ResolutionOverrides::from_sources(OverrideSources {
        keep_going: ExplainSource {
            cli: true,
            env: None,
        },
        kill_on_fail: ExplainSource {
            cli: true,
            env: None,
        },
        ..OverrideSources::default()
    })
    .expect_err("conflict must error");
    let downcast = err.downcast_ref::<ResolveError>();
    assert!(
        matches!(
            downcast,
            Some(ResolveError::ConflictingFailurePolicy { .. })
        ),
        "expected ConflictingFailurePolicy, got: {err:#}",
    );
}
```

Also add a helper near the top of the test module (or wherever `TempDir`-style test fixtures live):

```rust
#[cfg(test)]
fn test_loaded_config_with_chain(
    keep_going: Option<bool>,
    kill_on_fail: Option<bool>,
) -> crate::config::LoadedConfig {
    use crate::config::{ChainSection, LoadedConfig, RunnerConfig};
    LoadedConfig {
        path: std::path::PathBuf::from("/test/runner.toml"),
        config: RunnerConfig {
            chain: ChainSection {
                keep_going,
                kill_on_fail,
            },
            ..RunnerConfig::default()
        },
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test --lib resolver::tests::from_sources_resolves_cli_keep_going \
                  resolver::tests::from_sources_env_overrides_config_for_failure_policy \
                  resolver::tests::from_sources_rejects_both_keep_going_and_kill_on_fail
```

Expected: compile error — no `failure_policy` field, no `keep_going` / `kill_on_fail` on `OverrideSources`, no `ConflictingFailurePolicy` variant.

- [ ] **Step 3: Add `failure_policy` to `ResolutionOverrides`**

In `src/resolver/mod.rs` around line 85 (just before the closing brace of `ResolutionOverrides`), insert:

```rust
/// Failure policy for `run -s/-p` chains and `runner install <tasks>`.
/// Resolved from `-k`/`--kill-on-fail` (CLI) → `RUNNER_KEEP_GOING`/
/// `RUNNER_KILL_ON_FAIL` (env) → `[chain]` (config) → `FailFast`.
pub failure_policy: FailurePolicy,
```

Pull `FailurePolicy` into scope by adding to the existing `use crate::chain::...` line if present, otherwise add:

```rust
use crate::chain::FailurePolicy;
```

near the other crate-internal imports at the top of the file.

- [ ] **Step 4: Add `keep_going` and `kill_on_fail` to `OverrideSources`**

In `src/resolver/mod.rs` around line 595 (just before the closing brace of `OverrideSources`), insert:

```rust
/// `-k`/`--keep-going` flag presence plus `RUNNER_KEEP_GOING` env.
pub keep_going: ExplainSource<'a>,
/// `--kill-on-fail` flag presence plus `RUNNER_KILL_ON_FAIL` env.
pub kill_on_fail: ExplainSource<'a>,
```

- [ ] **Step 5: Extend `from_cli_and_env` to take and read the new sources**

Add two new parameters to `from_cli_and_env` after `cli_explain`:

```rust
pub(crate) fn from_cli_and_env(
    cli_pm: Option<&str>,
    cli_runner: Option<&str>,
    cli_fallback: Option<&str>,
    cli_on_mismatch: Option<&str>,
    cli_no_warnings: bool,
    cli_explain: bool,
    cli_keep_going: bool,
    cli_kill_on_fail: bool,
    config: Option<&LoadedConfig>,
) -> Result<Self> {
```

Read the env vars and pass through:

```rust
let env_keep_going = std::env::var("RUNNER_KEEP_GOING").ok();
let env_kill_on_fail = std::env::var("RUNNER_KILL_ON_FAIL").ok();
Self::from_sources(OverrideSources {
    // ... existing fields ...
    keep_going: ExplainSource {
        cli: cli_keep_going,
        env: env_keep_going.as_deref(),
    },
    kill_on_fail: ExplainSource {
        cli: cli_kill_on_fail,
        env: env_kill_on_fail.as_deref(),
    },
    config,
})
```

(Callers of `from_cli_and_env` in `src/lib.rs` need the two new arguments; pass `false`/`false` from any non-chain call sites for now. Task 6 wires them in for the actual CLI flags. Tests in `src/resolver/mod.rs` and elsewhere that call `from_cli_and_env` directly may need the additional args.)

- [ ] **Step 6: Add the `resolve_failure_policy` helper**

Add after `resolve_mismatch_policy` (around line 883 in `src/resolver/mod.rs`):

```rust
/// Resolve a chain failure policy from CLI/env/config sources.
///
/// `keep_going` and `kill_on_fail` are independent bool layers — CLI flag
/// presence beats env truthy beats `[chain]` config beats `false`. The
/// two layers are combined into a `FailurePolicy` and validated for
/// mutual exclusion: both true at any source or after layering returns
/// `ResolveError::ConflictingFailurePolicy`.
fn resolve_failure_policy(
    keep_going: ExplainSource<'_>,
    kill_on_fail: ExplainSource<'_>,
    config: Option<&LoadedConfig>,
) -> Result<FailurePolicy> {
    let keep = resolve_chain_bool(
        keep_going.cli,
        keep_going.env,
        config.and_then(|c| c.config.chain.keep_going),
    );
    let kill = resolve_chain_bool(
        kill_on_fail.cli,
        kill_on_fail.env,
        config.and_then(|c| c.config.chain.kill_on_fail),
    );

    // Per-source conflict detection: report the source where both went
    // true so the user can pin the offending knob quickly.
    if let Some(source) = single_source_conflict(&keep_going, &kill_on_fail, config) {
        return Err(ResolveError::ConflictingFailurePolicy { source }.into());
    }

    match (keep, kill) {
        (false, false) => Ok(FailurePolicy::FailFast),
        (true, false) => Ok(FailurePolicy::KeepGoing),
        (false, true) => Ok(FailurePolicy::KillOnFail),
        (true, true) => Err(ResolveError::ConflictingFailurePolicy {
            source: "cross-source",
        }
        .into()),
    }
}

/// Layered bool resolution: CLI flag > env truthy > config explicit > false.
fn resolve_chain_bool(cli: bool, env: Option<&str>, config: Option<bool>) -> bool {
    if cli {
        return true;
    }
    if let Some(raw) = env
        && is_env_truthy(raw)
    {
        return true;
    }
    config.unwrap_or(false)
}

/// If `keep_going` and `kill_on_fail` are both set true *within the same
/// source layer*, return that layer's label. None if no single-layer
/// conflict (cross-source conflicts are caught after layering).
fn single_source_conflict(
    keep: &ExplainSource<'_>,
    kill: &ExplainSource<'_>,
    config: Option<&LoadedConfig>,
) -> Option<&'static str> {
    if keep.cli && kill.cli {
        return Some("CLI flags");
    }
    let keep_env = keep.env.map(str::trim).filter(|s| !s.is_empty());
    let kill_env = kill.env.map(str::trim).filter(|s| !s.is_empty());
    if keep_env.is_some_and(is_env_truthy) && kill_env.is_some_and(is_env_truthy) {
        return Some("env vars");
    }
    if let Some(loaded) = config
        && loaded.config.chain.keep_going == Some(true)
        && loaded.config.chain.kill_on_fail == Some(true)
    {
        return Some("[chain] config");
    }
    None
}
```

- [ ] **Step 7: Call the helper from `from_sources`**

In `from_sources` (around line 714 in `src/resolver/mod.rs`, after `parse_prefer_runners`), add:

```rust
let failure_policy = resolve_failure_policy(
    sources.keep_going,
    sources.kill_on_fail,
    sources.config,
)?;
```

And include it in the `Ok(Self { ... })` literal at the bottom:

```rust
Ok(Self {
    pm,
    pm_by_ecosystem,
    runner,
    prefer_runners,
    fallback,
    on_mismatch,
    no_warnings,
    explain,
    failure_policy,
})
```

- [ ] **Step 8: Run the tests**

```bash
cargo test --lib resolver::tests::from_sources_resolves_cli_keep_going \
                  resolver::tests::from_sources_env_overrides_config_for_failure_policy \
                  resolver::tests::from_sources_rejects_both_keep_going_and_kill_on_fail
```

Expected: all three pass. Also run `cargo test --lib resolver::` to confirm nothing else broke.

- [ ] **Step 9: Commit**

```bash
git add src/resolver/mod.rs src/resolver/error.rs
git commit -m "$(cat <<'EOF'
add failure-policy precedence chain to resolver

`ResolutionOverrides.failure_policy` resolves from
`-k`/`--kill-on-fail` flags, `RUNNER_KEEP_GOING` / `RUNNER_KILL_ON_FAIL`
env vars, and the new `[chain]` config section in that order.
Mirrors the existing `--fallback` / `--on-mismatch` shape via a
`resolve_failure_policy` helper.

Mutual exclusion (keep-going vs kill-on-fail) is validated at every
source independently and again after layering — both-true returns
`ResolveError::ConflictingFailurePolicy { source }` so the
diagnostic names the offending layer (CLI / env / config / cross).
EOF
)"
```

---

## Task 5: `ResolveError::ConflictingFailurePolicy` variant

Spec §9. Add the variant first so Task 4's references compile; if Tasks 4 and 5 are landing as one commit, fold this into Task 4's commit.

**Files:**

- Modify: `src/resolver/error.rs:24` (the `ResolveError` enum)
- Modify: `src/resolver/error.rs:92` (the `Display` impl)
- Test: `src/resolver/error.rs` (`#[cfg(test)] mod tests` block — add if absent)

- [ ] **Step 1: Write the failing test**

Add a test module at the bottom of `src/resolver/error.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflicting_failure_policy_display_includes_source() {
        let err = ResolveError::ConflictingFailurePolicy { source: "env vars" };
        let msg = format!("{err}");
        assert!(msg.contains("keep_going"), "msg: {msg}");
        assert!(msg.contains("kill_on_fail"), "msg: {msg}");
        assert!(msg.contains("env vars"), "msg: {msg}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --lib resolver::error::tests
```

Expected: compile error — variant doesn't exist.

- [ ] **Step 3: Add the variant**

Insert into `ResolveError` (after `InvalidOverride`, line 73):

```rust
/// Both `keep_going` and `kill_on_fail` were set to true at the same
/// source (or once layered across CLI/env/config). The chain executor
/// can't honour both, so fail loudly before dispatching anything.
ConflictingFailurePolicy {
    /// Where the conflict was detected: `"CLI flags"`, `"env vars"`,
    /// `"[chain] config"`, or `"cross-source"`.
    source: &'static str,
},
```

Add the `Display` arm inside `impl fmt::Display for ResolveError` (after the `InvalidOverride` arm):

```rust
Self::ConflictingFailurePolicy { source } => write!(
    f,
    "`keep_going` and `kill_on_fail` are mutually exclusive but both were set ({source}). \
     Unset one of `--keep-going` / `RUNNER_KEEP_GOING` / `[chain].keep_going` or \
     `--kill-on-fail` / `RUNNER_KILL_ON_FAIL` / `[chain].kill_on_fail` to pick a policy.",
),
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test --lib resolver::error::tests::conflicting_failure_policy_display_includes_source
```

Expected: PASS.

- [ ] **Step 5: Commit**

(If folded into Task 4's commit, skip; otherwise commit standalone.)

```bash
git add src/resolver/error.rs
git commit -m "$(cat <<'EOF'
add ResolveError::ConflictingFailurePolicy variant

Fires when chain failure flags (`-k` / `--kill-on-fail`) conflict at
any source layer. Display message names the offending layer (CLI /
env / config / cross-source) so users can pin the bad knob quickly.
EOF
)"
```

---

## Task 6: CLI flags on `run` (`Cli::Run` + `RunAliasCli`)

Implements spec §2.1. Adds `-s` / `-p` / `-k` / `--kill-on-fail` to both run entry points. `-s` and `-p` conflict with each other; `-k` and `--kill-on-fail` conflict with each other.

The chain mode is implicit in the presence of `-s` or `-p`; without either, the existing single-task path is unchanged. `task` becomes optional (chain mode uses the positional list directly via `args`); when `-s`/`-p` is set we treat all positionals as task names.

The cleanest clap shape: keep `task: Option<String>` + `args: Vec<String>` and post-validate at the dispatch site (Task 12) — the chain parser glues `[task].iter().chain(args.iter())` into the positional list and applies v1 validation.

**Files:**

- Modify: `src/cli.rs:692` (the `Command::Run` variant)
- Modify: `src/cli.rs:799` (the `RunAliasCli` struct)
- Test: `src/cli.rs` (existing test module)

- [ ] **Step 1: Write the failing test**

Append to `src/cli.rs`'s test module:

```rust
#[test]
fn run_accepts_sequential_chain_flag() {
    let cli = Cli::try_parse_from(["runner", "run", "-s", "build", "test"]).expect("parses");
    let Command::Run {
        task,
        args,
        sequential,
        parallel,
        ..
    } = cli.command.expect("subcommand")
    else {
        panic!("expected Run")
    };
    assert!(sequential, "-s should set sequential");
    assert!(!parallel, "-p should not be set");
    assert_eq!(task.as_deref(), Some("build"));
    assert_eq!(args, vec!["test".to_string()]);
}

#[test]
fn run_rejects_sequential_and_parallel_together() {
    let err = Cli::try_parse_from(["runner", "run", "-s", "-p", "build"]).expect_err("conflict");
    let msg = format!("{err}");
    assert!(msg.contains("--parallel") || msg.contains("--sequential"));
}

#[test]
fn run_rejects_keep_going_and_kill_on_fail_together() {
    let err = Cli::try_parse_from([
        "runner",
        "run",
        "-s",
        "-k",
        "--kill-on-fail",
        "build",
        "test",
    ])
    .expect_err("conflict");
    let msg = format!("{err}");
    assert!(msg.contains("--keep-going") || msg.contains("--kill-on-fail"));
}

#[test]
fn run_alias_parses_chain_flags_too() {
    let cli = RunAliasCli::try_parse_from(["run", "-p", "lint", "test"]).expect("parses");
    assert!(cli.parallel);
    assert!(!cli.sequential);
    assert_eq!(cli.task.as_deref(), Some("lint"));
    assert_eq!(cli.args, vec!["test".to_string()]);
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test --lib cli::tests::run_accepts_sequential_chain_flag \
                  cli::tests::run_rejects_sequential_and_parallel_together \
                  cli::tests::run_rejects_keep_going_and_kill_on_fail_together \
                  cli::tests::run_alias_parses_chain_flags_too
```

Expected: compile error — no `sequential` / `parallel` / `keep_going` / `kill_on_fail` fields.

- [ ] **Step 3: Add chain flags to `Command::Run`**

Replace the `Command::Run` variant in `src/cli.rs:696` with:

```rust
/// Run a task, or exec a command through the detected package manager.
/// With `-s` or `-p`, runs multiple tasks as a chain.
#[command(alias = "r")]
Run {
    /// Task name or command to execute. In chain mode, the first task in the chain.
    #[arg(add = ArgValueCandidates::new(task_candidates))]
    task: Option<String>,
    /// Arguments forwarded to the task, OR additional task names in chain mode.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
    /// Run the given tasks sequentially. Conflicts with `--parallel`.
    #[arg(short = 's', long, conflicts_with = "parallel")]
    sequential: bool,
    /// Run the given tasks in parallel. Conflicts with `--sequential`.
    #[arg(short = 'p', long)]
    parallel: bool,
    /// Run every task in the chain regardless of failures. Conflicts
    /// with `--kill-on-fail`.
    #[arg(short = 'k', long, conflicts_with = "kill_on_fail")]
    keep_going: bool,
    /// Parallel only: SIGTERM siblings on first failure. Accepted but
    /// unused in sequential mode.
    #[arg(long)]
    kill_on_fail: bool,
},
```

(Note `task` changed from `String` to `Option<String>` — the bare `runner run` invocation already worked because `Option<String>` is the existing shape used by `RunAliasCli` and the calling code in `src/lib.rs` handles `None`. If a separate dispatch site assumed `task: String`, follow the compile error and add a `.unwrap_or_default()` or `bail!("task required")` guard.)

- [ ] **Step 4: Add chain flags to `RunAliasCli`**

Append to `src/cli.rs:811` (inside `RunAliasCli`, after `args`):

```rust
    /// Run the given tasks sequentially. Conflicts with `--parallel`.
    #[arg(short = 's', long, conflicts_with = "parallel")]
    pub sequential: bool,

    /// Run the given tasks in parallel. Conflicts with `--sequential`.
    #[arg(short = 'p', long)]
    pub parallel: bool,

    /// Run every task in the chain regardless of failures.
    #[arg(short = 'k', long, conflicts_with = "kill_on_fail")]
    pub keep_going: bool,

    /// Parallel only: SIGTERM siblings on first failure.
    #[arg(long)]
    pub kill_on_fail: bool,
```

- [ ] **Step 5: Run tests**

```bash
cargo test --lib cli::tests
```

Expected: all four new tests pass, plus all existing cli tests.

- [ ] **Step 6: Commit**

```bash
git add src/cli.rs
git commit -m "$(cat <<'EOF'
add chain flags `-s`/`-p`/`-k`/`--kill-on-fail` to run

Mirrored across `Command::Run` and `RunAliasCli` (the `run` alias
binary). `-s` and `-p` conflict via clap; `-k` and `--kill-on-fail`
conflict via clap. Chain mode is implicit in the presence of `-s`
or `-p`; the existing single-task `run task --watch` path stays
unchanged when neither is set.

`task` became `Option<String>` on `Command::Run` to align with the
alias binary's shape and let chain-mode dispatch glue the positional
list together at the call site.
EOF
)"
```

---

## Task 7: Task-list positional on `runner install` + `-k` / `--kill-on-fail`

Implements spec §2.2. The `runner install` subcommand learns to accept a positional list of task names. `-p` is **rejected** at parse time (install is always sequential). `-k` and `--kill-on-fail` parse but only `-k` is meaningful for install.

**Files:**

- Modify: `src/cli.rs:707` (the `Command::Install` variant)
- Test: `src/cli.rs` (existing test module)

- [ ] **Step 1: Write the failing test**

Append to `src/cli.rs`'s test module:

```rust
#[test]
fn install_accepts_task_list() {
    let cli = Cli::try_parse_from(["runner", "install", "build", "test"]).expect("parses");
    let Command::Install {
        tasks,
        frozen,
        keep_going,
        ..
    } = cli.command.expect("subcommand")
    else {
        panic!("expected Install")
    };
    assert!(!frozen);
    assert!(!keep_going);
    assert_eq!(tasks, vec!["build".to_string(), "test".to_string()]);
}

#[test]
fn install_accepts_keep_going_flag() {
    let cli = Cli::try_parse_from(["runner", "install", "-k", "build"]).expect("parses");
    let Command::Install {
        tasks, keep_going, ..
    } = cli.command.expect("subcommand")
    else {
        panic!("expected Install")
    };
    assert!(keep_going);
    assert_eq!(tasks, vec!["build".to_string()]);
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test --lib cli::tests::install_accepts_task_list \
                  cli::tests::install_accepts_keep_going_flag
```

Expected: compile error — no `tasks` / `keep_going` fields on `Install`.

- [ ] **Step 3: Replace `Command::Install` with the chain-aware shape**

In `src/cli.rs:707`, replace:

```rust
/// Install project dependencies, then optionally chain tasks
/// (`runner install build test` → install → build → test, sequential).
#[command(alias = "i")]
Install {
    /// Reproducible install from lockfile (npm ci, --frozen-lockfile, etc.)
    #[arg(long)]
    frozen: bool,
    /// Optional task names to run after install completes. Chain is
    /// always sequential; `-p` is not accepted here.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    tasks: Vec<String>,
    /// Run every chained task regardless of failures. Equivalent to
    /// `-k` on `run -s`.
    #[arg(short = 'k', long, conflicts_with = "kill_on_fail")]
    keep_going: bool,
    /// Accepted but unused (install chain is always sequential).
    #[arg(long, hide = true)]
    kill_on_fail: bool,
},
```

> Note: `--kill-on-fail` is `hide = true` because it's a no-op for install. We accept it (to satisfy spec §2.2: "accepted but redundant") but don't advertise it in `--help` to avoid confusing users.

- [ ] **Step 4: Run tests**

```bash
cargo test --lib cli::tests
```

Expected: new tests pass; existing install tests still pass (the `frozen` flag is unchanged).

- [ ] **Step 5: Commit**

```bash
git add src/cli.rs
git commit -m "$(cat <<'EOF'
extend `runner install` with task-list + chain flags

`runner install build test` runs PM install, then dispatches `build`
and `test` sequentially. `-k` runs the chain regardless of failures.
`--kill-on-fail` is accepted (per spec) but hidden in `--help` since
install chains are always sequential.

Install never offers `-p` — chain mode there is always sequential.
EOF
)"
```

---

## Task 8: Chain parser in `src/chain/parse.rs`

Implements spec §10 (v1 validation rules). Converts the positional list (gathered from CLI in Tasks 6/7) into a `Vec<ChainItem>` and applies v1 rejections.

**Files:**

- Create: `src/chain/parse.rs`
- Test: `src/chain/parse.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Create `src/chain/parse.rs` with:

```rust
//! Chain parser. Converts the positional list from CLI into a typed
//! `Vec<ChainItem>` and applies v1 validation rules.

use anyhow::{Result, anyhow, bail};

use super::ChainItem;

/// Parse a positional list of task names into a v1 chain.
///
/// v1 rules (reserved space for v2 quoted bundles — see spec §10):
/// - Positionals containing whitespace are rejected.
/// - Positionals starting with `-` are rejected.
/// - At least one task is required.
pub(crate) fn parse_task_list(raw: &[String]) -> Result<Vec<ChainItem>> {
    if raw.is_empty() {
        bail!("chain mode requires at least one task name");
    }
    let mut out = Vec::with_capacity(raw.len());
    for token in raw {
        validate_v1_token(token)?;
        out.push(ChainItem::task(token));
    }
    Ok(out)
}

fn validate_v1_token(token: &str) -> Result<()> {
    if token.is_empty() {
        bail!("empty task name in chain");
    }
    if token.chars().any(char::is_whitespace) {
        return Err(anyhow!(
            "per-task arguments are not supported in this version\n\
             note: positional {token:?} contains whitespace\n\
             note: quoted-bundle syntax is reserved for a future runner release",
        ));
    }
    if token.starts_with('-') {
        return Err(anyhow!(
            "in chain mode, all positionals must be task names (got {token:?}). \
             To forward arguments to a single task, drop `-s`/`-p` and use the \
             classic `run <task> <args...>` form.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::ChainItemKind;

    #[test]
    fn parses_simple_task_list() {
        let items =
            parse_task_list(&["build".into(), "test".into(), "lint".into()]).expect("parses");
        assert_eq!(items.len(), 3);
        assert!(matches!(&items[0].kind, ChainItemKind::Task(n) if n == "build"));
        assert!(items[0].args.is_empty(), "v1 always empty");
    }

    #[test]
    fn rejects_empty_list() {
        let err = parse_task_list(&[]).expect_err("empty list");
        assert!(format!("{err:#}").contains("at least one"));
    }

    #[test]
    fn rejects_token_with_whitespace() {
        let err = parse_task_list(&["build --release".into()]).expect_err("whitespace token");
        let msg = format!("{err:#}");
        assert!(msg.contains("whitespace"), "msg: {msg}");
        assert!(msg.contains("quoted-bundle"), "msg: {msg}");
    }

    #[test]
    fn rejects_token_starting_with_dash() {
        let err = parse_task_list(&["build".into(), "--release".into()])
            .expect_err("dash-prefixed token");
        let msg = format!("{err:#}");
        assert!(msg.contains("task names"), "msg: {msg}");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test --lib chain::parse::tests
```

Expected: compile error (file is new but the `pub(crate) mod parse;` line in `src/chain/mod.rs` from Task 2 wired it in).

- [ ] **Step 3: Verify tests pass**

The implementation above is the full file — tests should pass on first compile after the file lands.

```bash
cargo test --lib chain::parse::tests
```

Expected: all four tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/chain/parse.rs
git commit -m "$(cat <<'EOF'
add chain parser with v1 validation

`parse_task_list` converts the positional list from CLI into a
`Vec<ChainItem>`. v1 rejects positionals containing whitespace
(reserved for v2 quoted-bundle syntax) or starting with `-`
(unambiguous orphan flag).
EOF
)"
```

---

## Task 9: Line-prefix multiplexer in `src/chain/mux.rs`

Implements spec §5. Sits between N piped child stdio streams and the parent's terminal. Used only by parallel mode (Task 11).

The implementation uses `std::thread::scope` for spawning reader threads tied to the call's lifetime, plus a single `mpsc::channel` to serialize writes. Color comes from `colored::Colorize` which is already a dependency; `NO_COLOR` / non-TTY gating is via `is_terminal()` on the parent's stdout/stderr.

**Files:**

- Create: `src/chain/mux.rs`
- Test: `src/chain/mux.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Sketch the public API and write the failing test**

Create `src/chain/mux.rs` with the structure:

```rust
//! Line-prefix multiplexer for parallel chain output. Captures each
//! task's stdout/stderr, prefixes lines with `[<task-name>]`, and
//! writes to the parent terminal. Color and prefix-padding are derived
//! from the set of task names supplied up front.

use std::io::{BufRead, BufReader, Read};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use colored::{Color, Colorize};

/// One captured line from a task's piped stdio.
#[derive(Debug)]
pub(crate) struct PrefixedLine {
    /// Padded prefix bracket, e.g. `[build ]` — color may be embedded.
    pub prefix: String,
    /// The line content (no trailing `\n`).
    pub line: String,
    /// Whether this line came from the task's stderr.
    pub is_stderr: bool,
}

/// Compute the right-padded width for prefix labels in the chain.
pub(crate) fn prefix_width(names: &[&str]) -> usize {
    names.iter().map(|n| n.chars().count()).max().unwrap_or(0)
}

/// Deterministic ANSI color for a task name, chosen from an 8-color
/// palette so multiple parallel tasks visually distinguish.
pub(crate) fn color_for(name: &str) -> Color {
    const PALETTE: [Color; 8] = [
        Color::Cyan,
        Color::Magenta,
        Color::Yellow,
        Color::Green,
        Color::Blue,
        Color::Red,
        Color::BrightCyan,
        Color::BrightMagenta,
    ];
    let hash = name
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    PALETTE[hash as usize % PALETTE.len()]
}

/// Render a prefix bracket for `name` padded to `width` characters.
/// Skips color when `colorize == false` (NO_COLOR or non-TTY).
pub(crate) fn render_prefix(name: &str, width: usize, colorize: bool) -> String {
    let padded = format!("{name:<width$}");
    let bracketed = format!("[{padded}]");
    if colorize {
        bracketed.color(color_for(name)).to_string()
    } else {
        bracketed
    }
}

/// Spawn reader threads that read line-by-line from each `Read`, prefix
/// the lines, and send them through the returned receiver. The caller
/// must keep the returned join handles alive until the channel closes.
///
/// `streams` is a slice of `(prefix, is_stderr, reader)` tuples.
pub(crate) fn spawn_readers<R>(
    streams: Vec<(String, bool, R)>,
    sender: Sender<PrefixedLine>,
) -> Vec<JoinHandle<()>>
where
    R: Read + Send + 'static,
{
    streams
        .into_iter()
        .map(|(prefix, is_stderr, reader)| {
            let tx = sender.clone();
            std::thread::spawn(move || {
                let buf = BufReader::new(reader);
                for line in buf.lines() {
                    let Ok(line) = line else { return };
                    if tx
                        .send(PrefixedLine {
                            prefix: prefix.clone(),
                            line,
                            is_stderr,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_width_picks_longest() {
        assert_eq!(prefix_width(&["a", "build", "test"]), 5);
        assert_eq!(prefix_width(&[]), 0);
    }

    #[test]
    fn color_for_is_deterministic() {
        assert_eq!(color_for("build"), color_for("build"));
        let a = color_for("build");
        let b = color_for("test");
        // Not asserting inequality (hash collisions possible across many
        // names), just determinism for the same input.
        let _ = (a, b);
    }

    #[test]
    fn render_prefix_pads_and_brackets() {
        let p = render_prefix("a", 5, false);
        assert_eq!(p, "[a    ]");
    }

    #[test]
    fn render_prefix_colors_when_enabled() {
        let p = render_prefix("a", 1, true);
        // ANSI escape sequence present; bracket content unchanged.
        assert!(p.contains("[a]"));
        assert!(p.contains("\u{1b}["));
    }

    #[test]
    fn spawn_readers_streams_lines_through_channel() {
        let (tx, rx) = channel();
        let stream = std::io::Cursor::new(b"hello\nworld\n".to_vec());
        let handles = spawn_readers(vec![("[t]".into(), false, stream)], tx);
        for h in handles {
            h.join().unwrap();
        }
        let mut got: Vec<String> = rx.iter().map(|p| p.line).collect();
        got.sort();
        assert_eq!(got, vec!["hello".to_string(), "world".to_string()]);
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

(The file above is complete; tests should pass on first compile.)

```bash
cargo test --lib chain::mux::tests
```

Expected: all five tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/chain/mux.rs
git commit -m "$(cat <<'EOF'
add line-prefix multiplexer for parallel chain output

Self-contained helpers for prefix padding (`prefix_width`),
deterministic ANSI color per task name (`color_for`), prefix
rendering (`render_prefix`), and a reader-thread spawner that pipes
captured lines through an `mpsc::Sender<PrefixedLine>`.

No knowledge of chains, tasks, or resolvers — pure I/O primitives
the parallel executor (Task 11) composes.
EOF
)"
```

---

## Task 10: Sequential executor in `src/chain/exec.rs`

Implements spec §4.1. Sequential mode inherits stdio directly (no muxer), prints a banner before each task, applies fail-fast or keep-going.

Per-item dispatch reuses `crate::cmd::run::run` for `ChainItemKind::Task` and a new `install_pms_chain_aware(ctx, frozen) -> Result<i32>` helper for `ChainItemKind::Install` (extracted in Task 13). For now, stub the install branch and return an error so we can land sequential dispatch incrementally.

**Files:**

- Create: `src/chain/exec.rs`
- Test: `src/chain/exec.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Create `src/chain/exec.rs`:

```rust
//! Chain executor. Sequential mode inherits parent stdio. Parallel
//! mode pipes per-task stdio through the prefix multiplexer in
//! `chain::mux`.

use anyhow::Result;

use crate::chain::{Chain, ChainItem, ChainItemKind, ChainMode, FailurePolicy};
use crate::resolver::ResolutionOverrides;
use crate::types::ProjectContext;

/// Dispatch a chain. Returns the first failing task's exit code, or 0
/// if every task succeeded.
pub(crate) fn run_chain(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    chain: &Chain,
) -> Result<i32> {
    match chain.mode {
        ChainMode::Sequential => run_sequential(ctx, overrides, chain),
        ChainMode::Parallel => run_parallel(ctx, overrides, chain),
    }
}

fn run_sequential(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    chain: &Chain,
) -> Result<i32> {
    let keep_going = matches!(chain.failure, FailurePolicy::KeepGoing);
    let mut first_failure: Option<i32> = None;

    for item in &chain.items {
        let code = dispatch_item(ctx, overrides, item)?;
        if code != 0 {
            first_failure.get_or_insert(code);
            if !keep_going {
                return Ok(code);
            }
        }
    }
    Ok(first_failure.unwrap_or(0))
}

fn run_parallel(
    _ctx: &ProjectContext,
    _overrides: &ResolutionOverrides,
    _chain: &Chain,
) -> Result<i32> {
    // Filled in by Task 11.
    anyhow::bail!("parallel chain execution not yet implemented");
}

fn dispatch_item(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    item: &ChainItem,
) -> Result<i32> {
    match &item.kind {
        ChainItemKind::Task(name) => {
            // v1 ChainItem.args is always empty; v2 will populate it.
            crate::cmd::run::run(ctx, overrides, name, &item.args)
        }
        ChainItemKind::Install => {
            // Wired in Task 13 once `install_pms` is extracted.
            anyhow::bail!("install dispatch not yet wired into chain executor")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::{ChainItem, ChainMode, FailurePolicy};

    fn empty_ctx() -> ProjectContext {
        ProjectContext::default()
    }

    fn empty_overrides() -> ResolutionOverrides {
        ResolutionOverrides::from_sources(crate::resolver::OverrideSources::default())
            .expect("default sources resolve")
    }

    #[test]
    fn run_chain_sequential_returns_zero_when_empty() {
        let chain = Chain {
            mode: ChainMode::Sequential,
            items: vec![],
            failure: FailurePolicy::FailFast,
        };
        // The "empty chain" case is rejected at parse, but the executor
        // is defensive — exercising it directly should be a no-op.
        let code =
            run_chain(&empty_ctx(), &empty_overrides(), &chain).expect("empty chain runs cleanly");
        assert_eq!(code, 0);
    }
}
```

> The "empty ctx" + "empty overrides" fixtures need `ProjectContext::default()` and the default `OverrideSources` to work in tests. If `ProjectContext` lacks `Default`, add a test-only helper that builds a minimal context (or use the existing test fixtures in `src/cmd/run.rs:select_task_entry_*` tests as a reference).

- [ ] **Step 2: Run the test to verify it passes**

```bash
cargo test --lib chain::exec::tests::run_chain_sequential_returns_zero_when_empty
```

Expected: PASS (the empty-chain branch is a no-op).

- [ ] **Step 3: Commit**

```bash
git add src/chain/exec.rs
git commit -m "$(cat <<'EOF'
add sequential chain executor

`run_chain` dispatches a `Chain` and returns the first failing task's
exit code. Sequential mode inherits parent stdio (no muxer), banners
come from the existing `cmd::run::run` per-task. Fail-fast aborts
after the first non-zero exit; keep-going collects all exits but
still returns the first failure's code.

Parallel mode is stubbed (Task 11) and `ChainItemKind::Install`
dispatch is stubbed (Task 13).
EOF
)"
```

---

## Task 11: Parallel executor

Implements spec §4.2. Replaces the `run_parallel` stub with a real implementation using `std::thread::scope`, piped stdio per child, the prefix muxer from Task 9, and a writer thread that drains the channel.

This is the most intricate task. Read spec §4.2 and §5 alongside.

**Files:**

- Modify: `src/chain/exec.rs` (replace `run_parallel` stub)
- Test: `src/chain/exec.rs` (add integration-style test using a real subprocess)

> Reusing `cmd::run::run` for parallel dispatch is awkward because that function inherits stdio (`Stdio::inherit()`) and we need piped stdio. Two options:
>
> 1. **Extract a `dispatch_task_piped(ctx, overrides, task, args) -> Result<Child>` from `cmd::run::run`** that returns a spawned `Child` with piped stdio, and have the parallel executor manage waits + muxing. This is the cleanest split.
> 2. **Spawn a sub-process for each task that re-invokes `runner run <task>`** with piped stdio. Simpler but adds binary recursion + duplicate resolution work per task.
>
> Pick option 1. The extraction is small: lift the body of `cmd::run::run` that resolves a task into a `Command`, set piped stdio, return the `Child`. The single-task path keeps inheriting stdio by calling the same helper with an `inherit_stdio: bool` arg or via two parallel helpers.

- [ ] **Step 1: Extract `dispatch_task_piped` from `cmd::run`**

This step needs careful reading of `src/cmd/run.rs` to find where the resolved task becomes a `Command::spawn`. Refactor the resolved-to-spawn segment into a helper that takes a `piped: bool` parameter, then call it from both `run` (inherit) and `run_parallel` (piped).

Pseudocode for the new helper:

```rust
pub(crate) fn dispatch_task_piped(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    task: &str,
    args: &[String],
) -> Result<std::process::Child> {
    // ... existing resolution logic from `cmd::run::run` up to the point
    // where it has a `Command` ready to spawn ...
    let mut cmd = build_resolved_command(ctx, overrides, task, args)?;
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    Ok(cmd.spawn()?)
}
```

Concrete refactor depends on the current `cmd::run::run` body — when implementing this task, read `src/cmd/run.rs:36` through to its first child-spawn call to understand the structure before splitting.

- [ ] **Step 2: Write the parallel executor**

Replace `run_parallel` in `src/chain/exec.rs`:

```rust
fn run_parallel(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    chain: &Chain,
) -> Result<i32> {
    use std::process::Child;
    use std::sync::mpsc::channel;

    use crate::chain::mux::{PrefixedLine, prefix_width, render_prefix, spawn_readers};

    let names: Vec<&str> = chain.items.iter().map(|i| i.display_name()).collect();
    let width = prefix_width(&names);
    let colorize = colored::control::SHOULD_COLORIZE.should_colorize();

    let (tx, rx) = channel::<PrefixedLine>();

    std::thread::scope(|s| -> Result<i32> {
        // Spawn each task, capture its piped stdio readers, and tee them
        // through the muxer.
        let mut children: Vec<(String, Child)> = Vec::with_capacity(chain.items.len());
        let mut reader_handles = Vec::new();

        for item in &chain.items {
            let prefix = render_prefix(item.display_name(), width, colorize);
            let mut child = match &item.kind {
                ChainItemKind::Task(name) => {
                    crate::cmd::run::dispatch_task_piped(ctx, overrides, name, &item.args)?
                }
                ChainItemKind::Install => {
                    anyhow::bail!("install dispatch not yet wired into parallel executor")
                }
            };
            let stdout = child.stdout.take().expect("piped");
            let stderr = child.stderr.take().expect("piped");
            reader_handles.extend(spawn_readers(
                vec![
                    (prefix.clone(), false, stdout),
                    (prefix.clone(), true, stderr),
                ],
                tx.clone(),
            ));
            children.push((item.display_name().to_string(), child));
        }

        // Drop the producer-side sender so the channel closes once all
        // reader threads finish.
        drop(tx);

        // Writer thread: drain the channel and write prefixed lines.
        let writer = s.spawn(move || {
            use std::io::Write;
            let mut stdout = std::io::stdout().lock();
            let mut stderr = std::io::stderr().lock();
            for msg in rx {
                let target: &mut dyn Write = if msg.is_stderr {
                    &mut stderr
                } else {
                    &mut stdout
                };
                let _ = writeln!(target, "{} {}", msg.prefix, msg.line);
            }
        });

        // Wait for every child. Track first-failure exit code.
        let mut first_failure: Option<i32> = None;
        let mut failure_seen = false;
        let kill_on_fail = matches!(chain.failure, FailurePolicy::KillOnFail);

        for (name, mut child) in children {
            let status = child.wait()?;
            let code = status.code().unwrap_or(1);
            if code != 0 {
                first_failure.get_or_insert(code);
                if kill_on_fail && !failure_seen {
                    failure_seen = true;
                    // Send SIGTERM to remaining children. They're already
                    // in the `children` vec we're iterating; on Unix,
                    // `Child::kill()` sends SIGKILL. For SIGTERM we'd need
                    // a libc nix dep — v1 uses SIGKILL via `Child::kill`
                    // since it's the std-library primitive. Reconsider
                    // if a follow-up needs proper SIGTERM semantics.
                    let _ = name; // silence unused
                    // Best-effort: walk remaining children and kill.
                    // (Implementation note: this loop has already moved
                    // them out of `children`, so the kill needs to happen
                    // BEFORE the wait loop or via a different structure.
                    // See implementation note below.)
                }
            }
            // Drain stdout/stderr for cleanly closed children.
            let _ = (name, status);
        }

        // Join readers + writer. Readers finish when child stdio closes;
        // writer finishes when all senders drop.
        for h in reader_handles {
            let _ = h.join();
        }
        let _ = writer.join();

        Ok(first_failure.unwrap_or(0))
    })
}
```

> **Implementation note on kill-on-fail:** The wait-then-kill loop above moves children out of the `Vec`, which means we can't kill remaining children after wait finds the first failure. The correct shape is either:
>
> - Keep children in an `Arc<Mutex<Vec<Child>>>` shared with a watcher thread, OR
> - Use `try_wait` in a loop so we can detect the first failure without consuming children, then kill the rest.
>
> The simplest fix: change `Vec<(String, Child)>` to `Vec<(String, Arc<Mutex<Option<Child>>>)>` and have wait threads `take()` the child when done. For v1, the option-2 try-wait loop is cleaner — see below.

- [ ] **Step 3: Refine kill-on-fail with a try-wait loop**

Replace the wait loop in `run_parallel` with a try-wait poll:

```rust
        // Poll children. On first failure with kill_on_fail, terminate
        // remaining siblings; otherwise let them finish.
        let mut remaining: Vec<(String, Child)> = children;
        let mut first_failure: Option<i32> = None;
        let kill_on_fail = matches!(chain.failure, FailurePolicy::KillOnFail);

        while !remaining.is_empty() {
            let mut next: Vec<(String, Child)> = Vec::with_capacity(remaining.len());
            for (name, mut child) in remaining.drain(..) {
                match child.try_wait()? {
                    Some(status) => {
                        let code = status.code().unwrap_or(1);
                        if code != 0 {
                            first_failure.get_or_insert(code);
                            if kill_on_fail {
                                for (_, mut sibling) in next.drain(..) {
                                    let _ = sibling.kill();
                                }
                                // Drain the rest of `remaining` (none yet,
                                // they're still in iteration scope).
                            }
                        }
                    }
                    None => {
                        // Still running — keep for the next pass.
                        next.push((name, child));
                    }
                }
            }
            remaining = next;
            if !remaining.is_empty() {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
```

> The 50 ms poll interval is conservative. If users complain about latency, drop to 10–20 ms.

- [ ] **Step 4: Write the integration test**

This test spawns a small parallel chain with controlled exit codes and verifies behavior. Skip it if your test environment can't reliably spawn child processes (CI-only test).

```rust
#[test]
fn parallel_chain_returns_first_failure_code() {
    // This test requires a project context that can actually dispatch
    // tasks. Easiest: skip in unit-test layer, exercise via integration
    // tests in tests/fixtures/. See Task 15.
}
```

Move the deep parallel testing to the integration suite in Task 15. The unit test for `chain::exec` stays minimal — testing the executor in isolation needs a mock `cmd::run::run`, which adds plumbing not worth it for v1.

- [ ] **Step 5: Commit**

```bash
git add src/chain/exec.rs src/cmd/run.rs
git commit -m "$(cat <<'EOF'
add parallel chain executor

`run_parallel` spawns each task with piped stdio, tees stdout/stderr
through the line-prefix muxer (`chain::mux`), and try-waits all
children in a polling loop. On first failure: `FailFast` lets
siblings finish, `KeepGoing` collects all exits, `KillOnFail`
sends SIGKILL to siblings (std-library primitive — SIGTERM would
need a libc dep; deferred).

Extracts `dispatch_task_piped` from `cmd::run::run` so the per-task
spawn logic is shared between inherit-stdio (sequential, single-task)
and piped-stdio (parallel) paths.

Integration tests in tests/fixtures/ exercise the full parallel
flow with real child processes (Task 15).
EOF
)"
```

---

## Task 12: Wire chain mode into `cmd::run::run`

Implements spec §2 + §4 — the boundary between CLI and chain executor. `cmd::run::run` learns to detect chain mode and delegate.

**Files:**

- Modify: `src/cmd/run.rs:36` (top of `pub(crate) fn run`)
- Modify: `src/lib.rs` (the dispatch site that calls `cmd::run::run` — Task 6 changed `task` from `String` to `Option<String>`, so the call site needs to pass `task.as_deref().unwrap_or("")` or branch on `None`)

- [ ] **Step 1: Find the call site in `src/lib.rs`**

Search for the existing call to `crate::cmd::run::run` in `src/lib.rs`. It should look roughly like:

```rust
Command::Run { task, args } => crate::cmd::run::run(&ctx, &overrides, &task, &args),
```

- [ ] **Step 2: Write the failing test**

Add to `src/cmd/run.rs`'s test module:

```rust
// Test removed — chain dispatch is exercised end-to-end in Task 15's
// integration fixtures. Unit-testing the branch here would require
// mocking `chain::exec::run_chain`, which is fragile and low-value.
```

(No new unit test for this step — the branch is a thin delegation and is covered by integration tests.)

- [ ] **Step 3: Add the chain branch**

In `src/lib.rs`, replace the `Command::Run` arm:

```rust
Command::Run { task, args, sequential, parallel, keep_going, kill_on_fail } => {
    if sequential || parallel {
        let mode = if parallel {
            crate::chain::ChainMode::Parallel
        } else {
            crate::chain::ChainMode::Sequential
        };
        let mut positionals: Vec<String> = Vec::new();
        if let Some(t) = task {
            positionals.push(t);
        }
        positionals.extend(args);
        let items = crate::chain::parse::parse_task_list(&positionals)?;
        // The CLI-level keep_going/kill_on_fail flags are now part of
        // `ResolutionOverrides.failure_policy` (Task 4 wired them in
        // via `from_cli_and_env`). Use that as the source of truth so
        // env/config layering still applies.
        let _ = (keep_going, kill_on_fail);
        let chain = crate::chain::Chain {
            mode,
            items,
            failure: overrides.failure_policy,
        };
        let code = crate::chain::exec::run_chain(&ctx, &overrides, &chain)?;
        std::process::exit(code);
    }
    // Existing single-task path:
    let task = task.unwrap_or_else(|| {
        eprintln!("error: task name required (see `runner --help`)");
        std::process::exit(2);
    });
    let code = crate::cmd::run::run(&ctx, &overrides, &task, &args)?;
    std::process::exit(code);
}
```

Mirror the same change for the `RunAliasCli` dispatch path in `src/bin/run.rs` (or wherever `RunAliasCli` is dispatched).

- [ ] **Step 4: Update `from_cli_and_env` call sites**

Find the existing `ResolutionOverrides::from_cli_and_env(...)` call (likely in `src/lib.rs`) and pass the new `cli_keep_going` / `cli_kill_on_fail` args from the parsed `RunAliasCli` / `Cli::Run`. For non-run subcommands, pass `false` / `false`.

- [ ] **Step 5: Run cargo build to verify the wiring**

```bash
cargo build
cargo test --lib
```

Expected: clean build, all existing tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/lib.rs src/bin/run.rs src/cmd/run.rs
git commit -m "$(cat <<'EOF'
wire chain mode into run dispatch

When `-s` or `-p` is set on `runner run` / `run`, the dispatcher
builds a `Chain` from the positional list and delegates to
`chain::exec::run_chain`. The single-task path is unchanged when
neither flag is set.

`ResolutionOverrides.failure_policy` (from Task 4) is the source of
truth for `-k` / `--kill-on-fail`, so CLI flag layering, env vars,
and `[chain]` config all flow through one resolver.
EOF
)"
```

---

## Task 13: Wire chain mode into `cmd::install`

Implements spec §6. The install command learns to accept a task list and chain through it after install. Refactors `install()` to expose a chain-aware variant returning an exit code.

**Files:**

- Modify: `src/cmd/install.rs:15` (`install` function)
- Modify: `src/lib.rs` (or `src/cli.rs` dispatch — wherever `Command::Install` is matched)
- Modify: `src/chain/exec.rs` (replace the install stub in `dispatch_item`)

- [ ] **Step 1: Extract `install_pms` from `install`**

Split the existing `install()` body into two parts: the PM-iteration logic that returns an exit code, and the thin wrapper that bails on non-zero.

In `src/cmd/install.rs`:

```rust
/// Public entry — runs install across every detected PM, bailing on
/// the first failure. Returns `Ok(())` on success.
pub(crate) fn install(ctx: &ProjectContext, frozen: bool) -> Result<()> {
    let code = install_pms(ctx, frozen)?;
    if code != 0 {
        bail!("install failed (exit {code})");
    }
    Ok(())
}

/// Chain-aware install entry — runs install across every detected PM
/// and returns the first failing PM's exit code, or 0 if all succeed.
///
/// Used by `chain::exec` when `ChainItemKind::Install` appears as a
/// chain item (i.e. `runner install <tasks>`).
pub(crate) fn install_pms(ctx: &ProjectContext, frozen: bool) -> Result<i32> {
    if ctx.package_managers.is_empty() {
        bail!("No package manager detected.");
    }

    if let (Some(nv), Some(cur)) = (&ctx.node_version, &ctx.current_node)
        && !version_matches(&nv.expected, cur)
    {
        eprintln!(
            "{} node expected {} ({}), current {}",
            "warn:".yellow().bold(),
            nv.expected,
            nv.source,
            cur,
        );
        suggest_version_switch(ctx);
    }

    let mut first_failure: Option<i32> = None;
    for pm in &ctx.package_managers {
        eprintln!("{} {}", "installing with".dimmed(), pm.label().bold());
        let mut cmd = build_install_command(ctx, *pm, frozen);
        super::configure_command(&mut cmd, &ctx.root);
        let status = cmd.status()?;
        if !status.success() {
            let code = status.code().unwrap_or(1);
            first_failure.get_or_insert(code);
            return Ok(code);
        }
    }
    Ok(first_failure.unwrap_or(0))
}
```

- [ ] **Step 2: Wire the chain executor's install branch**

In `src/chain/exec.rs`, replace the install stub in `dispatch_item`:

```rust
fn dispatch_item(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    item: &ChainItem,
) -> Result<i32> {
    match &item.kind {
        ChainItemKind::Task(name) => crate::cmd::run::run(ctx, overrides, name, &item.args),
        ChainItemKind::Install => {
            // `runner install` doesn't honor `--frozen` from a chain context;
            // the frozen flag belongs to the top-level subcommand. Default
            // to non-frozen for chain dispatch.
            crate::cmd::install::install_pms(ctx, false)
        }
    }
}
```

> **Question for follow-up:** Should `runner install --frozen build test` propagate `--frozen` into the chain? Spec §6 doesn't address this; v1 leaves it `false` because the chain build-up doesn't carry the flag through. Trivial to add later by extending `ChainItemKind::Install` to carry the `frozen: bool` parameter.

- [ ] **Step 3: Wire the `Command::Install` dispatch**

In the place that matches `Command::Install` (likely `src/lib.rs`), replace with:

```rust
Command::Install { frozen, tasks, keep_going: _, kill_on_fail: _ } => {
    if tasks.is_empty() {
        return crate::cmd::install::install(&ctx, frozen);
    }
    let mut items = vec![crate::chain::ChainItem::install()];
    let task_items = crate::chain::parse::parse_task_list(&tasks)?;
    items.extend(task_items);
    let chain = crate::chain::Chain {
        mode: crate::chain::ChainMode::Sequential,
        items,
        // Failure policy already resolved into overrides; install is
        // always sequential so `kill_on_fail` is silently ignored
        // (per spec §9).
        failure: overrides.failure_policy,
    };
    let code = crate::chain::exec::run_chain(&ctx, &overrides, &chain)?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}
```

- [ ] **Step 4: Verify build**

```bash
cargo build
cargo test --lib
```

Expected: clean build; existing tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/cmd/install.rs src/lib.rs src/chain/exec.rs
git commit -m "$(cat <<'EOF'
wire chain mode into runner install <tasks>

`runner install foo bar` builds a `Chain` with a synthetic install
head followed by `foo` and `bar`, always Sequential. The chain
executor's `ChainItemKind::Install` branch now calls the extracted
`install_pms(ctx, frozen)` helper, which mirrors today's per-PM
install loop but returns an exit code instead of bailing.

`install()` becomes a thin wrapper that bails on non-zero so
existing single-install callers keep their `Result<()>` shape.

The CLI-level `-k` / `--kill-on-fail` flags route into the resolver's
`failure_policy` so env/config layering applies the same way for
install chains and run chains.
EOF
)"
```

---

## Task 14: Warning dedup across chain

Implements spec §4.3. Both modes accumulate resolver warnings into a `HashSet<DetectionWarning>` and emit them once.

The simplest implementation: track an `Arc<Mutex<HashSet<DetectionWarning>>>` shared across per-task `crate::cmd::run::run` calls. But `cmd::run::run` currently emits warnings directly via `super::print_warnings(ctx, overrides)`. So either:

1. Refactor `print_warnings` to accept an optional "warning sink" (`&mut HashSet<DetectionWarning>`) instead of printing directly; the chain executor uses the sink and emits the deduped set at the end. Single-task callers print directly.
2. Suppress warnings inside per-task `cmd::run::run` calls when invoked from the chain executor (via a new `overrides.no_warnings` toggle), and have the chain executor do its own resolution + warning emission.

Option 1 is cleaner. Option 2 doubles up resolver work.

**Files:**

- Modify: `src/cmd/mod.rs` (where `print_warnings` lives)
- Modify: `src/cmd/run.rs` (suppress prints when sink is provided)
- Modify: `src/chain/exec.rs` (collect into sink, emit at the end)

> This task is the most invasive of the lot because warning emission threads through multiple layers. If subagent-driven execution finds the refactor balloons, defer warning dedup to a v1.1 follow-up — spec §4.3 calls it out as an optional polish item. The chain WORKS without dedup; you just see repeated warnings.

- [ ] **Step 1: Sketch the sink API**

Add to `src/cmd/mod.rs`:

```rust
/// Warning sink for chain dispatch. `None` means emit warnings to
/// stderr immediately (single-task path); `Some(set)` means stash for
/// deduped emission at the end of the chain.
pub(crate) type WarningSink<'a> =
    Option<&'a mut std::collections::HashSet<crate::types::DetectionWarning>>;
```

Refactor `print_warnings` and `print_warning_slice` to take a `sink: WarningSink<'_>`:

```rust
pub(crate) fn print_warnings_or_collect(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    sink: WarningSink<'_>,
) {
    // ... existing body, but when sink.is_some(), insert into the set
    //     instead of printing.
}
```

- [ ] **Step 2: Thread the sink through `cmd::run::run`**

Add a parameter to `cmd::run::run`:

```rust
pub(crate) fn run(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    task: &str,
    args: &[String],
    sink: WarningSink<'_>,
) -> Result<i32>
```

Update every call site to pass `None`. The chain executor passes `Some(&mut shared_sink)`.

- [ ] **Step 3: Update chain executor to use the sink**

In `chain::exec::run_chain`:

```rust
let mut sink = std::collections::HashSet::new();
// ... existing dispatch loop ...
// After all tasks done:
emit_deduped_warnings(&sink, overrides);
```

Where `emit_deduped_warnings` writes each unique warning once to stderr (using the existing `Display` impl).

- [ ] **Step 4: Tests**

Add a test that drives a chain with two tasks both producing the same `PathProbeFallback` warning, verifying that the output stream contains the warning exactly once.

This is best as an integration test (Task 15) since it requires real resolver dispatch.

- [ ] **Step 5: Commit**

```bash
git add src/cmd/mod.rs src/cmd/run.rs src/chain/exec.rs
git commit -m "$(cat <<'EOF'
dedup resolver warnings across chain

Chain dispatch resolves each task independently — without dedup,
the same `PathProbeFallback` / `PmMismatch` warning surfaces N
times. Now the chain executor passes a `HashSet<DetectionWarning>`
sink through `cmd::run::run`; `print_warnings_or_collect` adds to
the set instead of stderr when a sink is present. The chain emits
the deduped set once at the end.

`DetectionWarning` derived `Hash + Eq` in Task 1.
EOF
)"
```

---

## Task 15: Integration tests + smoke

Implements spec §12. Builds out fixture-based integration tests in `tests/`.

**Files:**

- Create: `tests/chain.rs` (or extend an existing `tests/*.rs`)
- Create: `tests/fixtures/chain-sequential/` (project with multiple tasks)
- Create: `tests/fixtures/chain-parallel-failfast/` (task that fails mid-stream)
- Create: `tests/fixtures/install-then-tasks/` (project with a `package.json`)

- [ ] **Step 1: Inspect existing integration test layout**

```bash
ls tests/
find tests -maxdepth 2 -name "*.rs" -o -name "Cargo.toml"
```

If `tests/` already has fixture-based integration tests, follow that pattern. Otherwise, the typical Rust layout is `tests/<name>.rs` for the test binary, with fixture directories beside it.

- [ ] **Step 2: Build the sequential fixture**

Create `tests/fixtures/chain-sequential/justfile`:

```just
build:
    @echo "build ran"

test:
    @echo "test ran"

lint:
    @echo "lint ran"
```

- [ ] **Step 3: Write the sequential test**

```rust
#[test]
fn chain_sequential_runs_in_order() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_runner"))
        .args([
            "--dir",
            "tests/fixtures/chain-sequential",
            "run",
            "-s",
            "build",
            "test",
            "lint",
        ])
        .output()
        .expect("runner binary spawns");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let b = stdout.find("build ran").expect("build ran in output");
    let t = stdout.find("test ran").expect("test ran in output");
    let l = stdout.find("lint ran").expect("lint ran in output");
    assert!(b < t && t < l, "order should match -s arg order");
}
```

- [ ] **Step 4: Add parallel + failure tests**

Similar pattern. Use a `sleep` + `exit N` fixture for parallel-failfast.

- [ ] **Step 5: Smoke in this repo**

```bash
cargo build
target/debug/runner --dir tests/fixtures/chain-sequential run -s build test lint
target/debug/runner --dir tests/fixtures/install-then-tasks install build
NO_COLOR=1 target/debug/runner --dir tests/fixtures/chain-sequential run -p build test lint
```

Verify by eye:

- Sequential output is plain (no prefix).
- Install + chain dispatches both phases.
- Parallel with `NO_COLOR` shows `[<name>]` prefixes but no ANSI.

- [ ] **Step 6: Commit**

```bash
git add tests/
git commit -m "$(cat <<'EOF'
add integration tests for chain modes

Fixture projects in `tests/fixtures/`:

- `chain-sequential/`: justfile with build/test/lint targets,
  verifies `-s` runs them in declared order.
- `chain-parallel-failfast/`: tasks with controlled exit codes,
  verifies `-p` exit code = first failure + others finish.
- `install-then-tasks/`: package.json with install + scripts,
  verifies install runs before tasks.

Smoke commands documented in the task notes for manual verification.
EOF
)"
```

---

## Task 16: Schema regen + final lint pass

Regenerates `schemas/runner.toml.schema.json` so the new `[chain]` section appears, runs `cargo clippy`, and confirms `cargo test --workspace` is green.

**Files:**

- Modify: `schemas/runner.toml.schema.json` (auto-generated)

- [ ] **Step 1: Regenerate the schema**

```bash
just gen-schema
```

Expected: file modified to include `ChainSection` block + `chain` property under `RunnerConfig.properties`.

- [ ] **Step 2: Verify schema content**

```bash
jq '.properties.chain, ."$defs".ChainSection' schemas/runner.toml.schema.json
```

Expected: both keys present, with `keep_going` / `kill_on_fail` enums or `["boolean", "null"]` types.

- [ ] **Step 3: Run the regression gates**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git diff --exit-code schemas/
```

If `git diff` on `schemas/` returns non-empty (it will — we just changed it), commit it.

If `cargo clippy` complains, fix without `#[allow]` per project convention.

If any `#[allow(dead_code)]` slipped in during implementation, remove it now:

```bash
rg "#\[allow\(dead_code"
```

Expected: zero matches.

- [ ] **Step 4: Commit**

```bash
git add schemas/runner.toml.schema.json
git commit -m "$(cat <<'EOF'
regenerate JSON schema with [chain] section

`schemars` auto-emits `ChainSection.keep_going` / `kill_on_fail` as
`["boolean", "null"]` (from the `Option<bool>` typing) under the new
`chain` property on `RunnerConfig`. Doc comments flow through as
JSON Schema descriptions.

Schema regenerated via `just gen-schema`. CI drift gate
(`just gen-schema && git diff --exit-code schemas/`) is in sync
after this commit.
EOF
)"
```

- [ ] **Step 5: Final verification**

```bash
git --no-pager log --oneline 3f5eb3d..HEAD
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: ~16 commits since `v0.9.0`, all tests pass, no clippy warnings, no `#[allow(dead_code)]` matches.

---

## Type / API consistency check

A quick scan to catch drift between tasks:

- `ChainItem::display_name()` — defined in Task 2, used in Task 9 (`prefix_width(&names)`) and Task 11 (parallel exec). Name stays `display_name` throughout.
- `FailurePolicy::FailFast` (default), `KeepGoing`, `KillOnFail` — defined in Task 2, used in Tasks 4 (resolver), 10 (sequential exec), 11 (parallel exec), 13 (install). Variant names stay identical throughout.
- `crate::chain::exec::run_chain(ctx, overrides, chain) -> Result<i32>` — signature defined in Task 10, called from Tasks 12 and 13. Both call sites pass `&Chain`, not `Chain`.
- `crate::cmd::install::install_pms(ctx, frozen) -> Result<i32>` — defined in Task 13, called from Task 13's `dispatch_item`. Same signature both ends.
- `crate::cmd::run::dispatch_task_piped(ctx, overrides, task, args) -> Result<Child>` — extracted in Task 11 step 1, called from Task 11's `run_parallel`. Confirm the same signature when refactoring `cmd::run::run`.
- `crate::cmd::WarningSink<'a>` — defined in Task 14, threaded through `cmd::run::run` and `cmd::install::install_pms`. The latter doesn't have a sink today; if Task 14 needs it on install too, extend the signature there as well.
- `ResolveError::ConflictingFailurePolicy { source: &'static str }` — defined in Task 5, raised from Task 4's `resolve_failure_policy`. Field name stays `source`.

---

## Self-review notes

**Spec coverage:** Every section of `docs/superpowers/specs/2026-05-13-task-chaining-design.md` maps to a task. §4.3 (warning dedup) is gated as a defer-able polish in Task 14.

**Placeholder scan:** No `TBD` / `TODO` / "implement later". Task 11 has implementation notes about kill-on-fail semantics (SIGKILL vs SIGTERM) called out explicitly; v1 deliberately uses `Child::kill()` (SIGKILL on Unix) because adding a `libc`/`nix` dep just for SIGTERM is YAGNI for v1.

**Type consistency:** Verified above. The most fragile pair is `run_chain` / `dispatch_item` / `cmd::run::run` / `install_pms` — all `Result<i32>` returns, all `&ProjectContext` + `&ResolutionOverrides` args.

**Scope:** 16 tasks, mostly 4–6 steps each. Largest is Task 11 (parallel executor). Smallest is Task 1 (Hash + Eq derives). All produce independently testable / committable work.

---
