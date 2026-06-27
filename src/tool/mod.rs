//! Per-tool modules: detection, task extraction, and command building.
//!
//! Each module corresponds to a single tool (package manager or task runner)
//! and exposes a consistent set of public functions:
//!
//! - `detect(dir)` — returns `true` if the tool's config/lockfile exists
//! - `extract_tasks(dir)` — parses config and returns task names or a parse error
//! - `run_cmd(task, args)` — builds a [`std::process::Command`] to run a task
//! - `install_cmd(frozen)` — builds a [`std::process::Command`] to install deps
//! - `exec_cmd(args)` — builds a [`std::process::Command`] for ad-hoc execution
//! - clean-dir constants — directories to remove on `runner clean`
//!
//! Not every module exposes every function; only what the tool supports.

/// bacon — Rust background checker (`bacon.toml`).
pub(crate) mod bacon;
/// Bun JavaScript runtime and package manager.
pub(crate) mod bun;
/// Bundler, the Ruby dependency manager (`Gemfile`).
pub(crate) mod bundler;
/// Cargo `[alias]` table — `.cargo/config.toml` aliases as runnable tasks.
pub(crate) mod cargo_aliases;
/// Cargo, the Rust package manager and build tool (`Cargo.toml`).
pub(crate) mod cargo_pm;
/// Composer, the PHP dependency manager (`composer.json`).
pub(crate) mod composer;
/// Deno JavaScript/TypeScript runtime (`deno.json` / `deno.jsonc`).
pub(crate) mod deno;
/// In-process execution of deno tasks via `deno_task_shell` (no deno binary).
pub(crate) mod deno_exec;
/// Shared filesystem helpers for tool modules.
pub(crate) mod files;
/// Go modules (`go.mod`).
pub(crate) mod go_pm;
/// go-task, a task runner using `Taskfile.yml`.
pub(crate) mod go_task;
/// just, a command runner using `justfile`.
pub(crate) mod just;
/// GNU Make (`Makefile`).
pub(crate) mod make;
/// mise, a polyglot dev tool manager (`mise.toml`).
pub(crate) mod mise;
/// Shared Node.js helpers: `package.json` parsing, script extraction, PM detection.
pub(crate) mod node;
/// npm, the default Node.js package manager (`package-lock.json`).
pub(crate) mod npm;
/// Nx monorepo build system (`nx.json`).
pub(crate) mod nx;
/// Detect `package.json` scripts that wrap a known task runner.
pub(crate) mod passthrough;
/// Pipenv, a Python dependency manager (`Pipfile`).
pub(crate) mod pipenv;
/// pnpm, a fast Node.js package manager (`pnpm-lock.yaml`).
pub(crate) mod pnpm;
/// Poetry, a Python dependency manager (`poetry.lock`, `pyproject.toml`).
pub(crate) mod poetry;
/// Spawn helper with Windows-aware PATH/PATHEXT resolution.
pub(crate) mod program;
/// Shared Python tooling helpers.
pub(crate) mod python;
/// Cross-platform in-process shell runner (`deno_task_shell`), reusable
/// for any tool whose task bodies are shell command strings.
pub(crate) mod shell;
/// Turborepo monorepo build system (`turbo.json` / `turbo.jsonc`).
pub(crate) mod turbo;
/// uv, a fast Python package manager (`uv.lock`).
pub(crate) mod uv;
/// Volta toolchain manager — shim classification and `volta which` resolution.
pub(crate) mod volta;
/// Yarn, a Node.js package manager (`yarn.lock`).
pub(crate) mod yarn;

#[cfg(test)]
pub(crate) mod test_support;

/// What an install command should do with lifecycle scripts.
///
/// Lowered from `crate::resolver::ScriptPolicy` at the install dispatch
/// boundary so the per-tool `install_cmd` builders stay decoupled from the
/// resolver. Each manager translates this into its own flag/env (or no-ops
/// where it cannot express the request — `cmd::install` warns about those).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ScriptDirective {
    /// Leave the package manager at its built-in default — add nothing.
    #[default]
    Default,
    /// Skip lifecycle scripts where the manager exposes a skip mechanism
    /// (npm/yarn-classic/pnpm/bun `--ignore-scripts`, composer `--no-scripts`,
    /// yarn-berry `YARN_ENABLE_SCRIPTS=false`).
    Deny,
    /// Force lifecycle scripts on where the manager exposes a mechanism
    /// (npm `--no-ignore-scripts`, yarn-berry `YARN_ENABLE_SCRIPTS=true`,
    /// deno `--allow-scripts`). Managers that already run scripts by default
    /// (composer, cargo, go, bundler, the Python backends, yarn-classic) need
    /// nothing; pnpm/bun gate dependency build scripts behind a manifest
    /// allowlist runner won't write, so the flag cannot express it.
    ForceOn,
}
