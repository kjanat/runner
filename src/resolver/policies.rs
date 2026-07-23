//! Policy parsing, `FallbackPolicy`, `MismatchPolicy`, `FailurePolicy`,
//! plus the `[task_runner].prefer` list and the shared `RUNNER_*` env
//! parsers.
//!
//! Pure string→enum/bool logic; no side effects. Consumed by
//! [`super::overrides::ResolutionOverrides::from_sources`].

use std::collections::BTreeMap;

use anyhow::{Result, anyhow};

use super::types::{CollisionPolicy, ExplainSource, FallbackPolicy, MismatchPolicy, TaskVerbosity};
use super::{ResolveError, join_labels};
use crate::chain::FailurePolicy;
use crate::config::{LoadedConfig, TaskSpec, VerbosityConfig};
use crate::tool::{QuietLevel, Stream};
use crate::types::{JsRuntime, PackageManager, TaskRunner, TaskSource};

/// Treat any env-var value as truthy unless it's empty, `"0"`, or a
/// case-insensitive variant of `false` / `no` / `off`.
///
/// Surrounding whitespace is stripped first so a trailing newline (the
/// shell-export pattern `RUNNER_EXPLAIN=$VAR \n …`) doesn't accidentally
/// flip an explicit "off" into truthy. Without the case-insensitive
/// compare, `RUNNER_EXPLAIN=FALSE` would silently enable the trace,
/// the opposite of what the user clearly meant.
pub(super) fn is_env_truthy(raw: &str) -> bool {
    let v = raw.trim();
    !v.is_empty()
        && !ENV_BOOL_FALSY
            .iter()
            .any(|token| v.eq_ignore_ascii_case(token))
}

/// The boolean env-var token vocabulary, shared by [`is_env_truthy`]
/// (falsy check) and the lenient validator in `overrides.rs`
/// (recognized = falsy ∪ truthy) so the two can't drift: a token one
/// side treats as boolean but the other warns about would be a bug.
pub(super) const ENV_BOOL_FALSY: &[&str] = &["0", "false", "no", "off"];
pub(super) const ENV_BOOL_TRUTHY: &[&str] = &["1", "true", "yes", "on"];

pub(super) fn parse_fallback_label(raw: &str) -> Result<FallbackPolicy> {
    FallbackPolicy::ALL
        .into_iter()
        .find(|policy| policy.label() == raw)
        .ok_or_else(|| {
            anyhow!(
                "unknown fallback policy {raw:?}; expected one of {}",
                join_labels(FallbackPolicy::ALL.iter().map(|p| p.label())),
            )
        })
}

pub(super) fn resolve_fallback_policy(
    cli: Option<&str>,
    env: Option<&str>,
    config: Option<&LoadedConfig>,
) -> Result<FallbackPolicy> {
    // Mirror `parse_override`'s whitespace handling so
    // `RUNNER_FALLBACK=" probe "` and `[resolution].fallback = " npm "`
    // work the same as their stripped forms. Empty/whitespace-only
    // values count as unset and fall through to the next source.
    if let Some(raw) = cli.map(str::trim).filter(|s| !s.is_empty()) {
        return parse_fallback_label(raw);
    }
    if let Some(raw) = env.map(str::trim).filter(|s| !s.is_empty()) {
        return parse_fallback_label(raw);
    }
    if let Some(loaded) = config
        && let Some(raw) = loaded
            .config
            .resolution
            .fallback
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        return parse_fallback_label(raw);
    }
    Ok(FallbackPolicy::default())
}

/// Parse the `[task_runner].prefer` list, validating each entry against
/// the known [`TaskRunner`] variants. Empty/missing → empty `Vec`.
///
/// Per the resolved design decision, an unknown runner name in the
/// prefer-list is a parse error (not a silent skip) so misconfigured
/// entries surface immediately at startup rather than producing
/// surprising selection results at run time.
pub(super) fn parse_prefer_runners(config: Option<&LoadedConfig>) -> Result<Vec<TaskRunner>> {
    let Some(loaded) = config else {
        return Ok(Vec::new());
    };
    let raw = &loaded.config.task_runner.prefer;
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity(raw.len());
    for entry in raw {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let runner = TaskRunner::from_label(trimmed).ok_or_else(|| {
            anyhow!(
                "[task_runner].prefer: unknown runner {trimmed:?}; expected one of {}",
                join_labels(TaskRunner::all().iter().map(|r| r.label())),
            )
        })?;
        out.push(runner);
    }
    Ok(out)
}

