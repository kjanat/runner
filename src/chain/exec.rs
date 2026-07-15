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
/// "First-observed" is detection order, not wall-clock completion:
/// sequential mode short-circuits on the first non-zero exit; parallel
/// mode polls children and records the first non-zero code seen, with
/// ties within a poll window broken by spawn order.
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
    // typo at item 3. `precheck_task` is side-effect-free, no
    // warnings emitted, no arrows printed, no subprocess spawned,
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

    // Emit warnings on both success and error paths: a chain that
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
        let started = std::time::Instant::now();
        let code = dispatch_item(ctx, overrides, item, warnings)?;
        crate::cmd::emit_task_timing(overrides, item.display_name(), started.elapsed(), code);
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
        // Suppress per-task groups when a parent runner already opened one:
        // GHA groups don't nest, so fall back to the live prefix muxer (which
        // also renders any child group markers inert via the line prefix).
        overrides.group_output && overrides.github_group_parallel && !overrides.parent_group_open
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
    use std::time::Instant;

    use crate::chain::mux::{LineSink, StdioSink, prefix_width, render_prefix, spawn_readers};

    let names: Vec<&str> = chain.items.iter().map(ChainItem::display_name).collect();
    let width = prefix_width(&names);
    let colorize = colored::control::SHOULD_COLORIZE.should_colorize();

    // Synchronous sink: each reader thread writes lines directly to
    // stdout/stderr, taking the lock per line. Bounding lock duration to
    // one `writeln!` avoids deadlocking against `eprintln!` on the main
    // thread (the `→ <source> <task>` arrow in `dispatch_task_piped`).
    let sink: Arc<dyn LineSink> = Arc::new(StdioSink);

    // Spawn each task with piped stdio and start reader threads. The
    // `Instant` recorded at spawn anchors the per-task wall-clock duration
    // reported when the child is reaped.
    let mut children: Vec<(String, Instant, Child)> = Vec::with_capacity(chain.items.len());
    let mut reader_handles = Vec::new();

    // Spawn loop. On any per-item failure (resolver error or the Install
    // bail-out below), already-spawned children would otherwise outlive
    // this function because `std::process::Child::drop` does NOT kill the
    // process. Cleanup explicitly: kill + reap accumulated children,
    // then join readers (their pipes close once the children are
    // reaped, so the threads exit on their own).
    let spawn_outcome: Result<()> = (|| {
        for item in &chain.items {
            let prefix = render_prefix(item.display_name(), width, colorize);
            let started = Instant::now();
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
                    // Parallel chain was constructed elsewhere, so bail loudly.
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
            children.push((item.display_name().to_string(), started, child));
        }
        Ok(())
    })();
    if let Err(e) = spawn_outcome {
        kill_and_reap(children);
        // Bounded drain, same as the poll paths: an already-spawned task
        // may have left a descendant holding the pipe open, and an
        // unbounded join would block this error return on it.
        wait_for_readers(&mut reader_handles, READER_DRAIN_GRACE);
        return Err(e);
    }

    // Poll children. On first failure with KillOnFail, kill remaining
    // siblings; otherwise let them finish naturally.
    let mut remaining: Vec<(String, Instant, Child)> = children;
    let mut first_failure: Option<i32> = None;
    let kill_on_fail = matches!(chain.failure, FailurePolicy::KillOnFail);

    while !remaining.is_empty() {
        let mut next: Vec<(String, Instant, Child)> = Vec::with_capacity(remaining.len());
        // A `try_wait` error must not orphan the siblings: `Child::drop`
        // does not kill, so bail out through the same kill + reap cleanup
        // the spawn phase uses instead of `?`-ing mid-iteration.
        let mut poll_error: Option<anyhow::Error> = None;
        let mut pending = std::mem::take(&mut remaining).into_iter();
        for (name, started, mut child) in pending.by_ref() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let code = crate::cmd::exit_code(status);
                    crate::cmd::emit_task_timing(overrides, &name, started.elapsed(), code);
                    if code != 0 {
                        first_failure.get_or_insert(code);
                    }
                }
                Ok(None) => {
                    if kill_on_fail && first_failure.is_some() {
                        let _ = child.kill();
                        // Wait so stdio drains fully; a killed sibling still
                        // reports timing for the work it managed before SIGKILL.
                        if let Ok(status) = child.wait() {
                            crate::cmd::emit_task_timing(
                                overrides,
                                &name,
                                started.elapsed(),
                                crate::cmd::exit_code(status),
                            );
                        }
                    } else {
                        next.push((name, started, child));
                    }
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    poll_error = Some(e.into());
                    break;
                }
            }
        }
        if let Some(e) = poll_error {
            kill_and_reap(next.into_iter().chain(pending));
            wait_for_readers(&mut reader_handles, READER_DRAIN_GRACE);
            return Err(e);
        }
        remaining = next;
        if !remaining.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    // Bounded drain, not an unbounded join: a reader only EOFs once every
    // write end of its pipe closes, and a task can leave a backgrounded
    // descendant holding the inherited fd open after the direct child is
    // reaped. The grouped path already guards this (see
    // `flush_task_group`); without the bound, `run -p` hangs forever on
    // such a task. Unfinished readers are abandoned after the grace.
    wait_for_readers(&mut reader_handles, READER_DRAIN_GRACE);

    Ok(first_failure.unwrap_or(0))
}

