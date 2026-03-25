use std::process::{Command, Stdio};

use anyhow::{Result, bail};

use crate::tool;
use crate::types::{PackageManager, ProjectContext};

pub fn exec(ctx: &ProjectContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("usage: runner exec <command> [args...]");
    }

    let pm = ctx.primary_pm().unwrap_or(PackageManager::Npm);

    let mut cmd = match pm {
        PackageManager::Npm => tool::npm::exec_cmd(args),
        PackageManager::Yarn => tool::yarn::exec_cmd(args),
        PackageManager::Pnpm => tool::pnpm::exec_cmd(args),
        PackageManager::Bun => tool::bun::exec_cmd(args),
        PackageManager::Cargo => tool::cargo_pm::exec_cmd(args),
        PackageManager::Deno => tool::deno::exec_cmd(args),
        PackageManager::Uv => tool::uv::exec_cmd(args),
        _ => {
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