/// Resolve a `[tasks]` label to the [`TaskSource`]s it names, most-native
/// first. The label vocabulary is unified across the three kinds a user might
/// reach for, tried in order of richest mapping:
///
/// 1. a **package manager** (`bun`, `npm`, `pnpm`, `yarn`, `deno`, `cargo`,
///    `uv`, …) → its [`PackageManager::owned_task_sources`] (`bun` →
///    `package.json`; `deno` → `deno.json` then `package.json`),
/// 2. a **task runner** (`turbo`, `make`, `just`, `task`, `mise`, `bacon`) →
///    its [`TaskRunner::task_source`] (`nx` resolves to nothing; it has no
///    extractable source, which is recognized but contributes no source),
/// 3. a **source name** (`package.json`, `pyproject.toml`, …) via
///    [`TaskSource::from_label`].
///
/// Returns `Ok(vec)` for a recognized label (possibly empty, e.g. `nx`) and
/// `Err` for an unknown one. PM is tried first so a dual-natured tool like
/// `deno` expands to both its sources rather than just `deno.json`.
fn resolve_source_label(raw: &str) -> Result<Vec<TaskSource>> {
    let label = raw.trim();
    if let Some(pm) = PackageManager::from_label(label) {
        return Ok(pm.owned_task_sources().to_vec());
    }
    if let Some(runner) = TaskRunner::from_label(label) {
        return Ok(runner.task_source().into_iter().collect());
    }
    if let Some(source) = TaskSource::from_label(label) {
        return Ok(vec![source]);
    }
    Err(anyhow!(
        "unknown source {label:?}; expected a task runner ({}), a package manager ({}), or a \
         source name like package.json",
        join_labels(TaskRunner::all().iter().map(|r| r.label())),
        join_labels(
            PackageManager::all()
                .iter()
                .copied()
                .map(PackageManager::label)
        ),
    ))
}

/// Parse `[tasks].prefer` into a deduped, ranked list of [`TaskSource`]s.
/// Empty/missing → empty `Vec`. Rank-only: the list never restricts; it only
/// reorders same-name conflicts (see `cmd::run::select`).
///
/// Unknown labels are a hard error (like the legacy prefer-list) so a typo
/// surfaces at startup rather than silently changing selection.
pub(super) fn parse_tasks_prefer(config: Option<&LoadedConfig>) -> Result<Vec<TaskSource>> {
    let Some(loaded) = config else {
        return Ok(Vec::new());
    };
    let mut out: Vec<TaskSource> = Vec::new();
    for entry in &loaded.config.tasks.prefer {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let sources = resolve_source_label(trimmed).map_err(|e| anyhow!("[tasks].prefer: {e}"))?;
        for source in sources {
            if !out.contains(&source) {
                out.push(source);
            }
        }
    }
    Ok(out)
}

