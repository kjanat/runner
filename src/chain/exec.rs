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
    use std::sync::mpsc::channel;

    use crate::chain::mux::{PrefixedLine, prefix_width, render_prefix, spawn_readers};

    let names: Vec<&str> = chain.items.iter().map(ChainItem::display_name).collect();
    let width = prefix_width(&names);
    let colorize = colored::control::SHOULD_COLORIZE.should_colorize();

    let (tx, rx) = channel::<PrefixedLine>();

    // Writer thread: drain channel, write prefixed lines to parent stdio.
    // Spawned *before* the spawn loop so output flows the moment the
    // first reader pushes a line. Otherwise lines would queue in the
    // unbounded mpsc until the spawn loop finished, defeating the
    // "see progress immediately" point of parallel chains.
    //
    // Locks are acquired *per write* rather than once for the writer's
    // lifetime. Holding stdout/stderr locks across the entire mpsc
    // drain deadlocks against the spawn loop, which calls `eprintln!`
    // (the `→ <source> <task>` arrow inside `dispatch_task_piped`) for
    // each item: the writer parks on `rx`, the main thread parks on
    // `stderr`, and only the first arrow ever surfaces — whichever
    // thread won the startup race for the lock. Re-locking per line
    // costs nothing measurable next to the syscall it gates.
    let writer = std::thread::spawn(move || {
        use std::io::Write;
        for msg in rx {
            if msg.is_stderr {
                let mut stderr = std::io::stderr().lock();
                let _ = writeln!(stderr, "{} {}", msg.prefix, msg.line);
            } else {
                let mut stdout = std::io::stdout().lock();
                let _ = writeln!(stdout, "{} {}", msg.prefix, msg.line);
            }
        }
    });

    // Spawn each task with piped stdio and start reader threads.
    let mut children: Vec<(String, Child)> = Vec::with_capacity(chain.items.len());
    let mut reader_handles = Vec::new();

    // Spawn loop. On any per-item failure (resolver error or the Install
    // bail-out below), already-spawned children would otherwise outlive
    // this function — `std::process::Child::drop` does NOT kill the
    // process. Cleanup explicitly: kill + reap accumulated children,
    // drop the producer end of the channel so reader threads observe
    // EOF, and join the reader handles + writer before propagating
    // the error.
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
                &tx,
            ));
            children.push((item.display_name().to_string(), child));
        }
        Ok(())
    })();
    if let Err(e) = spawn_outcome {
        drop(tx);
        for (_, mut c) in children {
            let _ = c.kill();
            let _ = c.wait();
        }
        for h in reader_handles {
            let _ = h.join();
        }
        let _ = writer.join();
        return Err(e);
    }
    drop(tx); // close producer side so channel closes when all readers finish

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

    // Join readers + writer. Readers exit when child stdio closes.
    for h in reader_handles {
        let _ = h.join();
    }
    let _ = writer.join();

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
