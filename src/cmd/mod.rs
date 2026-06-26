//! Subcommand implementations: info, run, install, clean, list, completions.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use colored::Colorize;

use crate::resolver::ResolutionOverrides;
use crate::types::{DetectionWarning, ProjectContext};

mod clean;
mod completions;
mod config;
mod doctor;
mod info;
pub(crate) mod install;
mod list;
#[cfg(feature = "man")]
mod man;
pub(crate) mod run;
#[cfg(feature = "schema")]
mod schema;
mod why;

pub(crate) use clean::clean;
pub(crate) use completions::{completions, parse_shell_arg};
pub(crate) use config::config;
pub(crate) use doctor::doctor;
pub(crate) use info::info;
pub(crate) use install::install;
pub(crate) use list::list;
#[cfg(feature = "man")]
pub(crate) use man::{write_man_pages, write_runner_page_to_stdout};
pub(crate) use run::run;
#[cfg(feature = "schema")]
pub(crate) use schema::write_schema;
pub(crate) use why::why;

/// Shared setup for every spawned task: project-local `node_modules/.bin`
/// dirs on the child `PATH`, working directory, inherited stdio.
fn configure_command(command: &mut Command, dir: &Path, overrides: &ResolutionOverrides) {
    prepend_node_bin_path(command, dir);
    command
        .current_dir(dir)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    // Mark children (and, by env inheritance, all descendants) when this
    // runner opens a GHA group around them, so a nested `runner`/`run`
    // suppresses its own group — GHA groups don't nest. When we're already
    // nested the marker is in our inherited env, so children get it for free.
    if emits_group(overrides) {
        command.env(GROUP_ACTIVE_ENV, "1");
    }
}

/// Every existing `node_modules/.bin` from `dir` up to the filesystem
/// root, nearest first — the same set (and order) `npm run` exposes to
/// `package.json` scripts. Levels without an installed `.bin` are
/// skipped, so non-Node projects collect nothing and the whole
/// augmentation becomes a no-op.
fn node_bin_dirs(dir: &Path) -> Vec<PathBuf> {
    dir.ancestors()
        .map(|ancestor| ancestor.join("node_modules").join(".bin"))
        .filter(|bin| bin.is_dir())
        .collect()
}

/// Prepend the project's `node_modules/.bin` dirs to the child's `PATH`.
///
/// Node PMs inject this for `package.json` scripts, but runner spawns
/// tasks directly (`turbo run <task>`, the bare-binary fallback), so a
/// devDependency-only binary would die with ENOENT. The OS honors a
/// `PATH` set on the [`Command`] itself, so prepending fixes both the
/// spawn and anything the task launches in turn.
///
/// Entries are not deduplicated against the parent `PATH`: prepending
/// unconditionally gives local bins priority over global installs.
fn prepend_node_bin_path(command: &mut Command, dir: &Path) {
    let bins = node_bin_dirs(dir);
    if bins.is_empty() {
        return;
    }
    #[cfg(windows)]
    resolve_program_in_bins(command, &bins);
    if let Some(path) = prepended_path(&bins, std::env::var_os("PATH").as_deref()) {
        command.env("PATH", path);
    }
}

/// `bins` followed by the entries of `parent`, joined with the platform
/// separator. `None` when joining fails (a bin dir embeds the separator
/// itself) — the caller leaves `PATH` untouched rather than corrupt it.
fn prepended_path(bins: &[PathBuf], parent: Option<&OsStr>) -> Option<OsString> {
    let inherited = parent.map(std::env::split_paths).into_iter().flatten();
    std::env::join_paths(bins.iter().cloned().chain(inherited)).ok()
}