/// Parse per-task source pins (task name → preferred [`TaskSource`]s,
/// most-native first) from both the legacy `[tasks].overrides` map and the
/// `runner` field of a `[tasks.<name>]` entry. Empty/missing → empty map. A
/// label that names no task source (e.g. `nx`) is rejected here: a pin must be
/// actionable, unlike a `prefer` entry which may legitimately rank nothing. A
/// `[tasks.<name>] runner = …` entry wins over a legacy `overrides` pin for the
/// same task.
pub(super) fn parse_tasks_overrides(
    config: Option<&LoadedConfig>,
) -> Result<BTreeMap<String, Vec<TaskSource>>> {
    let Some(loaded) = config else {
        return Ok(BTreeMap::new());
    };
    let mut out = BTreeMap::new();
    for (task, label) in &loaded.config.tasks.overrides {
        let sources =
            resolve_source_label(label).map_err(|e| anyhow!("[tasks.overrides] {task:?}: {e}"))?;
        if sources.is_empty() {
            return Err(anyhow!(
                "[tasks.overrides] {task:?}: {label:?} names no task source to pin to",
            ));
        }
        out.insert(task.clone(), sources);
    }
    // The Cargo-`[dependencies]`-style per-task map. A bare string entry
    // (`build = "turbo"`) or a table's `runner` field pins the source, and
    // supersedes any legacy `overrides` pin for the same task.
    for (task, spec) in &loaded.config.tasks.tasks {
        let label = match spec {
            TaskSpec::Pin(label) => Some(label.as_str()),
            TaskSpec::Settings(settings) => settings.runner.as_deref(),
        };
        let Some(label) = label.map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let sources =
            resolve_source_label(label).map_err(|e| anyhow!("[tasks.{task}] runner: {e}"))?;
        if sources.is_empty() {
            return Err(anyhow!(
                "[tasks.{task}] runner: {label:?} names no task source to pin to",
            ));
        }
        out.insert(task.clone(), sources);
    }
    Ok(out)
}

/// Parse per-task verbosity partials from `[tasks.<name>].verbosity` (the table
/// form only; a bare-string task entry is a source pin, not verbosity). Each
/// partial names zero or more of the two axes; the missing axis is inherited
/// from the global CLI/env level/stream at dispatch. Empty/missing → empty map.
pub(super) fn parse_tasks_verbosity(
    config: Option<&LoadedConfig>,
) -> Result<BTreeMap<String, TaskVerbosity>> {
    let Some(loaded) = config else {
        return Ok(BTreeMap::new());
    };
    let mut out = BTreeMap::new();
    for (task, spec) in &loaded.config.tasks.tasks {
        let TaskSpec::Settings(settings) = spec else {
            continue;
        };
        let Some(verbosity) = &settings.verbosity else {
            continue;
        };
        let (level, stream) = match verbosity {
            VerbosityConfig::Level(raw) => (Some(parse_quiet_level_label(task, raw)?), None),
            VerbosityConfig::Table(table) => {
                let level = table
                    .level
                    .as_deref()
                    .map(|raw| parse_quiet_level_label(task, raw))
                    .transpose()?;
                let stream = table
                    .stream
                    .as_deref()
                    .map(|raw| parse_host_stream_for_task(task, raw))
                    .transpose()?;
                (level, stream)
            }
        };
        if level.is_some() || stream.is_some() {
            out.insert(task.clone(), TaskVerbosity { level, stream });
        }
    }
    Ok(out)
}

/// Parse a `RUNNER_QUIET` env value: a numeric level (`0..3`, saturating) or a
/// truthy/falsy boolean word (truthy → level 1, falsy → level 0). `None` for
/// anything unrecognized so the lenient path can warn.
pub(super) fn parse_quiet_env(raw: &str) -> Option<QuietLevel> {
    let v = raw.trim();
    if v.is_empty() {
        return Some(QuietLevel::Off);
    }
    // Parse wide, then saturate: `from_count` already caps `>= 3` at the floor,
    // so `RUNNER_QUIET=999` resolves to the quietest level rather than falling
    // through to the word checks and reading as invalid.
    if let Ok(n) = v.parse::<u64>() {
        return Some(QuietLevel::from_count(u8::try_from(n).unwrap_or(u8::MAX)));
    }
    if ENV_BOOL_TRUTHY.iter().any(|t| v.eq_ignore_ascii_case(t)) {
        return Some(QuietLevel::Quiet);
    }
    if ENV_BOOL_FALSY.iter().any(|t| v.eq_ignore_ascii_case(t)) {
        return Some(QuietLevel::Off);
    }
    None
}

