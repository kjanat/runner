//! `runner run <target>` — resolve a task name to the right tool and execute
//! it. When no task matches, fall back to executing the target as an
//! arbitrary command through the detected package manager (formerly `runner
//! exec`).
//!
//! # Module layout
//!
//! - [`qualify`] — `source:task` parsing, reversed-qualifier detection,
//!   and the side-effect-free [`qualify::precheck_task`] used by chain
//!   mode to bail before any sibling task runs.
//! - [`select`] — picking the best [`crate::types::Task`] candidate when
//!   a name matches multiple sources. The ranking key (priority, depth,
//!   display order, alias-ness) is split into individual `pub(crate)`
//!   helpers so [`crate::cmd::why`] can render the same key the dispatcher
//!   used.
//! - [`dispatch`] — turning a task token into a fully-configured
//!   [`std::process::Command`]: warning emission, the resolver chain,
//!   bun-test special case, PM-exec fallback, and per-source `run_cmd`
//!   selection.
//!
//! This file owns only the public entry points ([`run`] for inherited
//! stdio, [`dispatch_task_piped`] for the parallel chain executor) and
//! the test module.

use anyhow::Result;

mod dispatch;
mod qualify;
mod select;

pub(crate) use qualify::{allowed_runner_sources, precheck_task, runner_constraint_error};
pub(crate) use select::{select_task_entry, source_depth, source_priority};

pub(crate) use dispatch::{ResolvedPythonPm, resolve_python_pm};

use crate::resolver::ResolutionOverrides;
use crate::types::ProjectContext;

/// Resolve `task` and run it with inherited stdio, returning the exit
/// code. Bun special case: when `task == "test"` and no package-manifest
/// `test` script exists, falls back to `bun test`. PM-exec fallback for
/// unqualified misses runs the target through `npx`/`bunx`/`pnpm exec`/
/// `deno x`/`uvx`, plus `go run` for Go module/path-shaped targets;
/// otherwise spawns the binary directly from `PATH`.
pub(crate) fn run(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    task: &str,
    args: &[String],
    sink: super::WarningSink<'_>,
) -> Result<i32> {
    let dispatch = dispatch::resolve_dispatch(ctx, overrides, task, args, sink, true)?;
    // Wrap the child's output in a collapsible GitHub Actions group
    // (`runner: <task>`) when enabled. Opened after resolution so the `→`
    // dispatch arrow stays visible above the fold and a resolver error
    // never leaves an empty group; the guard closes the group on drop.
    let _group = super::task_group(overrides, task);
    match dispatch {
        dispatch::Dispatch::Spawn(mut cmd) => Ok(super::exit_code(cmd.status()?)),
        dispatch::Dispatch::DenoSelfExec(self_exec) => self_exec.run(),
    }
}

