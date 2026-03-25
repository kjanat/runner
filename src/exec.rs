use std::collections::HashSet;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::{fs, io};

use anyhow::{Result, bail};
use colored::Colorize;

use crate::detect::{PackageManager, ProjectContext, TaskRunner, TaskSource};

// ── info ───────────────────────────────────────────────────────────────

pub fn info(ctx: &ProjectContext) -> Result<()> {
    println!("{}", "runner".bold());
    println!();

    if ctx.package_managers.is_empty() && ctx.task_runners.is_empty() && ctx.tasks.is_empty() {
        println!("  {}", "No project detected in current directory.".dimmed());
        return Ok(());
    }

    if !ctx.package_managers.is_empty() {
        let pms: Vec<&str> = ctx.package_managers.iter().map(|pm| pm.label()).collect();
        println!("  {:<20}{}", "Package Managers".dimmed(), pms.join(", "));
    }

    if !ctx.task_runners.is_empty() {
        let trs: Vec<&str> = ctx.task_runners.iter().map(|tr| tr.label()).collect();
        println!("  {:<20}{}", "Task Runners".dimmed(), trs.join(", "));
    }

    // Node version
    if let Some(nv) = &ctx.node_version {
        let mut line = format!("{} ({})", nv.expected, nv.source);
        if let Some(cur) = &ctx.current_node {
            if version_matches(&nv.expected, cur) {
                line.push_str(&format!(", current {} {}", cur, "(ok)".green()));
            } else {
                line.push_str(&format!(", current {} {}", cur, "(mismatch)".red()));
            }
        }
        println!("  {:<20}{}", "Node".dimmed(), line);
    } else if let Some(cur) = &ctx.current_node {
        println!("  {:<20}{}", "Node".dimmed(), cur);
    }

    if ctx.is_monorepo {
        println!("  {:<20}{}", "Monorepo".dimmed(), "yes".green());
    }

    if !ctx.tasks.is_empty() {
        println!();
        print_tasks_grouped(ctx);
    }

    Ok(())
}

// ── list ───────────────────────────────────────────────────────────────

pub fn list(ctx: &ProjectContext, raw: bool) -> Result<()> {
    if raw {
        let mut seen = HashSet::new();
        for task in &ctx.tasks {
            if seen.insert(&task.name) {
                println!("{}", task.name);
            }
        }
    } else if ctx.tasks.is_empty() {
        println!("{}", "No tasks found.".dimmed());
    } else {
        print_tasks_grouped(ctx);
    }
    Ok(())
}

fn print_tasks_grouped(ctx: &ProjectContext) {
    let sources = [
        TaskSource::PackageJson,
        TaskSource::TurboJson,
        TaskSource::Makefile,
        TaskSource::Justfile,
        TaskSource::Taskfile,
        TaskSource::DenoJson,
    ];
    for source in sources {
        let names: Vec<&str> = ctx
            .tasks
            .iter()
            .filter(|t| t.source == source)
            .map(|t| t.name.as_str())
            .collect();
        if names.is_empty() {
            continue;
        }
        println!("  {:<16}{}", source.label().bold(), names.join(", "));
    }
}

// ── run ────────────────────────────────────────────────────────────────

pub fn run(ctx: &ProjectContext, task: &str, args: &[String]) -> Result<()> {
    let found: Vec<_> = ctx.tasks.iter().filter(|t| t.name == task).collect();

    if found.is_empty() {
        bail!("task {task:?} not found. Run `runner list` to see available tasks.");
    }

    // Priority: turbo > package.json > makefile > justfile > taskfile > deno
    let entry = found
        .iter()
        .find(|t| t.source == TaskSource::TurboJson)
        .or_else(|| found.iter().find(|t| t.source == TaskSource::PackageJson))
        .or_else(|| found.first())
        .unwrap();

    eprintln!(
        "{} {} {} {}",
        "→".dimmed(),
        entry.source.label().dimmed(),
        task.bold(),
        args.join(" ").dimmed(),
    );

    let mut cmd = build_run_command(ctx, entry.source, task, args)?;
    let status = cmd.status()?;
    std::process::exit(status.code().unwrap_or(1));
}

fn build_run_command(
    ctx: &ProjectContext,
    source: TaskSource,
    task: &str,
    extra: &[String],
) -> Result<Command> {
    let mut cmd = match source {
        TaskSource::TurboJson => {
            let mut c = Command::new("turbo");
            c.arg("run").arg(task);
            if !extra.is_empty() {
                c.arg("--").args(extra);
            }
            c
        }
        TaskSource::PackageJson => build_pm_run(ctx, task, extra)?,
        TaskSource::Makefile => {
            let mut c = Command::new("make");
            c.arg(task).args(extra);
            c
        }
        TaskSource::Justfile => {
            let mut c = Command::new("just");
            c.arg(task).args(extra);
            c
        }
        TaskSource::Taskfile => {
            let mut c = Command::new("task");
            c.arg(task).args(extra);
            c
        }
        TaskSource::DenoJson => {
            let mut c = Command::new("deno");
            c.arg("task").arg(task).args(extra);
            c
        }
    };
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    Ok(cmd)
}