/// Parse a host-stream label (`inherit` | `stderr`), for `--host-stream` and
/// `RUNNER_HOST_STREAM`.
pub(super) fn parse_host_stream_label(raw: &str) -> Result<Stream> {
    Stream::from_label(raw).ok_or_else(|| {
        anyhow!(
            "unknown host-stream {raw:?}; expected one of {}",
            join_labels(Stream::ALL.iter().map(|s| s.label())),
        )
    })
}

/// Parse a `--runtime` / `RUNNER_RUNTIME` / `[runtime].js` label.
pub(super) fn parse_runtime_label(raw: &str) -> Result<JsRuntime> {
    JsRuntime::from_label(raw).ok_or_else(|| {
        anyhow!(
            "unknown runtime {raw:?}; expected one of {}",
            join_labels(JsRuntime::all().iter().map(|r| r.label())),
        )
    })
}

/// Parse a `[tasks.<name>].verbosity` level label, error-prefixed with the task.
fn parse_quiet_level_label(task: &str, raw: &str) -> Result<QuietLevel> {
    QuietLevel::from_label(raw).ok_or_else(|| {
        anyhow!(
            "[tasks.{task}] verbosity level {raw:?}; expected one of {}",
            join_labels(QuietLevel::ALL.iter().map(|l| l.label())),
        )
    })
}

/// Parse a `[tasks.<name>].verbosity` stream label, error-prefixed with the task.
fn parse_host_stream_for_task(task: &str, raw: &str) -> Result<Stream> {
    parse_host_stream_label(raw).map_err(|e| anyhow!("[tasks.{task}] {e}"))
}

pub(super) fn parse_mismatch_label(raw: &str) -> Result<MismatchPolicy> {
    MismatchPolicy::ALL
        .into_iter()
        .find(|policy| policy.label() == raw)
        .ok_or_else(|| {
            anyhow!(
                "unknown on-mismatch policy {raw:?}; expected one of {}",
                join_labels(MismatchPolicy::ALL.iter().map(|p| p.label())),
            )
        })
}

pub(super) fn parse_collision_label(raw: &str) -> Result<CollisionPolicy> {
    CollisionPolicy::ALL
        .into_iter()
        .find(|policy| policy.label() == raw)
        .ok_or_else(|| {
            anyhow!(
                "unknown on-collision policy {raw:?}; expected one of {}",
                join_labels(CollisionPolicy::ALL.iter().map(|p| p.label())),
            )
        })
}

pub(super) fn resolve_mismatch_policy(
    cli: Option<&str>,
    env: Option<&str>,
    config: Option<&LoadedConfig>,
) -> Result<MismatchPolicy> {
    if let Some(raw) = cli.map(str::trim).filter(|s| !s.is_empty()) {
        return parse_mismatch_label(raw);
    }
    if let Some(raw) = env.map(str::trim).filter(|s| !s.is_empty()) {
        return parse_mismatch_label(raw);
    }
    if let Some(loaded) = config
        && let Some(raw) = loaded
            .config
            .resolution
            .on_mismatch
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        return parse_mismatch_label(raw);
    }
    Ok(MismatchPolicy::default())
}

