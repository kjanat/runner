//! `runner exec` — run an ad-hoc command through the detected package manager.

use std::process::Command;

use anyhow::{Result, bail};

use crate::tool;
use crate::types::{PackageManager, ProjectContext};

/// Execute `args` through the primary package manager's exec mechanism
/// (`npx`, `bunx`, `pnpm exec`, `cargo`, `uv run`, etc.).
///
/// Falls back to running the command directly when no PM is detected.
/// Returns the child process exit code.
pub(crate) fn exec(ctx: &ProjectContext, args: &[String]) -> Result<i32> {
    if args.is_empty() {
        bail!("usage: runner exec <command> [args...]");
    }

    let mut cmd = match ctx.primary_pm() {
        Some(PackageManager::Npm) => tool::npm::exec_cmd(args),
        Some(PackageManager::Yarn) => tool::yarn::exec_cmd(args),
        Some(PackageManager::Pnpm) => tool::pnpm::exec_cmd(args),
        Some(PackageManager::Bun) => tool::bun::exec_cmd(args),
        Some(PackageManager::Cargo) => tool::cargo_pm::exec_cmd(args),
        Some(PackageManager::Deno) => tool::deno::exec_cmd(args),
        Some(PackageManager::Uv) => tool::uv::exec_cmd(args),
        None | Some(_) => {
            let mut c = Command::new(&args[0]);
            if args.len() > 1 {
                c.args(&args[1..]);
            }
            c
        }
    };

    super::configure_command(&mut cmd, &ctx.root);

    Ok(super::exit_code(cmd.status()?))
}
