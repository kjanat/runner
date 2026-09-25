//! Chain executor. Sequential mode inherits parent stdio. Parallel
//! mode pipes per-task stdio through the prefix multiplexer in
//! `chain::mux` (Task 11).

use std::collections::HashSet;
use std::process::Child;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;

use crate::chain::mux::{BufferSink, LineSink};
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
///
/// A multi-task chain closes with a summary attributing the aggregate exit
/// code to the task that produced it (see [`emit_chain_summary`]).
pub(crate) fn run_chain(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    chain: &Chain,
) -> Result<i32> {
    run_chain_with_head(ctx, overrides, chain, None)
}

/// Run a chain after an imperative prerequisite already completed. Parallel
/// install chains use this because install must finish before task fan-out, but
/// still belongs in failure policy, timing attribution, and the final summary.
pub(crate) fn run_chain_after_completed(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    chain: &Chain,
    name: &str,
    elapsed: std::time::Duration,
    code: i32,
) -> Result<i32> {
    run_chain_with_head(
        ctx,
        overrides,
        chain,
        Some(ItemOutcome {
            name: name.to_string(),
            status: ItemStatus::Ran { code, elapsed },
        }),
    )
}

fn run_chain_with_head(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    chain: &Chain,
    head: Option<ItemOutcome>,
) -> Result<i32> {
    let mut warnings: HashSet<DetectionWarning> = HashSet::new();
    let head_code = head.as_ref().and_then(|outcome| match outcome.status {
        ItemStatus::Ran { code, .. } if code != 0 => Some(code),
        _ => None,
    });
    let mut outcomes: Vec<ItemOutcome> = head.into_iter().collect();

    if let Some(code) = head_code
        && !matches!(chain.failure, FailurePolicy::KeepGoing)
    {
        outcomes.extend(chain.items.iter().map(|item| ItemOutcome {
            name: item.display_name().to_string(),
            status: ItemStatus::Skipped,
        }));
        emit_chain_summary(overrides, &outcomes, code);
        return Ok(code);
    }

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
            crate::commands::run::precheck_task(ctx, overrides, name)?;
        }
    }

    // Emit warnings on both success and error paths: a chain that
    // crashes halfway through should still surface the resolver
    // warnings it accumulated, not swallow them with the error.
    let mode = if overrides.explain {
        ChainMode::Sequential
    } else {
        chain.mode
    };
    let result = match mode {
        ChainMode::Sequential => {
            run_sequential(ctx, overrides, chain, &mut warnings, &mut outcomes)
        }
        ChainMode::Parallel => run_parallel(ctx, overrides, chain, &mut warnings, &mut outcomes),
    };
    crate::commands::emit_collected_warnings(&warnings, overrides);
    match result {
        Ok(task_code) => {
            let code = head_code.unwrap_or(task_code);
            emit_chain_summary(overrides, &outcomes, code);
            Ok(code)
        }
        Err(error) => Err(error),
    }
}

/// What one chain item did, for the end-of-run summary.
struct ItemOutcome {
    name: String,
    status: ItemStatus,
}

enum ItemStatus {
    Ran {
        code: i32,
        elapsed: std::time::Duration,
    },
    /// Never started: an earlier task failed and the policy is fail-fast.
    Skipped,
    /// Got runner's own SIGKILL: a sibling failed under kill-on-fail.
    Killed { elapsed: std::time::Duration },
}