/// Resolve a chain failure policy from CLI/env/config sources.
///
/// `keep_going` and `kill_on_fail` are independent bool layers: CLI flag
/// presence beats env (explicit either polarity) beats `[chain]` config
/// beats `false`. The env layer is presence-authoritative: an explicit
/// `RUNNER_KEEP_GOING=0` overrides `[chain].keep_going = true` in config.
///
/// Mutual exclusion is checked per layer (both knobs true at one source
/// is a `ResolveError::ConflictingFailurePolicy`), but *across* layers
/// the stronger source wins the whole policy: `-k` on the command line
/// beats a `[chain] kill_on_fail = true` in config rather than
/// colliding with it; otherwise a config-pinned polarity would be
/// uncancellable from the CLI, contradicting CLI > env > config.
pub(super) fn resolve_failure_policy(
    keep_going: ExplainSource<'_>,
    kill_on_fail: ExplainSource<'_>,
    config: Option<&LoadedConfig>,
) -> Result<FailurePolicy> {
    let keep_env = parse_env_bool(keep_going.env);
    let kill_env = parse_env_bool(kill_on_fail.env);

    // Per-source conflict detection: report the source where both went
    // true so the user can pin the offending knob quickly.
    if let Some(source) =
        single_source_conflict(&keep_going, &kill_on_fail, keep_env, kill_env, config)
    {
        return Err(ResolveError::ConflictingFailurePolicy { source }.into());
    }

    let keep = chain_bool_layer(
        keep_going.cli,
        keep_env,
        config.and_then(|c| c.config.chain.keep_going),
    );
    let kill = chain_bool_layer(
        kill_on_fail.cli,
        kill_env,
        config.and_then(|c| c.config.chain.kill_on_fail),
    );

    match (keep, kill) {
        (None, None) => Ok(FailurePolicy::FailFast),
        (Some(_), None) => Ok(FailurePolicy::KeepGoing),
        (None, Some(_)) => Ok(FailurePolicy::KillOnFail),
        (Some(keep_layer), Some(kill_layer)) => match keep_layer.cmp(&kill_layer) {
            std::cmp::Ordering::Greater => Ok(FailurePolicy::KeepGoing),
            std::cmp::Ordering::Less => Ok(FailurePolicy::KillOnFail),
            // Same layer with both true is caught by
            // `single_source_conflict` above; keep the error as a
            // defensive backstop rather than an unreachable panic.
            std::cmp::Ordering::Equal => Err(ResolveError::ConflictingFailurePolicy {
                source: "cross-source",
            }
            .into()),
        },
    }
}

/// Precedence rank of the layers a chain bool can be set on. Order is
/// the override chain: CLI > env > config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ChainBoolLayer {
    Config,
    Env,
    Cli,
}

/// The highest-precedence layer that turns a chain knob ON, or `None`
/// when no layer does. An explicit env falsy (`RUNNER_*=0`) shadows a
/// config `true` below it, presence-authoritative, matching
/// [`parse_env_bool`].
fn chain_bool_layer(cli: bool, env: Option<bool>, config: Option<bool>) -> Option<ChainBoolLayer> {
    if cli {
        return Some(ChainBoolLayer::Cli);
    }
    match env {
        Some(true) => Some(ChainBoolLayer::Env),
        Some(false) => None,
        None => (config == Some(true)).then_some(ChainBoolLayer::Config),
    }
}

/// Parse a chain-bool env var into a tri-state. `None` means the var is
/// unset (or whitespace-only / empty, matching the rest of the resolver's
/// "treat blank env as unset" convention); `Some(true)` for truthy
/// values, `Some(false)` for explicit falsy values (`0`, `false`, `no`,
/// `off`, case-insensitive). This is what lets `RUNNER_KEEP_GOING=0`
/// override a `[chain].keep_going = true` in config.
fn parse_env_bool(env: Option<&str>) -> Option<bool> {
    let raw = env.map(str::trim).filter(|s| !s.is_empty())?;
    Some(is_env_truthy(raw))
}

/// If `keep_going` and `kill_on_fail` are both set true *within the same
/// source layer*, return that layer's label. None if no single-layer
/// conflict (cross-source conflicts are caught after layering).
///
/// The env-layer check uses the parsed `Option<bool>` so an explicit
/// `RUNNER_*=0` neutralises that side: a config-level "[chain] config"
/// conflict only fires when neither env var explicitly disabled its
/// side.
fn single_source_conflict(
    keep: &ExplainSource<'_>,
    kill: &ExplainSource<'_>,
    keep_env: Option<bool>,
    kill_env: Option<bool>,
    config: Option<&LoadedConfig>,
) -> Option<&'static str> {
    if keep.cli && kill.cli {
        return Some("CLI flags");
    }
    if keep_env == Some(true) && kill_env == Some(true) {
        return Some("env vars");
    }
    if let Some(loaded) = config
        && loaded.config.chain.keep_going == Some(true)
        && loaded.config.chain.kill_on_fail == Some(true)
        && keep_env != Some(false)
        && kill_env != Some(false)
    {
        return Some("[chain] config");
    }
    None
}
