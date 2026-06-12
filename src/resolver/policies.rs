//! Policy parsing — `FallbackPolicy`, `MismatchPolicy`, `FailurePolicy`,
//! plus the `[task_runner].prefer` list and the shared `RUNNER_*` env
//! parsers.
//!
//! Pure string→enum/bool logic; no side effects. Consumed by
//! [`super::overrides::ResolutionOverrides::from_sources`].

use anyhow::{Result, anyhow};

use super::types::{ExplainSource, FallbackPolicy, MismatchPolicy};
use super::{ResolveError, join_labels};
use crate::chain::FailurePolicy;
use crate::config::LoadedConfig;
use crate::types::TaskRunner;

/// Treat any env-var value as truthy unless it's empty, `"0"`, or a
/// case-insensitive variant of `false` / `no` / `off`.
///
/// Surrounding whitespace is stripped first so a trailing newline (the
/// shell-export pattern `RUNNER_EXPLAIN=$VAR \n …`) doesn't accidentally
/// flip an explicit "off" into truthy. Without the case-insensitive
/// compare, `RUNNER_EXPLAIN=FALSE` would silently enable the trace —
/// the opposite of what the user clearly meant.
pub(super) fn is_env_truthy(raw: &str) -> bool {
    let v = raw.trim();
    !v.is_empty()
        && v != "0"
        && !v.eq_ignore_ascii_case("false")
        && !v.eq_ignore_ascii_case("no")
        && !v.eq_ignore_ascii_case("off")
}

pub(super) fn parse_fallback_label(raw: &str) -> Result<FallbackPolicy> {
    match raw {
        "probe" => Ok(FallbackPolicy::Probe),
        "npm" => Ok(FallbackPolicy::Npm),
        "error" => Ok(FallbackPolicy::Error),
        other => Err(anyhow!(
            "unknown fallback policy {other:?}; expected one of probe, npm, error",
        )),
    }
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

pub(super) fn parse_mismatch_label(raw: &str) -> Result<MismatchPolicy> {
    match raw {
        "warn" => Ok(MismatchPolicy::Warn),
        "error" => Ok(MismatchPolicy::Error),
        "ignore" => Ok(MismatchPolicy::Ignore),
        other => Err(anyhow!(
            "unknown on-mismatch policy {other:?}; expected one of warn, error, ignore",
        )),
    }
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
/// `keep_going` and `kill_on_fail` are independent bool layers — CLI flag
/// presence beats env (explicit either polarity) beats `[chain]` config
/// beats `false`. The env layer is presence-authoritative: an explicit
/// `RUNNER_KEEP_GOING=0` overrides `[chain].keep_going = true` in config.
/// The two layers are combined into a `FailurePolicy` and validated for
/// mutual exclusion: both true at any source or after layering returns
/// `ResolveError::ConflictingFailurePolicy`.
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

    let keep = resolve_chain_bool(
        keep_going.cli,
        keep_env,
        config.and_then(|c| c.config.chain.keep_going),
    );
    let kill = resolve_chain_bool(
        kill_on_fail.cli,
        kill_env,
        config.and_then(|c| c.config.chain.kill_on_fail),
    );

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

/// Layered bool resolution: CLI flag > env (explicit either polarity) >
/// config explicit > false. Env's authority is by *presence*, not just
/// truthiness — `Some(false)` from env overrides config.
fn resolve_chain_bool(cli: bool, env: Option<bool>, config: Option<bool>) -> bool {
    if cli {
        return true;
    }
    if let Some(value) = env {
        return value;
    }
    config.unwrap_or(false)
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