fn build_pm_run(ctx: &ProjectContext, task: &str, extra: &[String]) -> Result<Command> {
    let pm = ctx
        .primary_node_pm()
        .or_else(|| ctx.primary_pm())
        .unwrap_or(PackageManager::Npm);

    let c = match pm {
        PackageManager::Npm => {
            let mut c = Command::new("npm");
            c.arg("run").arg(task);
            if !extra.is_empty() {
                c.arg("--").args(extra);
            }
            c
        }
        PackageManager::Yarn => {
            let mut c = Command::new("yarn");
            c.arg(task).args(extra);
            c
        }
        PackageManager::Pnpm => {
            let mut c = Command::new("pnpm");
            c.arg("run").arg(task);
            if !extra.is_empty() {
                c.arg("--").args(extra);
            }
            c
        }
        PackageManager::Bun => {
            let mut c = Command::new("bun");
            c.arg("run").arg(task).args(extra);
            c
        }
        PackageManager::Deno => {
            let mut c = Command::new("deno");
            c.arg("task").arg(task).args(extra);
            c
        }
        other => bail!("{} cannot run scripts", other.label()),
    };
    Ok(c)
}

// ── install ────────────────────────────────────────────────────────────

pub fn install(ctx: &ProjectContext, frozen: bool) -> Result<()> {
    if ctx.package_managers.is_empty() {
        bail!("No package manager detected.");
    }

    // Node version check
    if let (Some(nv), Some(cur)) = (&ctx.node_version, &ctx.current_node) {
        if !version_matches(&nv.expected, cur) {
            eprintln!(
                "{} node expected {} ({}), current {}",
                "warn:".yellow().bold(),
                nv.expected,
                nv.source,
                cur,
            );
            suggest_version_switch(ctx);
        }
    }

    for pm in &ctx.package_managers {
        eprintln!("{} {}", "installing with".dimmed(), pm.label().bold(),);
        let mut cmd = build_install_command(*pm, frozen)?;
        let status = cmd.status()?;
        if !status.success() {
            bail!(
                "{} install failed (exit {})",
                pm.label(),
                status.code().unwrap_or(1)
            );
        }
    }
    Ok(())
}

fn build_install_command(pm: PackageManager, frozen: bool) -> Result<Command> {
    let mut cmd = match pm {
        PackageManager::Npm => {
            let mut c = Command::new("npm");
            c.arg(if frozen { "ci" } else { "install" });
            c
        }
        PackageManager::Yarn => {
            let mut c = Command::new("yarn");
            c.arg("install");
            if frozen {
                c.arg("--frozen-lockfile");
            }
            c
        }
        PackageManager::Pnpm => {
            let mut c = Command::new("pnpm");
            c.arg("install");
            if frozen {
                c.arg("--frozen-lockfile");
            }
            c
        }
        PackageManager::Bun => {
            let mut c = Command::new("bun");
            c.arg("install");
            if frozen {
                c.arg("--frozen-lockfile");
            }
            c
        }
        PackageManager::Cargo => {
            let mut c = Command::new("cargo");
            c.arg(if frozen { "fetch" } else { "build" });
            c
        }
        PackageManager::Deno => {
            let mut c = Command::new("deno");
            c.arg("install");
            c
        }
        PackageManager::Uv => {
            let mut c = Command::new("uv");
            c.arg("sync");
            if frozen {
                c.arg("--frozen");
            }
            c
        }
        PackageManager::Poetry => {
            let mut c = Command::new("poetry");
            c.arg("install");
            c
        }
        PackageManager::Pipenv => {
            let mut c = Command::new("pipenv");
            c.arg("install");
            c
        }
        PackageManager::Go => {
            let mut c = Command::new("go");
            c.arg("mod").arg("download");
            c
        }
        PackageManager::Bundler => {
            let mut c = Command::new("bundle");
            c.arg("install");
            c
        }
        PackageManager::Composer => {
            let mut c = Command::new("composer");
            c.arg("install");
            c
        }
    };
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    Ok(cmd)
}

fn suggest_version_switch(ctx: &ProjectContext) {
    let hint = if ctx
        .node_version
        .as_ref()
        .is_some_and(|nv| nv.source == ".nvmrc")
    {
        "nvm use"
    } else if ctx.task_runners.contains(&TaskRunner::Mise) {
        "mise install"
    } else {
        "fnm use"
    };
    eprintln!("       {} {}", "hint:".dimmed(), hint);
}

