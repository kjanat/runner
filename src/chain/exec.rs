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

    // Pre-flight every task token before *any* sibling runs. Catches
    // the common UX trap where `runner run -s bb t lint:cargo` would
    // run `bb` and `t` to completion before bailing on the obvious
    // typo at item 3. `precheck_task` is side-effect-free — no
    // warnings emitted, no arrows printed, no subprocess spawned —
    // and only fires for errors we can determine purely from
    // `ctx.tasks` + the override shape. Errors that need the resolver
    // (PM-exec fallback miss, manifest mismatch) still surface at
    // dispatch time, which is unavoidable without spawning probes
    // here.
    for item in &chain.items {
        if let ChainItemKind::Task(name) = &item.kind {
            crate::cmd::run::precheck_task(ctx, overrides, name)?;
        }
    }

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
    // Whether to buffer each task and print it as one block on completion
    // (first done, first shown) instead of interleaving lines live. Under
    // GitHub Actions, `[github].group_output = false` is the broad opt-out
    // that restores the live muxer; `[github].group_parallel` only controls
    // the parallel grouping feature while grouping is enabled.
    let in_gha = actions_rs::env::is_github_actions();
    let grouped = if in_gha {
        overrides.group_output && overrides.github_group_parallel
    } else {
        overrides.parallel_grouped
    };
    if grouped {
        // `::group::` workflow-command syntax is GitHub-only; elsewhere
        // grouped blocks get plain headers.
        let gha_syntax = in_gha;
        run_parallel_grouped(ctx, overrides, chain, warnings, gha_syntax)
    } else {
        run_parallel_streaming(ctx, overrides, chain, warnings)
    }
}