/// Re-resolve a bare program name against the project's bin dirs.
///
/// [`crate::tool::program::command`] resolves bare names against the
/// parent `PATH`×`PATHEXT` before the bin dirs are prepended, and the
/// std child-`PATH` search only appends `.exe` at spawn time — so a
/// `turbo.cmd`/`.ps1` shim living only under `node_modules/.bin` would
/// fail to spawn. When a bare name resolves inside `bins`, rebuild the
/// command around the absolute shim path, preserving args and env.
/// Absolute/relative programs and parent-`PATH` hits are left alone
/// (so a global install still shadows a local one here, unlike Unix).
#[cfg(windows)]
fn resolve_program_in_bins(command: &mut Command, bins: &[PathBuf]) {
    let program = command.get_program().to_os_string();
    let Some(name) = program.to_str() else { return };
    if Path::new(name).components().count() > 1 {
        return;
    }
    let Ok(joined) = std::env::join_paths(bins.iter().cloned()) else {
        return;
    };
    let pathext =
        std::env::var_os("PATHEXT").unwrap_or_else(|| crate::tool::program::DEFAULT_PATHEXT.into());
    let Some(resolved) = crate::tool::program::resolve_windows(name, &joined, &pathext) else {
        return;
    };

    let args: Vec<OsString> = command.get_args().map(ToOwned::to_owned).collect();
    let envs: Vec<(OsString, Option<OsString>)> = command
        .get_envs()
        .map(|(key, value)| (key.to_owned(), value.map(ToOwned::to_owned)))
        .collect();
    let cwd = command.get_current_dir().map(Path::to_path_buf);

    let mut next = Command::new(resolved);
    next.args(args);
    for (key, value) in envs {
        match value {
            Some(value) => {
                next.env(key, value);
            }
            None => {
                next.env_remove(key);
            }
        }
    }
    if let Some(cwd) = cwd {
        next.current_dir(cwd);
    }
    *command = next;
}

pub(crate) fn exit_code(status: ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;

        if let Some(code) = status.code() {
            return code;
        }
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }

    status.code().unwrap_or(1)
}

/// Env marker a runner sets on its children when it is in GitHub Actions
/// grouping mode ([`emits_group`]), so a nested `runner`/`run` (e.g. invoked
/// through an `npm` script) detects it and stays silent instead of emitting a
/// second `::group::` that would corrupt the parent's fold. Inherited
/// transitively through intermediate processes; read into
/// [`ResolutionOverrides::parent_group_open`].
///
/// The contract is "a parent is *collecting* your output — don't open your own
/// group", which is slightly broader than "a literal `::group::` is open right
/// now": it is also set in parallel-*streaming* mode, where the parent muxes
/// child output behind a `[task] ` prefix instead of a group. Suppressing
/// nested grouping is correct in every case — a nested group would otherwise
/// either nest-and-corrupt (grouped) or render as inert prefixed text
/// (streaming).
pub(crate) const GROUP_ACTIVE_ENV: &str = "RUNNER_GROUP_ACTIVE";

/// Whether to wrap a run in a GitHub Actions log group: only when the user
/// hasn't opted out (`[github].group_output`) *and* we're under GitHub
/// Actions, so `::group::` markers never leak into a normal terminal.
const fn should_group(group_output: bool, under_github_actions: bool) -> bool {
    group_output && under_github_actions
}

/// Whether *this* runner emits a GitHub Actions group around its children:
/// grouping is on, we're under Actions, and a parent runner hasn't already
/// opened one ([`ResolutionOverrides::parent_group_open`]). When true, the
/// group-opening sites fire AND children are marked with [`GROUP_ACTIVE_ENV`]
/// so a nested runner suppresses its own groups. When false, no group is
/// opened (nested output flows into the parent's group, or grouping is off).
pub(crate) fn emits_group(overrides: &ResolutionOverrides) -> bool {
    should_group(overrides.group_output, actions_rs::env::is_github_actions())
        && !overrides.parent_group_open
}

/// Open a collapsible GitHub Actions log group titled `runner: {name}` when
/// grouping is enabled (see [`should_group`]).
///
/// The returned [`actions_rs::log::GroupGuard`] emits `::endgroup::` when it
/// is dropped — including on the `?` error path and on panic — so callers
/// just bind it for the duration of the run. Returns `None` (emitting
/// nothing) when grouping is off, which lets callers hold it unconditionally.
fn task_group(overrides: &ResolutionOverrides, name: &str) -> Option<actions_rs::log::GroupGuard> {
    emits_group(overrides).then(|| actions_rs::log::group_guard(format!("runner: {name}")))
}

/// Optional warning collector. `None` means "emit warnings to stderr
/// directly" (single-task path). `Some(set)` means "stash for deduped
/// emission later" (chain dispatch — chain executor emits the deduped
/// set once at the end).
pub(crate) type WarningSink<'a> = Option<&'a mut std::collections::HashSet<DetectionWarning>>;