/// Per-task state for grouped parallel execution: the child, the
/// sink its reader threads append into, those reader handles, and the
/// spawn `Instant` anchoring the duration folded into the block footer.
struct GroupedTask {
    name: String,
    started: std::time::Instant,
    child: std::process::Child,
    sink: std::sync::Arc<crate::chain::mux::BufferSink>,
    readers: Vec<std::thread::JoinHandle<()>>,
}

const READER_DRAIN_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

/// Parallel execution that buffers each task's output and displays it as one
/// contiguous block the moment that task finishes (completion order, first
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
            let started = std::time::Instant::now();
            let (name, mut child, sink) = match &item.kind {
                ChainItemKind::Task(task_name) => {
                    let sink = Arc::new(BufferSink::new()?);
                    let child = crate::cmd::run::dispatch_task_piped(
                        ctx,
                        overrides,
                        task_name,
                        &item.args,
                        Some(warnings),
                    )?;
                    (item.display_name().to_string(), child, sink)
                }
                ChainItemKind::Install { .. } => {
                    anyhow::bail!("install items cannot run in parallel chains")
                }
            };
            let stdout: Box<dyn std::io::Read + Send> =
                Box::new(child.stdout.take().expect("stdout piped"));
            let stderr: Box<dyn std::io::Read + Send> =
                Box::new(child.stderr.take().expect("stderr piped"));
            // `.clone()` resolves on the concrete `Arc<BufferSink>` then
            // unsizes to the trait object; `Arc::clone(&sink)` would instead
            // infer its generic from the annotation and fail to coerce.
            let dyn_sink: Arc<dyn LineSink> = sink.clone();
            // No prefix: the group title identifies the task, while the sink
            // preserves stdout/stderr identity for replay.
            let readers = spawn_readers(
                vec![
                    (String::new(), false, stdout),
                    (String::new(), true, stderr),
                ],
                &dyn_sink,
            );
            tasks.push(GroupedTask {
                name,
                started,
                child,
                sink,
                readers,
            });
        }
        Ok(())
    })();
    if let Err(e) = spawn_outcome {
        for t in tasks {
            cleanup_grouped_task(t);
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
        // A `try_wait` error must not orphan the siblings: `Child::drop`
        // does not kill, so bail out through the same kill + reap + drain
        // cleanup the spawn phase uses instead of `?`-ing mid-iteration.
        let mut poll_error: Option<anyhow::Error> = None;
        let mut pending = std::mem::take(&mut remaining).into_iter();
        for mut t in pending.by_ref() {
            match t.child.try_wait() {
                Ok(Some(status)) => {
                    let code = crate::cmd::exit_code(status);
                    if code != 0 {
                        first_failure.get_or_insert(code);
                    }
                    let footer = timing_footer(overrides, t.started.elapsed(), code);
                    flush_grouped_task(t, gha_syntax, colorize, footer.as_deref());
                }
                Ok(None) => {
                    if kill_on_fail && first_failure.is_some() {
                        let _ = t.child.kill();
                        // A killed sibling still reports timing for the work it
                        // completed before SIGKILL.
                        let elapsed = t.started.elapsed();
                        let footer = t.child.wait().ok().and_then(|status| {
                            timing_footer(overrides, elapsed, crate::cmd::exit_code(status))
                        });
                        flush_grouped_task(t, gha_syntax, colorize, footer.as_deref());
                    } else {
                        next.push(t);
                    }
                }
                Err(e) => {
                    cleanup_grouped_task(t);
                    poll_error = Some(e.into());
                    break;
                }
            }
        }
        if let Some(e) = poll_error {
            for t in next.into_iter().chain(pending) {
                cleanup_grouped_task(t);
            }
            return Err(e);
        }
        remaining = next;
        if !remaining.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    Ok(first_failure.unwrap_or(0))
}

/// Flush a completed grouped task's block, moving its reader handles into
/// [`flush_task_group`]. Thin wrapper that keeps the supervisor poll loop
/// under the per-function line budget by hiding the field-destructuring at
/// the two completion sites (normal exit and SIGKILL).
fn flush_grouped_task(task: GroupedTask, gha_syntax: bool, colorize: bool, footer: Option<&str>) {
    flush_task_group(
        &task.name,
        gha_syntax,
        colorize,
        &task.sink,
        task.readers,
        footer,
    );
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
    timing_footer: Option<&str>,
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
        // Neutralize child group/endgroup commands so they can't nest in or
        // close our `runner: <name>` group early.
        let _ = sink.replay_to(&mut stdout, &mut stderr, true);
        // Footer goes inside the group, before the guard's Drop emits
        // `::endgroup::`, so the duration stays attached to the block.
        write_timing_footer(timing_footer, colorize);
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
        // Plain-header (non-Actions) replay: no `::group::` interpretation
        // happens here, so leave the child's bytes untouched.
        let _ = sink.replay_to(&mut stdout, &mut stderr, false);
        // Footer closes the plain `runner: <name>` block with its duration.
        write_timing_footer(timing_footer, colorize);
    }
}