fn run_sequential(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    chain: &Chain,
    warnings: &mut HashSet<DetectionWarning>,
    outcomes: &mut Vec<ItemOutcome>,
) -> Result<i32> {
    let keep_going = matches!(chain.failure, FailurePolicy::KeepGoing);
    let mut first_failure: Option<i32> = None;

    for (index, item) in chain.items.iter().enumerate() {
        let started = Instant::now();
        let (code, key) = dispatch_item(ctx, overrides, item, warnings)?;
        let elapsed = started.elapsed();
        crate::commands::emit_task_timing(overrides, &key, item.display_name(), elapsed, code);
        outcomes.push(ItemOutcome {
            name: item.display_name().to_string(),
            status: ItemStatus::Ran { code, elapsed },
        });
        if code != 0 {
            first_failure.get_or_insert(code);
            if !keep_going {
                outcomes.extend(chain.items[index + 1..].iter().map(|skipped| ItemOutcome {
                    name: skipped.display_name().to_string(),
                    status: ItemStatus::Skipped,
                }));
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
    outcomes: &mut Vec<ItemOutcome>,
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
        overrides.grouping.group_output
            && overrides.grouping.github_group_parallel
            && !overrides.parent.group_open
    } else {
        overrides.grouping.parallel_grouped
    };
    if grouped {
        // `::group::` workflow-command syntax is GitHub-only; elsewhere
        // grouped blocks get plain headers. Both land on stdout, so
        // `--quiet` drops the delimiter and keeps the buffered blocks.
        let style = if !overrides.emits_groups() {
            BlockStyle::Bare
        } else if in_gha {
            BlockStyle::Gha
        } else {
            BlockStyle::Header
        };
        run_parallel_grouped(ctx, overrides, chain, warnings, outcomes, style, in_gha)
    } else {
        run_parallel_streaming(ctx, overrides, chain, warnings, outcomes)
    }
}

/// How a completed parallel task's buffered block is delimited on stdout.
#[derive(Debug, Clone, Copy)]
enum BlockStyle {
    /// A collapsible GitHub Actions `::group::` section.
    Gha,
    /// A plain `runner: <task>` header line.
    Header,
    /// No delimiter, `--quiet` leaves stdout to the tasks themselves.
    Bare,
}

fn run_parallel_streaming(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    chain: &Chain,
    warnings: &mut HashSet<DetectionWarning>,
    outcomes: &mut Vec<ItemOutcome>,
) -> Result<i32> {
    Supervisor::new(overrides, outcomes, Streaming::new(chain)).run(ctx, chain, warnings)
}

trait ParallelOutput {
    type Spool;
    type Task;

    fn spool(&self) -> Result<Self::Spool>;
    fn attach(
        &mut self,
        overrides: &ResolutionOverrides,
        task: SpawnedTask,
        spool: Self::Spool,
    ) -> Self::Task;
    fn job(task: &mut Self::Task) -> &mut Job;
    fn finished(
        &mut self,
        overrides: &ResolutionOverrides,
        outcomes: &mut Vec<ItemOutcome>,
        task: Self::Task,
        code: i32,
    );
    fn kill(
        &mut self,
        overrides: &ResolutionOverrides,
        outcomes: &mut Vec<ItemOutcome>,
        task: Self::Task,
    );
    fn abort(&mut self, task: Self::Task);
    fn drain(&mut self);
}

struct SpawnedTask {
    name: String,
    key: String,
    started: Instant,
    job: Job,
    stdout: Box<dyn std::io::Read + Send>,
    stderr: Box<dyn std::io::Read + Send>,
}

impl SpawnedTask {
    fn child(name: String, key: String, started: Instant, mut child: Child) -> Self {
        Self {
            name,
            key,
            started,
            stdout: Box::new(child.stdout.take().expect("stdout piped")),
            stderr: Box::new(child.stderr.take().expect("stderr piped")),
            job: Job::Child(child),
        }
    }

    fn builtin(
        name: String,
        key: String,
        started: Instant,
        code: i32,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    ) -> Self {
        Self {
            name,
            key,
            started,
            job: Job::Done(code),
            stdout: Box::new(std::io::Cursor::new(stdout)),
            stderr: Box::new(std::io::Cursor::new(stderr)),
        }
    }
}

/// A parallel item's process, or the exit code of a builtin that already ran.
enum Job {
    Child(Child),
    Done(i32),
}

impl Job {
    fn try_wait(&mut self) -> std::io::Result<Option<i32>> {
        match self {
            Self::Child(child) => Ok(child.try_wait()?.map(crate::commands::exit_code)),
            Self::Done(code) => Ok(Some(*code)),
        }
    }

    /// Kill the process and return its exit code when it exited on its own.
    fn kill(&mut self) -> Option<i32> {
        match self {
            Self::Child(child) => {
                let _ = child.kill();
                child.wait().ok().and_then(natural_exit_code)
            }
            Self::Done(code) => Some(*code),
        }
    }

    fn abort(&mut self) {
        if let Self::Child(child) = self {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Supervisor<'a, O> {
    overrides: &'a ResolutionOverrides,
    outcomes: &'a mut Vec<ItemOutcome>,
    output: O,
    first_failure: Option<i32>,
}

impl<'a, O: ParallelOutput> Supervisor<'a, O> {
    const fn new(
        overrides: &'a ResolutionOverrides,
        outcomes: &'a mut Vec<ItemOutcome>,
        output: O,
    ) -> Self {
        Self {
            overrides,
            outcomes,
            output,
            first_failure: None,
        }
    }

    fn run(
        mut self,
        ctx: &ProjectContext,
        chain: &Chain,
        warnings: &mut HashSet<DetectionWarning>,
    ) -> Result<i32> {
        // Spawn loop. On any per-item failure (resolver error or the Install
        // bail-out below), already-spawned children would otherwise outlive
        // this function because `Child::drop` does NOT kill the
        // process. Cleanup explicitly: kill + reap accumulated children,
        // then join readers (their pipes close once the children are
        // reaped, so the threads exit on their own).
        let mut tasks: Vec<O::Task> = Vec::with_capacity(chain.items.len());
        if let Err(e) = self.spawn_all(ctx, chain, warnings, &mut tasks) {
            self.abort(tasks);
            return Err(e);
        }

        // Poll children. On first failure with KillOnFail, kill remaining
        // siblings; otherwise let them finish naturally.
        let kill_on_fail = matches!(chain.failure, FailurePolicy::KillOnFail);
        self.poll(tasks, kill_on_fail)?;
        self.output.drain();

        Ok(self.first_failure.unwrap_or(0))
    }

    fn spawn_all(
        &mut self,
        ctx: &ProjectContext,
        chain: &Chain,
        warnings: &mut HashSet<DetectionWarning>,
        tasks: &mut Vec<O::Task>,
    ) -> Result<()> {
        let mut builtins = Vec::new();
        for item in &chain.items {
            // Spawn each task with piped stdio and start reader threads. The
            // `Instant` recorded at spawn anchors the per-task wall-clock
            // duration reported when the child is reaped.
            let started = Instant::now();
            let name = match &item.kind {
                ChainItemKind::Task(name) => name,
                ChainItemKind::Install { .. } => {
                    // Parallel install is supported with the install head run
                    // first. This executor only handles the parallel tasks
                    // that follow, so an Install item here is invalid.
                    anyhow::bail!("install items cannot run in parallel chains")
                }
            };
            let spool = self.output.spool()?;
            let dispatch = crate::commands::run::dispatch_task_piped(
                ctx,
                self.overrides,
                name,
                &item.args,
                Some(&mut *warnings),
            )?;
            let (child, key) = match dispatch {
                crate::commands::run::PipedDispatch::Child(child, key) => (child, key),
                crate::commands::run::PipedDispatch::Builtin(name) => {
                    builtins.push((item, name));
                    continue;
                }
            };
            let task = SpawnedTask::child(item.display_name().to_string(), key, started, child);
            tasks.push(self.output.attach(self.overrides, task, spool));
        }
        for (item, name) in builtins {
            let started = Instant::now();
            let spool = self.output.spool()?;
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            let code = crate::run_builtin(
                ctx,
                self.overrides,
                &name,
                &item.args,
                &mut crate::render::out::Out::Captured(&mut stdout, &mut stderr),
                Some(&mut *warnings),
            )?;
            let task = SpawnedTask::builtin(
                item.display_name().to_string(),
                name,
                started,
                code,
                stdout,
                stderr,
            );
            tasks.push(self.output.attach(self.overrides, task, spool));
        }
        Ok(())
    }

    fn poll(&mut self, tasks: Vec<O::Task>, kill_on_fail: bool) -> Result<()> {
        let mut remaining = tasks;
        while !remaining.is_empty() {
            let mut next: Vec<O::Task> = Vec::with_capacity(remaining.len());
            // A `try_wait` error must not orphan the siblings: `Child::drop`
            // does not kill, so bail out through the same kill + reap + drain
            // cleanup the spawn phase uses instead of `?`-ing mid-iteration.
            let mut poll_error: Option<anyhow::Error> = None;
            let mut pending = std::mem::take(&mut remaining).into_iter();
            for mut task in pending.by_ref() {
                match O::job(&mut task).try_wait() {
                    Ok(Some(code)) => {
                        if code != 0 {
                            self.first_failure.get_or_insert(code);
                        }
                        self.output
                            .finished(self.overrides, self.outcomes, task, code);
                    }
                    Ok(None) => {
                        if kill_on_fail && self.first_failure.is_some() {
                            self.output.kill(self.overrides, self.outcomes, task);
                        } else {
                            next.push(task);
                        }
                    }
                    Err(e) => {
                        self.output.abort(task);
                        poll_error = Some(e.into());
                        break;
                    }
                }
            }
            if let Some(e) = poll_error {
                self.abort(next.into_iter().chain(pending));
                return Err(e);
            }
            remaining = next;
            if !remaining.is_empty() {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        Ok(())
    }

    fn abort(&mut self, tasks: impl IntoIterator<Item = O::Task>) {
        for task in tasks {
            self.output.abort(task);
        }
        self.output.drain();
    }
}

struct Streaming {
    width: usize,
    colorize: bool,
    base: Arc<dyn LineSink>,
    readers: Vec<std::thread::JoinHandle<()>>,
}

impl Streaming {
    fn new(chain: &Chain) -> Self {
        let names: Vec<&str> = chain.items.iter().map(ChainItem::display_name).collect();
        // Synchronous sink: each reader thread writes lines directly to
        // stdout/stderr, taking the lock per line. Bounding lock duration to
        // one `writeln!` avoids deadlocking against `eprintln!` on the main
        // thread (the `→ <source> <task>` arrow in `dispatch_task_piped`).
        Self {
            width: crate::chain::mux::prefix_width(&names),
            colorize: colored::control::SHOULD_COLORIZE.should_colorize(),
            base: Arc::new(crate::chain::mux::StdioSink),
            readers: Vec::new(),
        }
    }
}

impl ParallelOutput for Streaming {
    type Spool = ();
    type Task = SpawnedTask;

    fn spool(&self) -> Result<()> {
        Ok(())
    }

    fn attach(
        &mut self,
        overrides: &ResolutionOverrides,
        mut task: SpawnedTask,
        (): (),
    ) -> SpawnedTask {
        let prefix = if overrides.emits_groups_for(&task.key) {
            crate::chain::mux::render_prefix(&task.name, self.width, self.colorize)
        } else {
            String::new()
        };
        let stdout = std::mem::replace(&mut task.stdout, Box::new(std::io::empty()));
        let stderr = std::mem::replace(&mut task.stderr, Box::new(std::io::empty()));
        let (stdout_policy, stderr_policy) = overrides.task_streams_for(&task.key);
        let sink: Arc<dyn LineSink> = Arc::new(crate::chain::mux::SelectiveSink::new(
            Arc::clone(&self.base),
            stdout_policy == crate::tool::TaskStream::Inherit,
            stderr_policy == crate::tool::TaskStream::Inherit,
        ));
        self.readers.extend(crate::chain::mux::spawn_readers(
            vec![(prefix.clone(), false, stdout), (prefix, true, stderr)],
            &sink,
        ));
        task
    }

    fn job(task: &mut SpawnedTask) -> &mut Job {
        &mut task.job
    }

    fn finished(
        &mut self,
        overrides: &ResolutionOverrides,
        outcomes: &mut Vec<ItemOutcome>,
        task: SpawnedTask,
        code: i32,
    ) {
        record_finished(
            &task.key,
            overrides,
            outcomes,
            task.name,
            task.started.elapsed(),
            code,
        );
    }

    fn kill(
        &mut self,
        overrides: &ResolutionOverrides,
        outcomes: &mut Vec<ItemOutcome>,
        task: SpawnedTask,
    ) {
        let SpawnedTask {
            name,
            key,
            started,
            mut job,
            ..
        } = task;
        // Wait so stdio drains fully; a killed sibling still
        // reports timing for the work it managed before SIGKILL.
        // An exit that raced the kill stays a real result.
        let natural = job.kill();
        let elapsed = started.elapsed();
        match natural {
            Some(code) => {
                record_finished(&key, overrides, outcomes, name, elapsed, code);
            }
            None => record_killed(&key, overrides, outcomes, name, elapsed),
        }
    }

    /// Kill + reap a streaming-chain child that must not outlive an error
    /// return: `Child::drop` does not kill, so every early exit routes
    /// through here.
    fn abort(&mut self, mut task: SpawnedTask) {
        task.job.abort();
    }

    /// Bounded drain, not an unbounded join: a reader only EOFs once every
    /// write end of its pipe closes, and a task can leave a backgrounded
    /// descendant holding the inherited fd open after the direct child is
    /// reaped. The grouped path already guards this (see
    /// `flush_task_group`); without the bound, `run -p` hangs forever on
    /// such a task. Unfinished readers are abandoned after the grace.
    fn drain(&mut self) {
        wait_for_readers(&mut self.readers, READER_DRAIN_GRACE);
    }
}

/// Emit a finished streaming-chain task's timing line and record it for the
/// end-of-chain summary. Shared by the normal-exit and SIGKILL branches, so
/// a killed sibling appears in the summary with the work it did manage.
fn record_finished(
    key: &str,
    overrides: &ResolutionOverrides,
    outcomes: &mut Vec<ItemOutcome>,
    name: String,
    elapsed: std::time::Duration,
    code: i32,
) {
    crate::commands::emit_task_timing(overrides, key, &name, elapsed, code);
    outcomes.push(ItemOutcome {
        name,
        status: ItemStatus::Ran { code, elapsed },
    });
}

/// Emit a killed streaming-chain sibling's timing line and record it for the
/// end-of-chain summary, distinct from a real failure.
fn record_killed(
    key: &str,
    overrides: &ResolutionOverrides,
    outcomes: &mut Vec<ItemOutcome>,
    name: String,
    elapsed: std::time::Duration,
) {
    crate::commands::emit_task_killed(overrides, key, &name, elapsed);
    outcomes.push(ItemOutcome {
        name,
        status: ItemStatus::Killed { elapsed },
    });
}

/// Per-task state for grouped parallel execution: the child, the
/// sink its reader threads append into, those reader handles, and the
/// spawn `Instant` anchoring the duration folded into the block footer.
struct GroupedTask {
    name: String,
    key: String,
    started: Instant,
    job: Job,
    sink: Arc<BufferSink>,
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
    outcomes: &mut Vec<ItemOutcome>,
    style: BlockStyle,
    in_gha: bool,
) -> Result<i32> {
    // `style` (passed in) selects `::group::` vs a plain header vs no
    // delimiter per block; colorize the plain headers only when stdout
    // supports it.
    let output = Grouped {
        style,
        in_gha,
        colorize: colored::control::SHOULD_COLORIZE.should_colorize(),
    };
    Supervisor::new(overrides, outcomes, output).run(ctx, chain, warnings)
}

struct Grouped {
    style: BlockStyle,
    in_gha: bool,
    colorize: bool,
}

impl ParallelOutput for Grouped {
    type Spool = Arc<BufferSink>;
    type Task = GroupedTask;

    fn spool(&self) -> Result<Arc<BufferSink>> {
        Ok(Arc::new(BufferSink::new()?))
    }

    fn attach(
        &mut self,
        overrides: &ResolutionOverrides,
        task: SpawnedTask,
        sink: Arc<BufferSink>,
    ) -> GroupedTask {
        // `.clone()` resolves on the concrete `Arc<BufferSink>` then
        // unsizes to the trait object; `Arc::clone(&sink)` would instead
        // infer its generic from the annotation and fail to coerce.
        let base: Arc<dyn LineSink> = sink.clone();
        let (stdout_policy, stderr_policy) = overrides.task_streams_for(&task.key);
        let dyn_sink: Arc<dyn LineSink> = Arc::new(crate::chain::mux::SelectiveSink::new(
            base,
            stdout_policy == crate::tool::TaskStream::Inherit,
            stderr_policy == crate::tool::TaskStream::Inherit,
        ));
        // No prefix: the group title identifies the task, while the sink
        // preserves stdout/stderr identity for replay.
        let readers = crate::chain::mux::spawn_readers(
            vec![
                (String::new(), false, task.stdout),
                (String::new(), true, task.stderr),
            ],
            &dyn_sink,
        );
        GroupedTask {
            name: task.name,
            key: task.key,
            started: task.started,
            job: task.job,
            sink,
            readers,
        }
    }

    fn job(task: &mut GroupedTask) -> &mut Job {
        &mut task.job
    }

    /// Flush each task's block the moment it completes, so blocks appear in
    /// completion order (first done, first shown). Only the supervisor
    /// thread writes them, one at a time, so blocks never overlap.
    fn finished(
        &mut self,
        overrides: &ResolutionOverrides,
        outcomes: &mut Vec<ItemOutcome>,
        task: GroupedTask,
        code: i32,
    ) {
        let footer = record_grouped(overrides, outcomes, &task, code);
        flush_grouped_task(
            task,
            self.style,
            self.in_gha,
            self.colorize,
            footer.as_deref(),
        );
    }

    fn kill(
        &mut self,
        overrides: &ResolutionOverrides,
        outcomes: &mut Vec<ItemOutcome>,
        task: GroupedTask,
    ) {
        kill_grouped_sibling(
            overrides,
            outcomes,
            task,
            self.style,
            self.in_gha,
            self.colorize,
        );
    }

    fn abort(&mut self, task: GroupedTask) {
        cleanup_grouped_task(task);
    }

    fn drain(&mut self) {}
}

/// Record a finished grouped task for the end-of-chain summary and return
/// its block footer. Reads the elapsed time once so the summary row and the
/// footer report the same duration.
fn record_grouped(
    overrides: &ResolutionOverrides,
    outcomes: &mut Vec<ItemOutcome>,
    task: &GroupedTask,
    code: i32,
) -> Option<String> {
    let elapsed = task.started.elapsed();
    outcomes.push(ItemOutcome {
        name: task.name.clone(),
        status: ItemStatus::Ran { code, elapsed },
    });
    timing_footer(overrides, &task.key, elapsed, code)
}

/// Kill a still-running grouped sibling after a chain failure, record it,
/// and flush its block. A killed sibling still reports timing for the work
/// it completed before SIGKILL; a clean exit that raced the kill stays a
/// real result. Split out to keep the supervisor poll loop under the
/// per-function line budget, like [`flush_grouped_task`].
fn kill_grouped_sibling(
    overrides: &ResolutionOverrides,
    outcomes: &mut Vec<ItemOutcome>,
    mut task: GroupedTask,
    style: BlockStyle,
    in_gha: bool,
    colorize: bool,
) {
    let footer = match task.job.kill() {
        Some(code) => record_grouped(overrides, outcomes, &task, code),
        None => record_grouped_killed(overrides, outcomes, &task),
    };
    flush_grouped_task(task, style, in_gha, colorize, footer.as_deref());
}

/// The exit code of a just-killed sibling that beat the SIGKILL to its own
/// exit, or `None` when the kill (or a `wait` failure) took it down. On Unix
/// only a SIGKILL death reads as the kill; a crash under another signal
/// keeps its 128+n code. Windows `TerminateProcess` reports exit code 1,
/// indistinguishable from a real failure, so only a clean exit counts as
/// natural there.
#[cfg(unix)]
fn natural_exit_code(status: std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;

    const SIGKILL: i32 = 9;
    match status.signal() {
        Some(SIGKILL) => None,
        Some(_) => Some(crate::commands::exit_code(status)),
        None => status.code(),
    }
}

#[cfg(not(unix))]
fn natural_exit_code(status: std::process::ExitStatus) -> Option<i32> {
    Some(crate::commands::exit_code(status)).filter(|&code| code == 0)
}

/// Record a killed grouped sibling for the end-of-chain summary, distinct
/// from a real failure, and return its block footer.
fn record_grouped_killed(
    overrides: &ResolutionOverrides,
    outcomes: &mut Vec<ItemOutcome>,
    task: &GroupedTask,
) -> Option<String> {
    let elapsed = task.started.elapsed();
    outcomes.push(ItemOutcome {
        name: task.name.clone(),
        status: ItemStatus::Killed { elapsed },
    });
    crate::commands::timing_enabled_for(overrides, &task.key)
        .then(|| crate::commands::task_killed_summary(elapsed))
}

/// Flush a completed grouped task's block, moving its reader handles into
/// [`flush_task_group`]. Thin wrapper that keeps the supervisor poll loop
/// under the per-function line budget by hiding the field-destructuring at
/// the two completion sites (normal exit and SIGKILL).
fn flush_grouped_task(
    task: GroupedTask,
    style: BlockStyle,
    in_gha: bool,
    colorize: bool,
    footer: Option<&str>,
) {
    flush_task_group(
        &task.name,
        style,
        in_gha,
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
    style: BlockStyle,
    in_gha: bool,
    colorize: bool,
    sink: &BufferSink,
    mut readers: Vec<std::thread::JoinHandle<()>>,
    timing_footer: Option<&str>,
) {
    use std::io::Write as _;

    wait_for_readers(&mut readers, READER_DRAIN_GRACE);
    sink.close();
    join_finished_readers(&mut readers);

    // GroupGuard writes `::group::` now and `::endgroup::` on drop; don't
    // hold the stdout lock across the guard's Drop. Bound to the whole body
    // so the footer lands inside the fold.
    let group = match style {
        BlockStyle::Gha => Some(actions_rs::log::group_guard(format!("runner: {name}"))),
        BlockStyle::Header => {
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
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{header}");
            let _ = out.flush();
            None
        }
        BlockStyle::Bare => None,
    };

    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    // Under Actions, neutralize child group/endgroup commands: replay
    // reorders them relative to when they were written, so they would nest
    // in or close a fold early. Elsewhere no interpretation happens, so
    // leave the child's bytes untouched.
    let _ = sink.replay_to(&mut stdout, &mut stderr, in_gha);
    write_timing_footer(timing_footer, colorize);
    drop(group);
}

/// Compute the grouped-mode block footer (`finished in 1.2s (exit 0)`) when
/// per-task timing is enabled, or `None` when muted via `--quiet` /
/// `--no-warnings`. Shared by both grouped completion paths so the gating
/// stays in one place.
fn timing_footer(
    overrides: &ResolutionOverrides,
    task: &str,
    elapsed: std::time::Duration,
    code: i32,
) -> Option<String> {
    crate::commands::timing_enabled_for(overrides, task)
        .then(|| crate::commands::task_timing_summary(elapsed, code))
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

/// Close a multi-task chain with a per-task roll-up on stderr, so the one
/// failing task in a long `--keep-going` run is visible without scrolling
/// back through the interleaved logs, and the chain's exit code is
/// attributed to the task that produced it.
///
/// Single-task chains get nothing: the per-task timing line already says
/// everything a summary would. The independent summary category allows a
/// final roll-up without per-task timing, and vice versa.
fn emit_chain_summary(overrides: &ResolutionOverrides, outcomes: &[ItemOutcome], code: i32) {
    use colored::Colorize as _;

    if outcomes.len() < 2 {
        return;
    }

    let failed: Vec<&ItemOutcome> = outcomes.iter().filter(|o| o.failed()).collect();
    if overrides.shows_summary() {
        let counts = summary_counts(outcomes, failed.len());
        // The aggregate is the first failure the chain observed, in detection
        // order; say so rather than leaving a bare number to interpret.
        let verdict = if failed.is_empty() {
            format!("exit {code}")
        } else {
            format!("exit {code}, first failure")
        };
        eprintln!(
            "{} {} {}",
            "·".dimmed(),
            format!("summary: {counts}").bold(),
            format!("({verdict})").dimmed(),
        );

        let names: Vec<&str> = outcomes.iter().map(|o| o.name.as_str()).collect();
        let width = crate::chain::mux::prefix_width(&names);
        for outcome in outcomes {
            eprintln!("{}   {}", "·".dimmed(), outcome.render(width));
        }
    }

    // Failure attribution in the Annotations panel, where a reader lands
    // before they ever open the log. Suppressed with the broad
    // `[github].group_output` opt-out, which owns runner's Actions output.
    if overrides.shows_errors()
        && overrides.grouping.group_output
        && actions_rs::env::is_github_actions()
    {
        for outcome in failed {
            let ItemStatus::Ran { code, .. } = outcome.status else {
                continue;
            };
            actions_rs::Annotation::new()
                .title(format!("runner: {}", outcome.name))
                .error(format!("exit {code}"));
        }
    }
}

/// `4 tasks, 1 ok, 1 failed, 1 killed, 1 skipped`, with the zero buckets
/// left out.
fn summary_counts(outcomes: &[ItemOutcome], failed: usize) -> String {
    use std::fmt::Write as _;

    let skipped = outcomes
        .iter()
        .filter(|o| matches!(o.status, ItemStatus::Skipped))
        .count();
    let killed = outcomes
        .iter()
        .filter(|o| matches!(o.status, ItemStatus::Killed { .. }))
        .count();
    let mut counts = format!(
        "{} tasks, {} ok",
        outcomes.len(),
        outcomes.len() - failed - killed - skipped
    );
    if failed > 0 {
        let _ = write!(counts, ", {failed} failed");
    }
    if killed > 0 {
        let _ = write!(counts, ", {killed} killed");
    }
    if skipped > 0 {
        let _ = write!(counts, ", {skipped} skipped");
    }
    counts
}

impl ItemOutcome {
    const fn failed(&self) -> bool {
        matches!(self.status, ItemStatus::Ran { code, .. } if code != 0)
    }

    /// One summary row: status mark, padded task name, and either the
    /// duration (plus exit code when non-zero) or `skipped`.
    fn render(&self, width: usize) -> String {
        use colored::Colorize as _;

        let (mark, detail) = match self.status {
            ItemStatus::Ran { code: 0, elapsed } => {
                ("✓".green(), crate::commands::format_duration(elapsed))
            }
            ItemStatus::Ran { code, elapsed } => (
                "✗".red(),
                format!(
                    "{} (exit {code})",
                    crate::commands::format_duration(elapsed)
                ),
            ),
            ItemStatus::Killed { elapsed } => {
                ("–".dimmed(), crate::commands::task_killed_summary(elapsed))
            }
            ItemStatus::Skipped => ("–".dimmed(), String::from("skipped")),
        };
        format!(
            "{mark} {:<width$}  {}",
            self.name,
            detail.dimmed(),
            width = width,
        )
    }
}

/// Kill + reap a grouped task and drain its readers with the bounded
/// grace, the cleanup every grouped-chain error path shares.
fn cleanup_grouped_task(mut t: GroupedTask) {
    t.job.abort();
    t.sink.close();
    wait_for_readers(&mut t.readers, READER_DRAIN_GRACE);
}

fn wait_for_readers(readers: &mut Vec<std::thread::JoinHandle<()>>, grace: std::time::Duration) {
    let deadline = Instant::now() + grace;
    loop {
        join_finished_readers(readers);
        if readers.is_empty() {
            return;
        }

        let now = Instant::now();
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
) -> Result<(i32, String)> {
    match &item.kind {
        ChainItemKind::Task(name) => {
            // v1 ChainItem.args is always empty; v2 will populate it.
            crate::commands::run::run_with_key(ctx, overrides, name, &item.args, Some(warnings))
        }
        ChainItemKind::Install { flags } => {
            crate::commands::install::install_pms(ctx, overrides, *flags, Some(warnings))
                .map(|code| (code, "install".into()))
        }
    }
}
