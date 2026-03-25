//! Per-tool modules: detection, task extraction, and command building.
//!
//! Each module corresponds to a single tool (package manager or task runner)
//! and exposes a consistent set of public functions:
//!
//! - `detect(dir)` — returns `true` if the tool's config/lockfile exists
//! - `extract_tasks(dir)` — parses config and returns task names (task runners)
//! - `run_cmd(task, args)` — builds a [`std::process::Command`] to run a task
//! - `install_cmd(frozen)` — builds a [`std::process::Command`] to install deps
//! - `exec_cmd(args)` — builds a [`std::process::Command`] for ad-hoc execution
//! - `CLEAN_DIRS` — directories to remove on `runner clean`
//!
//! Not every module exposes every function; only what the tool supports.

/// Bun JavaScript runtime and package manager.
pub mod bun;
/// Bundler, the Ruby dependency manager (`Gemfile`).
pub mod bundler;
/// Cargo, the Rust package manager and build tool (`Cargo.toml`).
pub mod cargo_pm;
/// Composer, the PHP dependency manager (`composer.json`).
pub mod composer;
/// Deno JavaScript/TypeScript runtime (`deno.json` / `deno.jsonc`).
pub mod deno;
/// Go modules (`go.mod`).
pub mod go_pm;
/// go-task, a task runner using `Taskfile.yml`.
pub mod go_task;
/// just, a command runner using `justfile`.
pub mod just;
/// GNU Make (`Makefile`).
pub mod make;
/// mise, a polyglot dev tool manager (`mise.toml`).
pub mod mise;
/// Shared Node.js helpers: `package.json` parsing, script extraction, PM detection.
pub mod node;
/// npm, the default Node.js package manager (`package-lock.json`).
pub mod npm;
/// Nx monorepo build system (`nx.json`).
pub mod nx;
/// Pipenv, a Python dependency manager (`Pipfile`).
pub mod pipenv;
/// pnpm, a fast Node.js package manager (`pnpm-lock.yaml`).
pub mod pnpm;
/// Poetry, a Python dependency manager (`poetry.lock`).
pub mod poetry;
/// Turborepo monorepo build system (`turbo.json`).
pub mod turbo;
/// uv, a fast Python package manager (`uv.lock`).
pub mod uv;
/// Yarn, a Node.js package manager (`yarn.lock`).
pub mod yarn;