// ── clean ──────────────────────────────────────────────────────────────

pub fn clean(ctx: &ProjectContext, skip_confirm: bool) -> Result<()> {
    let mut targets: Vec<&str> = Vec::new();

    for pm in &ctx.package_managers {
        match pm {
            PackageManager::Npm
            | PackageManager::Yarn
            | PackageManager::Pnpm
            | PackageManager::Bun => {
                for d in &[
                    "node_modules",
                    ".next",
                    "dist",
                    ".cache",
                    ".parcel-cache",
                    ".svelte-kit",
                ] {
                    push_if_exists(&mut targets, d);
                }
            }
            PackageManager::Cargo => push_if_exists(&mut targets, "target"),
            PackageManager::Deno => push_if_exists(&mut targets, ".deno"),
            PackageManager::Uv | PackageManager::Poetry | PackageManager::Pipenv => {
                for d in &[".venv", "__pycache__", ".mypy_cache", ".ruff_cache"] {
                    push_if_exists(&mut targets, d);
                }
            }
            PackageManager::Go => push_if_exists(&mut targets, "vendor"),
            _ => {}
        }
    }

    for tr in &ctx.task_runners {
        match tr {
            TaskRunner::Turbo => push_if_exists(&mut targets, ".turbo"),
            TaskRunner::Nx => push_if_exists(&mut targets, ".nx"),
            _ => {}
        }
    }

    // Deduplicate
    targets.sort();
    targets.dedup();

    if targets.is_empty() {
        println!("{}", "Nothing to clean.".dimmed());
        return Ok(());
    }

    println!("Will remove:");
    for t in &targets {
        println!("  {}", t);
    }

    if !skip_confirm {
        print!("\nProceed? [y/N] ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    for t in &targets {
        let path = ctx.root.join(t);
        if path.is_dir() {
            fs::remove_dir_all(&path)?;
        } else if path.is_file() {
            fs::remove_file(&path)?;
        }
        println!("  {} {}", "removed".red(), t);
    }

    Ok(())
}

fn push_if_exists<'a>(targets: &mut Vec<&'a str>, name: &'a str) {
    // Check relative to CWD since we're called with relative names
    if Path::new(name).exists() && !targets.contains(&name) {
        targets.push(name);
    }
}

// ── exec ───────────────────────────────────────────────────────────────

pub fn exec(ctx: &ProjectContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("usage: runner exec <command> [args...]");
    }

    let pm = ctx.primary_pm().unwrap_or(PackageManager::Npm);

    let mut cmd = match pm {
        PackageManager::Npm => {
            let mut c = Command::new("npx");
            c.args(args);
            c
        }
        PackageManager::Yarn => {
            let mut c = Command::new("yarn");
            c.arg("exec").args(args);
            c
        }
        PackageManager::Pnpm => {
            let mut c = Command::new("pnpm");
            c.arg("exec").args(args);
            c
        }
        PackageManager::Bun => {
            let mut c = Command::new("bunx");
            c.args(args);
            c
        }
        PackageManager::Cargo => {
            let mut c = Command::new("cargo");
            c.args(args);
            c
        }
        PackageManager::Deno => {
            let mut c = Command::new("deno");
            c.arg("run").args(args);
            c
        }
        PackageManager::Uv => {
            let mut c = Command::new("uv");
            c.arg("run").args(args);
            c
        }
        _ => {
            // Fallback: run directly
            let mut c = Command::new(&args[0]);
            if args.len() > 1 {
                c.args(&args[1..]);
            }
            c
        }
    };

    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let status = cmd.status()?;
    std::process::exit(status.code().unwrap_or(1));
}

// ── completions ────────────────────────────────────────────────────────

pub fn completions(shell: clap_complete::Shell) -> Result<()> {
    use clap::CommandFactory;
    clap_complete::generate(
        shell,
        &mut crate::cli::Cli::command(),
        "runner",
        &mut io::stdout(),
    );
    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Loose semver match: "20" matches "20.x.y", ">=18" matches "18+", etc.
fn version_matches(expected: &str, current: &str) -> bool {
    let expected = expected.trim();
    let current = current.trim();

    // Strip leading >= or ~ or ^ for a rough check
    let expected_clean = expected
        .trim_start_matches(">=")
        .trim_start_matches("<=")
        .trim_start_matches('>')
        .trim_start_matches('<')
        .trim_start_matches('~')
        .trim_start_matches('^')
        .trim_start_matches('v')
        .trim();

    // If expected is just a major version like "20", match major
    if !expected_clean.contains('.') {
        return current.starts_with(expected_clean)
            && current[expected_clean.len()..]
                .chars()
                .next()
                .is_none_or(|c| c == '.');
    }

    // Exact prefix match (e.g. "20.11" matches "20.11.0")
    current.starts_with(expected_clean)
}