/// Resolve `task` and spawn it with piped stdout/stderr (so the caller
/// can multiplex output) and `Stdio::null()` stdin (so parallel
/// siblings don't compete for the parent TTY or interfere with each
/// other's terminal modes). Used by the parallel chain executor
/// (`chain::exec::run_parallel`).
pub(crate) fn dispatch_task_piped(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    task: &str,
    args: &[String],
    sink: super::WarningSink<'_>,
) -> Result<std::process::Child> {
    use std::process::Stdio;

    // Chain mode disables deno self-exec (in-process execution can't be
    // piped/spawned as a child), so resolution always yields a Command.
    match dispatch::resolve_dispatch(ctx, overrides, task, args, sink, false)? {
        dispatch::Dispatch::Spawn(mut cmd) => {
            cmd.stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            Ok(cmd.spawn()?)
        }
        dispatch::Dispatch::DenoSelfExec(_) => {
            anyhow::bail!("internal: deno self-exec is not available in chain mode")
        }
    }
}
#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::dispatch::should_use_bun_test_fallback;
    use super::qualify::{detect_reversed_qualifier, parse_qualified_task};
    use super::{precheck_task, select_task_entry};
    use crate::resolver::ResolutionOverrides;
    use crate::tool::test_support::TempDir;
    use crate::types::{PackageManager, ProjectContext, Task, TaskRunner, TaskSource};

    #[test]
    fn parse_qualified_task_splits_source_and_name() {
        let (source, name) = parse_qualified_task("justfile:fmt");
        assert_eq!(source, Some(TaskSource::Justfile));
        assert_eq!(name, "fmt");
    }

    #[test]
    fn parse_qualified_task_returns_bare_name() {
        let (source, name) = parse_qualified_task("build");
        assert_eq!(source, None);
        assert_eq!(name, "build");
    }

    #[test]
    fn parse_qualified_task_handles_unknown_source() {
        let (source, name) = parse_qualified_task("unknown:build");
        assert_eq!(source, None);
        assert_eq!(name, "unknown:build");
    }

    #[test]
    fn parse_qualified_task_with_colons_in_task_name() {
        let (source, name) = parse_qualified_task("package.json:helix:sync");
        assert_eq!(source, Some(TaskSource::PackageJson));
        assert_eq!(name, "helix:sync");
    }

    #[test]
    fn parse_qualified_task_preserves_colons_in_bare_name() {
        let (source, name) = parse_qualified_task("helix:sync");
        assert_eq!(source, None);
        assert_eq!(name, "helix:sync");
    }

    #[test]
    fn parse_qualified_task_accepts_turbo_jsonc_qualifier() {
        let (source, name) = parse_qualified_task("turbo.jsonc:build");
        assert_eq!(source, Some(TaskSource::TurboJson));
        assert_eq!(name, "build");
    }

    #[test]
    fn parse_qualified_task_accepts_deno_jsonc_qualifier() {
        let (source, name) = parse_qualified_task("deno.jsonc:test");
        assert_eq!(source, Some(TaskSource::DenoJson));
        assert_eq!(name, "test");
    }

    #[test]
    fn parse_qualified_task_accepts_bacon_toml_qualifier() {
        let (source, name) = parse_qualified_task("bacon.toml:check");
        assert_eq!(source, Some(TaskSource::BaconToml));
        assert_eq!(name, "check");
    }

    #[test]
    fn detect_reversed_qualifier_catches_task_colon_source() {
        // `lint:cargo` has the qualifier inverted — caller should bail
        // with `did you mean "cargo:lint"?` instead of falling through
        // to PM-exec and spawning a binary named `lint:cargo`.
        let got = detect_reversed_qualifier("lint:cargo");
        assert_eq!(got, Some((TaskSource::CargoAliases, "lint")));
    }

    #[test]
    fn detect_reversed_qualifier_returns_none_for_correct_syntax() {
        // Correct ordering — the prefix branch (`parse_qualified_task`)
        // handles this; the reversed-detector must not fire.
        assert!(detect_reversed_qualifier("cargo:lint").is_none());
        // Plain name, no colon.
        assert!(detect_reversed_qualifier("lint").is_none());
        // Suffix that is not a known source.
        assert!(detect_reversed_qualifier("lint:zoot").is_none());
    }

    #[test]
    fn detect_reversed_qualifier_matches_last_colon() {
        // Multi-colon with a recognized suffix still fires: hint the
        // user toward the canonical ordering. Anything else (suffix not
        // a source label) returns None and falls through to the
        // existing PM-exec / not-found path.
        let got = detect_reversed_qualifier("foo:bar:cargo");
        assert_eq!(got, Some((TaskSource::CargoAliases, "foo:bar")));
        assert!(detect_reversed_qualifier("lint:cargo:extra").is_none());
    }

    #[test]
    fn precheck_reversed_qualifier_beats_runner_constraint() {
        let ctx = context(vec![], vec![]);
        let overrides = ResolutionOverrides {
            prefer_runners: vec![TaskRunner::Just],
            ..ResolutionOverrides::default()
        };

        let err = precheck_task(&ctx, &overrides, "lint:cargo")
            .expect_err("reversed qualifier should fail precheck");

        assert!(format!("{err:#}").contains("cargo:lint"));
    }

    #[test]
    fn reversed_qualifier_fast_fail_does_not_block_real_tasks() {
        // The fast-fail in `resolve_dispatch` is gated by
        // `restricted.is_empty()` — a real task whose name happens to
        // match the `task:source` shape must still dispatch.
        //
        // We mirror the dispatch lookup directly: `parse_qualified_task`
        // returns `(None, original)` for an unknown prefix, then the
        // filter on `ctx.tasks` runs. If that filter is non-empty,
        // `resolve_dispatch` skips the empty-branch entirely and
        // `detect_reversed_qualifier` is never reached.
        let ctx = ProjectContext {
            root: PathBuf::from("/tmp/has-quirky-task-name"),
            package_managers: Vec::new(),
            task_runners: Vec::new(),
            tasks: vec![Task {
                name: "lint:cargo".to_string(),
                source: TaskSource::Justfile,
                run_target: None,
                description: None,
                alias_of: None,
                passthrough_to: None,
            }],
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        };

        let (qualifier, task_name) = parse_qualified_task("lint:cargo");
        assert_eq!(qualifier, None);
        assert_eq!(task_name, "lint:cargo");

        let found: Vec<_> = ctx.tasks.iter().filter(|t| t.name == task_name).collect();
        assert_eq!(
            found.len(),
            1,
            "real task named `lint:cargo` must be reachable; \
             fast-fail only fires when the filter is empty",
        );
        assert_eq!(found[0].source, TaskSource::Justfile);
    }

    #[test]
    fn bun_test_fallback_enabled_when_resolved_to_bun() {
        let ctx = context(vec![PackageManager::Bun], vec![]);

        // The resolver would return Bun via Lockfile for ctx=[Bun].
        assert!(should_use_bun_test_fallback(
            &ctx,
            Some(PackageManager::Bun),
            "test"
        ));
    }

    #[test]
    fn bun_test_fallback_disabled_when_test_script_exists() {
        let ctx = context(
            vec![PackageManager::Bun],
            vec![Task {
                name: "test".to_string(),
                source: TaskSource::PackageJson,
                run_target: None,
                description: None,
                alias_of: None,
                passthrough_to: None,
            }],
        );

        assert!(!should_use_bun_test_fallback(
            &ctx,
            Some(PackageManager::Bun),
            "test"
        ));
    }

    #[test]
    fn bun_test_fallback_disabled_for_other_package_managers() {
        let ctx = context(vec![PackageManager::Npm], vec![]);

        assert!(!should_use_bun_test_fallback(
            &ctx,
            Some(PackageManager::Npm),
            "test"
        ));
    }

    #[test]
    fn bun_test_fallback_disabled_for_non_test_task() {
        let ctx = context(vec![PackageManager::Bun], vec![]);

        assert!(!should_use_bun_test_fallback(
            &ctx,
            Some(PackageManager::Bun),
            "build"
        ));
    }

    #[test]
    fn bun_test_fallback_suppressed_when_resolver_returns_non_bun() {
        // `--pm npm` against a Bun-detected project: the resolver
        // returns Npm (override wins), so the fallback must not fire.
        let ctx = context(vec![PackageManager::Bun], vec![]);

        assert!(!should_use_bun_test_fallback(
            &ctx,
            Some(PackageManager::Npm),
            "test"
        ));
    }

    #[test]
    fn bun_test_fallback_disabled_when_resolver_returns_none() {
        // Resolver errored (--fallback=error with no signal) → no
        // fallback. Even though ctx says Bun, the caller already
        // collapsed the error to None.
        let ctx = context(vec![PackageManager::Bun], vec![]);

        assert!(!should_use_bun_test_fallback(&ctx, None, "test"));
    }

    #[test]
    fn bun_test_fallback_enabled_when_resolver_picks_bun_with_no_lockfile() {
        // `--pm bun` against an empty ctx: resolver returns Bun despite
        // no detected PM, so the fallback fires.
        let ctx = context(vec![], vec![]);

        assert!(should_use_bun_test_fallback(
            &ctx,
            Some(PackageManager::Bun),
            "test"
        ));
    }

    #[test]
    fn source_depth_walks_upward_for_non_node_sources() {
        // Every source consults `tool::files::find_first_upwards`, so a
        // Makefile two levels up resolves with a finite depth (and thus
        // beats a hypothetical sibling resolved at MAX).
        let dir = TempDir::new("source-depth-upward");
        let nested = dir.path().join("apps").join("api");
        fs::create_dir_all(&nested).expect("nested dir should be created");
        fs::write(dir.path().join("Makefile"), "build:\n\techo build\n")
            .expect("root Makefile should be written");

        let ctx = ProjectContext {
            root: nested,
            package_managers: Vec::new(),
            task_runners: Vec::new(),
            tasks: Vec::new(),
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        };

        let depth = super::source_depth(&ctx, TaskSource::Makefile);
        assert_ne!(depth, usize::MAX, "Makefile two levels up should resolve");
    }

    #[test]
    fn source_depth_treats_subdirectory_config_as_depth_zero() {
        // `.cargo/config.toml` sits *inside* root (parent dir is
        // `<root>/.cargo`), not as an ancestor. The ancestors() walk
        // never matches it, so without the subdir-fallback the depth
        // would collapse to `usize::MAX` and any root-level source
        // (`bacon.toml`, `Makefile`, …) would win every tiebreak by
        // default — robbing `display_order` of the tie-break it was
        // designed to perform.
        let dir = TempDir::new("source-depth-subdirectory");
        let cargo_dir = dir.path().join(".cargo");
        fs::create_dir_all(&cargo_dir).expect(".cargo dir should be created");
        fs::write(
            cargo_dir.join("config.toml"),
            "[alias]\nlint = \"clippy\"\n",
        )
        .expect("config.toml should be written");

        let ctx = ProjectContext {
            root: dir.path().to_path_buf(),
            package_managers: Vec::new(),
            task_runners: Vec::new(),
            tasks: Vec::new(),
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        };

        let depth = super::source_depth(&ctx, TaskSource::CargoAliases);
        assert_eq!(
            depth, 0,
            ".cargo/config.toml is a subdir of root → treat as depth 0",
        );
    }

    #[test]
    fn cargo_aliases_beats_bacon_toml_for_same_name_task() {
        // Once both sources resolve to depth 0 (cargo via the subdir
        // fallback, bacon via root-level match), the `display_order`
        // tiebreak should pick cargo (6) over bacon (7). This is what
        // the user expected when their `.cargo/config.toml` alias for
        // `lint` was being silently overridden by `bacon.toml`'s
        // `[jobs.lint]` + `default_job = "lint"`.
        let dir = TempDir::new("priority-cargo-vs-bacon");
        let cargo_dir = dir.path().join(".cargo");
        fs::create_dir_all(&cargo_dir).expect(".cargo dir should be created");
        fs::write(
            cargo_dir.join("config.toml"),
            "[alias]\nlint = \"clippy\"\n",
        )
        .expect("config.toml should be written");
        fs::write(
            dir.path().join("bacon.toml"),
            "[jobs.lint]\ncommand = [\"cargo\", \"clippy\"]\n",
        )
        .expect("bacon.toml should be written");

        let tasks = vec![
            Task {
                name: "lint".to_string(),
                source: TaskSource::BaconToml,
                run_target: None,
                description: None,
                alias_of: None,
                passthrough_to: None,
            },
            Task {
                name: "lint".to_string(),
                source: TaskSource::CargoAliases,
                run_target: None,
                description: None,
                alias_of: None,
                passthrough_to: None,
            },
        ];
        let ctx = ProjectContext {
            root: dir.path().to_path_buf(),
            package_managers: Vec::new(),
            task_runners: Vec::new(),
            tasks,
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        };

        let candidates: Vec<&Task> = ctx.tasks.iter().collect();
        let entry = select_task_entry(&ctx, &ResolutionOverrides::default(), &candidates);
        assert_eq!(
            entry.source,
            TaskSource::CargoAliases,
            "display_order should pick CargoAliases over BaconToml once both hit depth 0",
        );
    }

    #[test]
    fn select_task_entry_prefers_package_json_over_deno_json() {
        let dir = TempDir::new("run-deno-nearest");
        let nested = dir.path().join("apps").join("site").join("src");
        fs::create_dir_all(&nested).expect("nested dir should be created");
        fs::write(
            dir.path().join("deno.jsonc"),
            r#"{ tasks: { build: "deno task build" } }"#,
        )
        .expect("root deno.jsonc should be written");
        fs::write(
            dir.path().join("apps").join("site").join("package.json"),
            r#"{ "scripts": { "build": "deno task build" } }"#,
        )
        .expect("member package.json should be written");
        let ctx = ProjectContext {
            root: nested,
            package_managers: vec![PackageManager::Deno],
            task_runners: Vec::new(),
            tasks: vec![
                Task {
                    name: "build".to_string(),
                    source: TaskSource::DenoJson,
                    run_target: None,
                    description: None,
                    alias_of: None,
                    passthrough_to: None,
                },
                Task {
                    name: "build".to_string(),
                    source: TaskSource::PackageJson,
                    run_target: None,
                    description: None,
                    alias_of: None,
                    passthrough_to: None,
                },
            ],
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        };

        let found: Vec<_> = ctx.tasks.iter().collect();
        let overrides = ResolutionOverrides::default();
        let entry = select_task_entry(&ctx, &overrides, &found);

        assert_eq!(entry.source, TaskSource::PackageJson);
    }

    fn context(package_managers: Vec<PackageManager>, tasks: Vec<Task>) -> ProjectContext {
        ProjectContext {
            root: PathBuf::from("."),
            package_managers,
            task_runners: Vec::new(),
            tasks,
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        }
    }

    fn task(name: &str, source: TaskSource) -> Task {
        Task {
            name: name.to_string(),
            source,
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: None,
        }
    }

    #[test]
    fn prefer_runners_reorders_default_tier() {
        // Default priority would pick TurboJson first; `prefer = [just]`
        // promotes the Justfile candidate above it.
        let ctx = context(
            vec![],
            vec![
                task("build", TaskSource::TurboJson),
                task("build", TaskSource::Justfile),
            ],
        );
        let found: Vec<_> = ctx.tasks.iter().collect();
        let overrides = ResolutionOverrides {
            prefer_runners: vec![TaskRunner::Just],
            ..ResolutionOverrides::default()
        };
        let entry = select_task_entry(&ctx, &overrides, &found);

        assert_eq!(entry.source, TaskSource::Justfile);
    }

    #[test]
    fn runner_override_promotes_just_over_turbo() {
        // `--runner just` restricts candidates; `select_task_entry` is
        // called after `run()` filters by the constraint, but with no
        // constraint helper here we exercise the priority directly.
        let ctx = context(
            vec![],
            vec![
                task("build", TaskSource::TurboJson),
                task("build", TaskSource::Justfile),
            ],
        );
        // Only the Justfile candidate survives the constraint.
        let found: Vec<&Task> = ctx
            .tasks
            .iter()
            .filter(|t| t.source == TaskSource::Justfile)
            .collect();
        let overrides = ResolutionOverrides::default();
        let entry = select_task_entry(&ctx, &overrides, &found);

        assert_eq!(entry.source, TaskSource::Justfile);
    }
}
