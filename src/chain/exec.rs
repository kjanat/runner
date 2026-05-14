//! Chain executor. Sequential mode inherits parent stdio. Parallel
//! mode pipes per-task stdio through the prefix multiplexer in
//! `chain::mux` (Task 11).

use std::collections::HashSet;

use anyhow::Result;

use crate::chain::{Chain, ChainItem, ChainItemKind, ChainMode, FailurePolicy};
use crate::resolver::ResolutionOverrides;
use crate::types::{DetectionWarning, ProjectContext};

/// Dispatch a chain. Returns the first-observed failing task's exit
/// code, or 0 if every task succeeded.
///
/// "First-observed" means first in *detection* order, not necessarily
/// first by wall-clock completion: sequential mode short-circuits on
/// the first non-zero exit (so detection order == completion order);
/// parallel mode polls children every 50ms and records the first
/// non-zero code seen during a poll window. When multiple parallel
/// siblings finish inside the same 50ms window, the recorded code
/// follows `remaining` iteration (i.e. spawn) order. True
/// completion-time ordering would need an OS-level termination
/// timestamp (waitpid + rusage on Linux) which the std crate doesn't
/// surface — out of scope for v1.
///
/// Per-task resolver warnings are collected into a shared `HashSet`
/// so the user sees each unique warning once, not N times.
pub(crate) fn run_chain(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    chain: &Chain,
) -> Result<i32> {
    let mut warnings: HashSet<DetectionWarning> = HashSet::new();
    // Emit warnings on both success and error paths — a chain that
    // crashes halfway through should still surface the resolver
    // warnings it accumulated, not swallow them with the error.
    let result = match chain.mode {
        ChainMode::Sequential => run_sequential(ctx, overrides, chain, &mut warnings),
        ChainMode::Parallel => run_parallel(ctx, overrides, chain, &mut warnings),
    };
    crate::cmd::emit_collected_warnings(&warnings, overrides);
    result
}

fn run_sequential(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    chain: &Chain,
    warnings: &mut HashSet<DetectionWarning>,
) -> Result<i32> {
    let keep_going = matches!(chain.failure, FailurePolicy::KeepGoing);
    let mut first_failure: Option<i32> = None;

    for item in &chain.items {
        let code = dispatch_item(ctx, overrides, item, warnings)?;
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
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    chain: &Chain,
    warnings: &mut HashSet<DetectionWarning>,
) -> Result<i32> {
    use std::process::Child;
    use std::sync::Arc;

    use crate::chain::mux::{LineSink, StdioSink, prefix_width, render_prefix, spawn_readers};

    let names: Vec<&str> = chain.items.iter().map(ChainItem::display_name).collect();
    let width = prefix_width(&names);
    let colorize = colored::control::SHOULD_COLORIZE.should_colorize();

    // Synchronous sink: each reader thread writes lines directly to
    // stdout/stderr, acquiring the underlying lock per line. The old
    // design ran a dedicated writer thread that held the stdio locks
    // across an mpsc drain, which deadlocked against `eprintln!` calls
    // on the main thread (the `→ <source> <task>` arrow inside
    // `dispatch_task_piped`). A sink keeps every emit point on the
    // caller's thread and bounds lock duration to one `writeln!`.
    let sink: Arc<dyn LineSink> = Arc::new(StdioSink);

    // Spawn each task with piped stdio and start reader threads.
    let mut children: Vec<(String, Child)> = Vec::with_capacity(chain.items.len());
    let mut reader_handles = Vec::new();

    // Spawn loop. On any per-item failure (resolver error or the Install
    // bail-out below), already-spawned children would otherwise outlive
    // this function — `std::process::Child::drop` does NOT kill the
    // process. Cleanup explicitly: kill + reap accumulated children,
    // then join readers (their pipes close once the children are
    // reaped, so the threads exit on their own).
    let spawn_outcome: Result<()> = (|| {
        for item in &chain.items {
            let prefix = render_prefix(item.display_name(), width, colorize);
            let mut child = match &item.kind {
                ChainItemKind::Task(name) => crate::cmd::run::dispatch_task_piped(
                    ctx,
                    overrides,
                    name,
                    &item.args,
                    Some(warnings),
                )?,
                ChainItemKind::Install { .. } => {
                    // Install is always Sequential in v1 (CLI rejects `-p` on
                    // `runner install`); reaching here would mean a synthetic
                    // Parallel chain was constructed elsewhere — bail loudly.
                    anyhow::bail!("install items cannot run in parallel chains")
                }
            };
            let stdout: Box<dyn std::io::Read + Send> =
                Box::new(child.stdout.take().expect("stdout piped"));
            let stderr: Box<dyn std::io::Read + Send> =
                Box::new(child.stderr.take().expect("stderr piped"));
            reader_handles.extend(spawn_readers(
                vec![
                    (prefix.clone(), false, stdout),
                    (prefix.clone(), true, stderr),
                ],
                &sink,
            ));
            children.push((item.display_name().to_string(), child));
        }
        Ok(())
    })();
    if let Err(e) = spawn_outcome {
        for (_, mut c) in children {
            let _ = c.kill();
            let _ = c.wait();
        }
        for h in reader_handles {
            let _ = h.join();
        }
        return Err(e);
    }

    // Poll children. On first failure with KillOnFail, kill remaining
    // siblings; otherwise let them finish naturally.
    let mut remaining: Vec<(String, Child)> = children;
    let mut first_failure: Option<i32> = None;
    let kill_on_fail = matches!(chain.failure, FailurePolicy::KillOnFail);

    while !remaining.is_empty() {
        let mut next: Vec<(String, Child)> = Vec::with_capacity(remaining.len());
        for (name, mut child) in std::mem::take(&mut remaining) {
            match child.try_wait()? {
                Some(status) => {
                    let code = crate::cmd::exit_code(status);
                    if code != 0 {
                        first_failure.get_or_insert(code);
                    }
                }
                None => {
                    if kill_on_fail && first_failure.is_some() {
                        let _ = child.kill();
                        // Wait so stdio drains fully.
                        let _ = child.wait();
                    } else {
                        next.push((name, child));
                    }
                }
            }
        }
        remaining = next;
        if !remaining.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    // Readers exit when child stdio closes; join so any in-flight
    // `sink.emit` finishes before we return.
    for h in reader_handles {
        let _ = h.join();
    }

    Ok(first_failure.unwrap_or(0))
}

fn dispatch_item(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    item: &ChainItem,
    warnings: &mut HashSet<DetectionWarning>,
) -> Result<i32> {
    match &item.kind {
        ChainItemKind::Task(name) => {
            // v1 ChainItem.args is always empty; v2 will populate it.
            crate::cmd::run::run(ctx, overrides, name, &item.args, Some(warnings))
        }
        ChainItemKind::Install { frozen } => {
            crate::cmd::install::install_pms(ctx, *frozen, Some(warnings))
        }
    }
}
