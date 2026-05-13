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

    // Spawn each task with piped stdio and start reader threads.
    let mut children: Vec<(String, Child)> = Vec::with_capacity(chain.items.len());
    let mut reader_handles = Vec::new();

    for item in &chain.items {
        let prefix = render_prefix(item.display_name(), width, colorize);
        let mut child = match &item.kind {
            ChainItemKind::Task(name) => {
                crate::cmd::run::dispatch_task_piped(ctx, overrides, name, &item.args)?
            }
            ChainItemKind::Install => {
                anyhow::bail!("install dispatch not yet wired into parallel executor (Task 13)")
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
            tx.clone(),
        ));
        children.push((item.display_name().to_string(), child));
    }
    drop(tx); // close producer side so channel closes when all readers finish

    // Writer thread: drain channel, write prefixed lines to parent stdio.
    let writer = std::thread::spawn(move || {
        use std::io::Write;
        let stdout = std::io::stdout();
        let stderr = std::io::stderr();
        let mut stdout = stdout.lock();
        let mut stderr = stderr.lock();
        for msg in rx {
            let target: &mut dyn Write = if msg.is_stderr {
                &mut stderr
            } else {
                &mut stdout
            };
            let _ = writeln!(target, "{} {}", msg.prefix, msg.line);
        }
    });

    // Poll children. On first failure with KillOnFail, kill remaining
    // siblings; otherwise let them finish naturally.
    let mut remaining: Vec<(String, Child)> = children;
    let mut first_failure: Option<i32> = None;
    let kill_on_fail = matches!(chain.failure, FailurePolicy::KillOnFail);

    while !remaining.is_empty() {
        let mut next: Vec<(String, Child)> = Vec::with_capacity(remaining.len());
        let mut killed_this_pass = false;
        for (name, mut child) in remaining.drain(..) {
            match child.try_wait()? {
                Some(status) => {
                    let code = status.code().unwrap_or(1);
                    if code != 0 {
                        first_failure.get_or_insert(code);
                        if kill_on_fail && !killed_this_pass {
                            killed_this_pass = true;
                        }
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
