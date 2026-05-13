//! Chain executor. Sequential mode inherits parent stdio. Parallel
//! mode pipes per-task stdio through the prefix multiplexer in
//! `chain::mux` (Task 11).

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
    anyhow::bail!("parallel chain execution not yet implemented")
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