fn print_warnings(ctx: &ProjectContext, overrides: &ResolutionOverrides, sink: WarningSink<'_>) {
    print_warning_slice(&ctx.warnings, overrides, sink);
}

fn print_warning_slice(
    warnings: &[DetectionWarning],
    overrides: &ResolutionOverrides,
    sink: WarningSink<'_>,
) {
    if overrides.no_warnings {
        return;
    }
    if let Some(set) = sink {
        for warning in warnings {
            set.insert(warning.clone());
        }
        return;
    }
    for warning in warnings {
        eprintln!("{} {warning}", "warn:".yellow().bold());
    }
}

/// Emit a previously-collected warning set to stderr. Used by the chain
/// executor after all per-task resolutions have populated the sink.
///
/// Sorted by `Display` form before emission so output is stable across
/// runs — `HashSet` iteration order is unspecified, which made the
/// warning block jump around between invocations of the same chain.
pub(crate) fn emit_collected_warnings(
    warnings: &std::collections::HashSet<DetectionWarning>,
    overrides: &ResolutionOverrides,
) {
    if overrides.no_warnings {
        return;
    }
    let mut sorted: Vec<(String, &DetectionWarning)> =
        warnings.iter().map(|w| (w.to_string(), w)).collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, warning) in sorted {
        eprintln!("{} {warning}", "warn:".yellow().bold());
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    use super::{configure_command, emits_group, node_bin_dirs, prepended_path};
    use crate::resolver::ResolutionOverrides;
    use crate::tool::test_support::TempDir;

    #[test]
    fn emits_group_false_when_nested_under_parent_group() {
        // A parent runner's already-open group (parent_group_open) suppresses
        // this runner's grouping regardless of the other signals — GHA groups
        // don't nest, so a nested `::endgroup::` would close the parent's early.
        let overrides = ResolutionOverrides {
            group_output: true,
            parent_group_open: true,
            ..ResolutionOverrides::default()
        };
        assert!(!emits_group(&overrides));
    }

    #[test]
    fn configure_command_sets_current_dir() {
        let dir = std::env::temp_dir();
        let mut command = Command::new("runner-test-command");

        configure_command(&mut command, dir.as_path(), &ResolutionOverrides::default());

        assert_eq!(command.get_current_dir(), Some(dir.as_path()));
    }

    #[test]
    fn node_bin_dirs_walks_ancestors_nearest_first() {
        let dir = TempDir::new("node-bin-walk");
        let member = dir.path().join("apps").join("web");
        let member_bin = member.join("node_modules").join(".bin");
        let root_bin = dir.path().join("node_modules").join(".bin");
        fs::create_dir_all(&member_bin).expect("member bin should be created");
        fs::create_dir_all(&root_bin).expect("root bin should be created");

        let bins = node_bin_dirs(&member);

        // `apps/` has no node_modules — levels without an installed
        // `.bin` are skipped, not invented. Entries past the temp root
        // (a stray `/tmp/node_modules`) are out of our control, so only
        // pin the leading order and that nothing else came from inside
        // the fixture.
        assert_eq!(&bins[..2], [member_bin, root_bin]);
        assert!(bins.iter().skip(2).all(|bin| !bin.starts_with(dir.path())));
    }

    #[test]
    fn node_bin_dirs_requires_bin_subdir() {
        // A `node_modules` without `.bin` (no dependencies expose
        // binaries) must not contribute a phantom PATH entry.
        let dir = TempDir::new("node-bin-missing");
        fs::create_dir_all(dir.path().join("node_modules")).expect("dir should be created");

        let bins = node_bin_dirs(dir.path());

        assert!(bins.iter().all(|bin| !bin.starts_with(dir.path())));
    }

    #[test]
    fn prepended_path_orders_bins_before_parent() {
        let bins = vec![
            PathBuf::from("/repo/apps/web/node_modules/.bin"),
            PathBuf::from("/repo/node_modules/.bin"),
        ];
        let parent = OsString::from("/usr/bin");

        let joined = prepended_path(&bins, Some(parent.as_os_str()))
            .expect("plain paths should always join");

        let parts: Vec<PathBuf> = std::env::split_paths(&joined).collect();
        assert_eq!(
            parts,
            [
                PathBuf::from("/repo/apps/web/node_modules/.bin"),
                PathBuf::from("/repo/node_modules/.bin"),
                PathBuf::from("/usr/bin"),
            ],
        );
    }

    #[test]
    fn prepended_path_handles_missing_parent() {
        let bins = vec![PathBuf::from("/repo/node_modules/.bin")];

        let joined = prepended_path(&bins, None).expect("plain paths should always join");

        let parts: Vec<PathBuf> = std::env::split_paths(&joined).collect();
        assert_eq!(parts, [PathBuf::from("/repo/node_modules/.bin")]);
    }

    #[cfg(unix)]
    #[test]
    fn spawn_resolves_dev_dependency_binary_via_child_path() {
        use std::os::unix::fs::PermissionsExt as _;

        // End-to-end pin for the mechanism the PATH fix relies on: the
        // OS-level bare-name lookup must honor the PATH set on the
        // child Command (std documents this on `Command::new`). A
        // devDependency-style shim that exists only under the project's
        // `node_modules/.bin` has to spawn — this is exactly the
        // "turbo.json task dies with ENOENT because turbo is only a
        // devDependency" report.
        let dir = TempDir::new("child-path-spawn");
        let bin = dir.path().join("node_modules").join(".bin");
        fs::create_dir_all(&bin).expect("bin dir should be created");
        let shim = bin.join("runner-test-shim");
        fs::write(&shim, "#!/bin/sh\nexit 42\n").expect("shim should be written");
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o755))
            .expect("shim should be marked executable");

        let mut command = Command::new("runner-test-shim");
        configure_command(&mut command, dir.path(), &ResolutionOverrides::default());

        let status = command
            .status()
            .expect("shim should spawn via the child PATH");
        assert_eq!(status.code(), Some(42));
    }

    #[cfg(windows)]
    #[test]
    fn configure_command_resolves_cmd_shim_from_bin_dir() {
        use std::ffi::OsStr;

        // `CreateProcessW` never consults PATHEXT and the std child-PATH
        // search only appends `.exe`, so a bare name backed only by a
        // `.cmd` shim in node_modules/.bin must be rebuilt around the
        // absolute shim path — with args and env tweaks surviving.
        let dir = TempDir::new("win-bin-shim");
        let bin = dir.path().join("node_modules").join(".bin");
        fs::create_dir_all(&bin).expect("bin dir should be created");
        let shim = bin.join("runner-test-shim.cmd");
        fs::write(&shim, "@echo off\r\n").expect("shim should be written");

        let mut command = Command::new("runner-test-shim");
        command.arg("run").env("RUNNER_TEST_MARKER", "1");
        configure_command(&mut command, dir.path(), &ResolutionOverrides::default());

        assert_eq!(PathBuf::from(command.get_program()), shim);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, [OsStr::new("run")]);
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == "RUNNER_TEST_MARKER" && value == Some(OsStr::new("1"))),
        );
    }

    #[test]
    fn no_warnings_suppresses_emission() {
        use super::print_warning_slice;
        use crate::resolver::ResolutionOverrides;
        use crate::types::{DetectionWarning, PackageManager};

        // Smoke: print_warning_slice with no_warnings=true must
        // short-circuit before the eprintln. The test asserts no
        // panic / no observable side effects; capturing stderr in
        // cargo test is fiddly and not worth a fixture.
        let warnings = vec![DetectionWarning::PmMismatch {
            declared: PackageManager::Pnpm,
            field: "packageManager",
            lockfile: PackageManager::Yarn,
        }];
        let overrides = ResolutionOverrides {
            no_warnings: true,
            ..ResolutionOverrides::default()
        };
        print_warning_slice(&warnings, &overrides, None);
    }

    #[cfg(unix)]
    #[test]
    fn exit_code_preserves_signal_status() {
        use std::os::unix::process::ExitStatusExt as _;

        use super::exit_code;

        assert_eq!(exit_code(std::process::ExitStatus::from_raw(5 << 8)), 5);
        assert_eq!(exit_code(std::process::ExitStatus::from_raw(2)), 130);
    }

    #[test]
    fn should_group_requires_both_opt_in_and_github_actions() {
        use super::should_group;

        assert!(should_group(true, true));
        assert!(!should_group(false, true), "config opt-out wins");
        assert!(
            !should_group(true, false),
            "no grouping outside GitHub Actions"
        );
        assert!(!should_group(false, false));
    }
}
