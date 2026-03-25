use std::process::{Command, Stdio};

use anyhow::{Result, bail};
use colored::Colorize;

use crate::detect::{PackageManager, ProjectContext, TaskRunner};

pub fn install(ctx: &ProjectContext, frozen: bool) -> Result<()> {
    if ctx.package_managers.is_empty() {
        bail!("No package manager detected.");
    }

    // Node version check
    if let (Some(nv), Some(cur)) = (&ctx.node_version, &ctx.current_node)
        && !version_matches(&nv.expected, cur)
    {
        eprintln!(
            "{} node expected {} ({}), current {}",
            "warn:".yellow().bold(),
            nv.expected,
            nv.source,
            cur,
        );
        suggest_version_switch(ctx);
    }

    for pm in &ctx.package_managers {
        eprintln!("{} {}", "installing with".dimmed(), pm.label().bold());
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

/// Loose semver match: "20" matches "20.x.y", ">=18" matches "18+", etc.
pub fn version_matches(expected: &str, current: &str) -> bool {
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