fn run_parallel_streaming(
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

/// Per-task state for grouped parallel execution: the child, the
/// sink its reader threads append into, and those reader handles.
struct GroupedTask {
    name: String,
    child: std::process::Child,
    sink: std::sync::Arc<crate::chain::mux::BufferSink>,
    readers: Vec<std::thread::JoinHandle<()>>,
}

const READER_DRAIN_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

/// Parallel execution that buffers each task's output and displays it as one
/// contiguous block the moment that task finishes (completion order — first
/// done, first shown). Under GitHub Actions each block is a `::group::`
/// section; elsewhere it gets a plain header. See [`run_parallel`].
fn run_parallel_grouped(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    chain: &Chain,
    warnings: &mut HashSet<DetectionWarning>,
    gha_syntax: bool,
) -> Result<i32> {
    use std::sync::Arc;

    use crate::chain::mux::{BufferSink, LineSink, spawn_readers};

    // Spawn each task with piped stdio + a per-task spooling sink. Same
    // explicit-cleanup contract as the streaming path: a spawn failure must
    // kill + reap already-spawned children and stop accepting reader output.
    let mut tasks: Vec<GroupedTask> = Vec::with_capacity(chain.items.len());
    let spawn_outcome: Result<()> = (|| {
        for item in &chain.items {
            let mut child = match &item.kind {
                ChainItemKind::Task(name) => crate::cmd::run::dispatch_task_piped(
                    ctx,
                    overrides,
                    name,
                    &item.args,
                    Some(warnings),
                )?,
                ChainItemKind::Install { .. } => {
                    anyhow::bail!("install items cannot run in parallel chains")
                }
            };
            let stdout: Box<dyn std::io::Read + Send> =
                Box::new(child.stdout.take().expect("stdout piped"));
            let stderr: Box<dyn std::io::Read + Send> =
                Box::new(child.stderr.take().expect("stderr piped"));
            let sink = Arc::new(BufferSink::new()?);
            // `.clone()` resolves on the concrete `Arc<BufferSink>` then
            // unsizes to the trait object; `Arc::clone(&sink)` would instead
            // infer its generic from the annotation and fail to coerce.
            let dyn_sink: Arc<dyn LineSink> = sink.clone();
            // No prefix — the group title identifies the task, while the sink
            // preserves stdout/stderr identity for replay.
            let readers = spawn_readers(
                vec![
                    (String::new(), false, stdout),
                    (String::new(), true, stderr),
                ],
                &dyn_sink,
            );
            tasks.push(GroupedTask {
                name: item.display_name().to_string(),
                child,
                sink,
                readers,
            });
        }
        Ok(())
    })();
    if let Err(e) = spawn_outcome {
        for mut t in tasks {
            let _ = t.child.kill();
            let _ = t.child.wait();
            t.sink.close();
            wait_for_readers(&mut t.readers, READER_DRAIN_GRACE);
        }
        return Err(e);
    }

    // `gha_syntax` (passed in) selects `::group::` vs a plain header per
    // block; colorize the plain headers only when stdout supports it.
    let colorize = colored::control::SHOULD_COLORIZE.should_colorize();

    // Poll children; flush each task's block the moment it completes, so
    // blocks appear in completion order (first done, first shown). Only this
    // thread writes them, one at a time, so blocks never overlap.
    let mut remaining = tasks;
    let mut first_failure: Option<i32> = None;
    let kill_on_fail = matches!(chain.failure, FailurePolicy::KillOnFail);

    while !remaining.is_empty() {
        let mut next: Vec<GroupedTask> = Vec::with_capacity(remaining.len());
        for mut t in std::mem::take(&mut remaining) {
            match t.child.try_wait()? {
                Some(status) => {
                    let code = crate::cmd::exit_code(status);
                    if code != 0 {
                        first_failure.get_or_insert(code);
                    }
                    flush_task_group(&t.name, gha_syntax, colorize, &t.sink, t.readers);
                }
                None => {
                    if kill_on_fail && first_failure.is_some() {
                        let _ = t.child.kill();
                        let _ = t.child.wait();
                        flush_task_group(&t.name, gha_syntax, colorize, &t.sink, t.readers);
                    } else {
                        next.push(t);
                    }
                }
            }
        }
        remaining = next;
        if !remaining.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    Ok(first_failure.unwrap_or(0))
}

/// Give a finished task's reader threads a bounded chance to drain, then
/// print its spooled output as one contiguous `runner: <name>` block. The
/// bounded drain prevents descendants that inherit stdio from blocking the
/// supervisor loop forever after the direct task process exits.
fn flush_task_group(
    name: &str,
    gha_syntax: bool,
    colorize: bool,
    sink: &crate::chain::mux::BufferSink,
    mut readers: Vec<std::thread::JoinHandle<()>>,
) {
    use std::io::Write as _;

    wait_for_readers(&mut readers, READER_DRAIN_GRACE);
    sink.close();
    join_finished_readers(&mut readers);

    if gha_syntax {
        // GroupGuard writes `::group::` now and `::endgroup::` on drop; don't
        // hold the stdout lock across the guard's Drop.
        let _group = actions_rs::log::group_guard(format!("runner: {name}"));
        let mut stdout = std::io::stdout();
        let mut stderr = std::io::stderr();
        let _ = sink.replay_to(&mut stdout, &mut stderr);
    } else {
        let header = format!("runner: {name}");
        let header = if colorize {
            use colored::Colorize as _;
            header
                .color(crate::chain::mux::color_for(name))
                .bold()
                .to_string()
        } else {
            header
        };
        {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{header}");
            let _ = out.flush();
        }
        let mut stdout = std::io::stdout();
        let mut stderr = std::io::stderr();
        let _ = sink.replay_to(&mut stdout, &mut stderr);
    }
}

fn wait_for_readers(readers: &mut Vec<std::thread::JoinHandle<()>>, grace: std::time::Duration) {
    let deadline = std::time::Instant::now() + grace;
    loop {
        join_finished_readers(readers);
        if readers.is_empty() {
            return;
        }

        let now = std::time::Instant::now();
        if now >= deadline {
            return;
        }
        std::thread::sleep((deadline - now).min(std::time::Duration::from_millis(10)));
    }
}

fn join_finished_readers(readers: &mut Vec<std::thread::JoinHandle<()>>) {
    let mut index = 0;
    while index < readers.len() {
        if readers[index].is_finished() {
            let handle = readers.swap_remove(index);
            let _ = handle.join();
        } else {
            index += 1;
        }
    }
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
            crate::cmd::install::install_pms(ctx, overrides, *frozen, Some(warnings))
        }
    }
}