/// Compute the grouped-mode block footer (`finished in 1.2s (exit 0)`) when
/// per-task timing is enabled, or `None` when muted via `--quiet` /
/// `--no-warnings`. Shared by both grouped completion paths so the gating
/// stays in one place.
fn timing_footer(
    overrides: &ResolutionOverrides,
    elapsed: std::time::Duration,
    code: i32,
) -> Option<String> {
    crate::cmd::timing_enabled(overrides).then(|| crate::cmd::task_timing_summary(elapsed, code))
}

/// Write a grouped-task block footer to stdout, dimmed when colorizing.
/// `None` is a no-op so callers can pass the gated footer through unchanged.
fn write_timing_footer(footer: Option<&str>, colorize: bool) {
    use std::io::Write as _;

    let Some(footer) = footer else { return };
    let line = if colorize {
        use colored::Colorize as _;
        footer.dimmed().to_string()
    } else {
        footer.to_string()
    };
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// Kill + reap streaming-chain children that must not outlive an error
/// return: `Child::drop` does not kill, so every early exit routes
/// through here.
fn kill_and_reap<I: IntoIterator<Item = (String, std::time::Instant, std::process::Child)>>(
    children: I,
) {
    for (_, _, mut c) in children {
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// Kill + reap a grouped task and drain its readers with the bounded
/// grace, the cleanup every grouped-chain error path shares.
fn cleanup_grouped_task(mut t: GroupedTask) {
    let _ = t.child.kill();
    let _ = t.child.wait();
    t.sink.close();
    wait_for_readers(&mut t.readers, READER_DRAIN_GRACE);
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
