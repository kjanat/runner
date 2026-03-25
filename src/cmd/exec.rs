use std::process::{Command, Stdio};

use anyhow::{Result, bail};

use crate::detect::{PackageManager, ProjectContext};

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
