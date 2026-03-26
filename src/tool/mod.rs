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

/// Bun JavaScript runtime and package manager.
pub(crate) mod bun;
/// Bundler, the Ruby dependency manager (`Gemfile`).
pub(crate) mod bundler;
/// Cargo, the Rust package manager and build tool (`Cargo.toml`).
pub(crate) mod cargo_pm;
/// Composer, the PHP dependency manager (`composer.json`).
pub(crate) mod composer;
/// Deno JavaScript/TypeScript runtime (`deno.json` / `deno.jsonc`).
pub(crate) mod deno;
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
/// Pipenv, a Python dependency manager (`Pipfile`).
pub(crate) mod pipenv;
/// pnpm, a fast Node.js package manager (`pnpm-lock.yaml`).
pub(crate) mod pnpm;
/// Poetry, a Python dependency manager (`poetry.lock`).
pub(crate) mod poetry;
/// Shared Python tooling constants.
pub(crate) mod python;
/// Turborepo monorepo build system (`turbo.json`).
pub(crate) mod turbo;
/// uv, a fast Python package manager (`uv.lock`).
pub(crate) mod uv;
/// Yarn, a Node.js package manager (`yarn.lock`).
pub(crate) mod yarn;

#[cfg(test)]
pub(crate) mod test_support;
