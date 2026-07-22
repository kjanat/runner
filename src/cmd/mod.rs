//! Subcommand implementations: info, run, install, clean, list, completions.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use colored::Colorize;

use crate::resolver::ResolutionOverrides;
use crate::types::{DetectionWarning, ProjectContext, TaskSource};

mod clean;
mod completions;
mod config;
mod doctor;
mod info;
pub(crate) mod install;
mod list;
#[cfg(feature = "lsp")]
pub(crate) mod lsp;
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
pub(crate) use schema::config_schema;
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
    // suppresses its own group. GHA groups don't nest. When we're already
    // nested the marker is in our inherited env, so children get it for free.
    if emits_group(overrides) {
        command.env(GROUP_ACTIVE_ENV, "1");
    }
    // Same idea for warnings: this process has already printed (or suppressed)
    // everything detection found for `dir` by the time it spawns anything, so a
    // nested runner over the same root stays quiet instead of repeating it.
    command.env(WARNED_ROOT_ENV, dir);
    // Propagate the *global* resolved verbosity across the process boundary so
    // a task that shells out to `runner` again inherits it, the way
    // `RUNNER_QUIET` already did by env inheritance but the `-q` flag did not.
    // Exported as the numeric level and the stream label, both re-parsed by the
    // child's env layer. Only set when non-default so we never force
    // quiet/stderr onto a child whose own config or flags meant to leave them
    // alone.
    //
    // Deliberately the global level, not a per-task
    // `[tasks.<name>].verbosity`: per-task verbosity is scoped to *this*
    // process's immediate host tool (it only shapes the spawned tool's flags,
    // not this runner's own output), so crossing it into a nested runner —
    // where it would become that runner's global level and mute *its* warnings
    // at `very-quiet`+ — would be a surprising, asymmetric blast radius. A
    // caller who wants nested runners quiet uses `-q`/`RUNNER_QUIET`, which is
    // global by construction.
    if overrides.quiet_level != crate::tool::QuietLevel::Off {
        command.env("RUNNER_QUIET", overrides.quiet_level.as_count().to_string());
    }
    if overrides.host_stream != crate::tool::Stream::Inherit {
        command.env("RUNNER_HOST_STREAM", overrides.host_stream.label());
    }
    // Same reasoning for the runtime axis: a script that shells out to
    // `runner` again should keep running on the runtime the user asked for,
    // or the nested dispatch silently drops back to the detected PM.
    if let Some(over) = overrides.runtime.as_ref() {
        command.env("RUNNER_RUNTIME", over.runtime.label());
    }
}

/// Every existing `node_modules/.bin` from `dir` up to the filesystem
/// root, nearest first, the same set (and order) `npm run` exposes to
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
/// itself). The caller leaves `PATH` untouched rather than corrupt it.
fn prepended_path(bins: &[PathBuf], parent: Option<&OsStr>) -> Option<OsString> {
    let inherited = parent.map(std::env::split_paths).into_iter().flatten();
    std::env::join_paths(bins.iter().cloned().chain(inherited)).ok()
}

/// Re-resolve a bare program name against the project's bin dirs.
///
/// [`crate::tool::program::command`] resolves bare names against the
/// parent `PATH`×`PATHEXT` before the bin dirs are prepended, and the
/// std child-`PATH` search only appends `.exe` at spawn time, so a
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
/// The contract is "a parent is *collecting* your output, don't open your own
/// group", which is slightly broader than "a literal `::group::` is open right
/// now": it is also set in parallel-*streaming* mode, where the parent muxes
/// child output behind a `[task] ` prefix instead of a group. Suppressing
/// nested grouping is correct in every case; a nested group would otherwise
/// either nest-and-corrupt (grouped) or render as inert prefixed text
/// (streaming).
pub(crate) const GROUP_ACTIVE_ENV: &str = "RUNNER_GROUP_ACTIVE";

/// Env marker carrying the project root whose detection warnings a parent
/// `runner`/`run` has already printed. A `package.json` script that calls
/// `runner` again (the common `"fmt": "runner run lint:fix fmt:dprint"` shape)
/// otherwise repeats every warning at every level.
pub(crate) const WARNED_ROOT_ENV: &str = "RUNNER_WARNED_ROOT";

/// Env marker carrying the stack of tasks the ancestor `runner`/`run`
/// processes are currently dispatching, so a package script that calls
/// `runner` again cannot resolve back to itself and fork bomb the machine.
/// Inherited transitively, which is what makes it survive the package
/// manager sitting between two runner processes (`run tsc` → `npm run tsc`
/// → `run tsc`).
pub(crate) const TASK_STACK_ENV: &str = "RUNNER_TASK_STACK";

/// Separators inside [`TASK_STACK_ENV`]: ASCII record/unit separators, which
/// no path or task name can contain.
const FRAME_SEP: char = '\u{1e}';
const FIELD_SEP: char = '\u{1f}';

/// Identify a dispatch by canonical project root plus the qualified task,
/// so the same task in two workspace members (or reached through a symlink)
/// stays two distinct frames.
fn task_frame(root: &Path, source: TaskSource, name: &str) -> String {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    format!(
        "{}{FIELD_SEP}{}{FIELD_SEP}{name}",
        root.to_string_lossy(),
        source.label(),
    )
}

/// The `source:task` form of a frame, which is also the qualified syntax the
/// user can type to pin the offending candidate.
fn frame_label(frame: &str) -> String {
    let mut fields = frame.split(FIELD_SEP).skip(1);
    let source = fields.next().unwrap_or_default();
    let name = fields.next().unwrap_or_default();
    format!("{source}:{name}")
}

fn inherited_task_stack() -> Vec<String> {
    let Ok(raw) = std::env::var(TASK_STACK_ENV) else {
        return Vec::new();
    };
    raw.split(FRAME_SEP)
        .filter(|frame| !frame.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Admit a task into the invocation stack, or reject it as a cycle.
///
/// Returns the [`TASK_STACK_ENV`] value to hand the child on success. The
/// check runs regardless of `--quiet`: quiet suppresses dispatch output,
/// never a safety diagnostic.
pub(crate) fn push_task_frame(
    root: &Path,
    source: TaskSource,
    name: &str,
) -> anyhow::Result<OsString> {
    let pushed = admit_frame(inherited_task_stack(), task_frame(root, source, name))?;
    Ok(OsString::from(pushed.join(&FRAME_SEP.to_string())))
}

/// Pure core of [`push_task_frame`], split from the env read so the cycle
/// rule is unit-testable without mutating the process environment.
fn admit_frame(stack: Vec<String>, frame: String) -> anyhow::Result<Vec<String>> {
    if let Some(start) = stack.iter().position(|seen| *seen == frame) {
        let cycle: Vec<String> = stack[start..]
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(frame.as_str()))
            .map(frame_label)
            .collect();
        anyhow::bail!(
            "recursive task resolution detected: {}\nnote: this task dispatches itself through a \
             nested `runner`/`run`; point the script at the binary, file, or qualified task it \
             actually means",
            cycle.join(" -> "),
        );
    }

    let mut pushed = stack;
    pushed.push(frame);
    Ok(pushed)
}

/// Whether a parent runner already warned about this project.
///
/// Keyed on the root, not merely on the marker's presence: a nested runner
/// pointed at a different directory (`--dir`, a monorepo package) has its own
/// detection to report, and must still report it.
pub(crate) fn parent_warned_about(root: &Path) -> bool {
    let Some(marked) = std::env::var_os(WARNED_ROOT_ENV) else {
        return false;
    };
    same_root(Path::new(&marked), root)
}

/// Compare two roots, preferring canonical paths so a symlinked or
/// `..`-laden spelling of one directory doesn't read as two.
fn same_root(marked: &Path, root: &Path) -> bool {
    match (marked.canonicalize(), root.canonicalize()) {
        (Ok(marked), Ok(root)) => marked == root,
        _ => marked == root,
    }
}

/// Whether to wrap a run in a GitHub Actions log group: only when the user
/// hasn't opted out (`[github].group_output`) *and* we're under GitHub
/// Actions, so `::group::` markers never leak into a normal terminal.
const fn should_group(group_output: bool, under_github_actions: bool) -> bool {
    group_output && under_github_actions
}

/// Why a runner stays silent instead of opening its own Actions group,
/// even where [`should_group`] says yes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupSuppression {
    /// Nothing in the way; open the group.
    None,
    /// A parent runner already opened one, and Actions groups don't nest:
    /// a nested `::endgroup::` closes the parent's fold early.
    ParentGroupOpen,
    /// `--quiet`, so stdout carries only what the child wrote.
    Quiet,
}

const fn suppression(parent_group_open: bool, quiet: bool) -> GroupSuppression {
    if quiet {
        GroupSuppression::Quiet
    } else if parent_group_open {
        GroupSuppression::ParentGroupOpen
    } else {
        GroupSuppression::None
    }
}

/// Pure core of [`emits_group`]: grouping on, under Actions, and nothing
/// suppressing it. Split out from the env read so the gates are
/// unit-testable without a live `GITHUB_ACTIONS` environment.
const fn group_emission(
    group_output: bool,
    under_github_actions: bool,
    suppression: GroupSuppression,
) -> bool {
    should_group(group_output, under_github_actions)
        && matches!(suppression, GroupSuppression::None)
}

/// Whether *this* runner emits a GitHub Actions group around its children:
/// grouping is on, we're under Actions, a parent runner hasn't already
/// opened one ([`ResolutionOverrides::parent_group_open`]), and `--quiet`
/// is off. When true, the group-opening sites fire AND children are marked
/// with [`GROUP_ACTIVE_ENV`] so a nested runner suppresses its own groups.
/// When false, no group is opened (nested output flows into the parent's
/// group, or grouping is off).
///
/// GitHub Actions reads `::group::`/`::endgroup::` off the child's own
/// stdout, so a runner that decorates while `--quiet` is set corrupts any
/// parent capturing that stdout (`npm pack --json` piped into a script).
/// The markers cannot move to stderr instead: GitHub Actions does not
/// preserve relative order between the two streams, so a fold opened there
/// would close around the wrong lines.
pub(crate) fn emits_group(overrides: &ResolutionOverrides) -> bool {
    group_emission(
        overrides.group_output,
        actions_rs::env::is_github_actions(),
        suppression(overrides.parent_group_open, overrides.silences_runner()),
    )
}

/// Open a collapsible GitHub Actions log group titled `runner: {name}` when
/// grouping is enabled (see [`should_group`]).
///
/// The returned [`actions_rs::log::GroupGuard`] emits `::endgroup::` when it
/// is dropped, including on the `?` error path and on panic, so callers
/// just bind it for the duration of the run. Returns `None` (emitting
/// nothing) when grouping is off, which lets callers hold it unconditionally.
fn task_group(overrides: &ResolutionOverrides, name: &str) -> Option<actions_rs::log::GroupGuard> {
    emits_group(overrides).then(|| actions_rs::log::group_guard(format!("runner: {name}")))
}

/// Optional warning collector. `None` means "emit warnings to stderr
/// directly" (single-task path). `Some(set)` means "stash for deduped
/// emission later" (chain dispatch, chain executor emits the deduped
/// set once at the end).
pub(crate) type WarningSink<'a> = Option<&'a mut std::collections::HashSet<DetectionWarning>>;

fn print_warnings(ctx: &ProjectContext, overrides: &ResolutionOverrides, sink: WarningSink<'_>) {
    print_warning_slice(&ctx.warnings, overrides, sink);
}

/// Whether detection warnings stay unsaid: the user asked for silence
/// (`--no-warnings`, or `-qq`+ which folds it in), or a parent runner already
/// said them for this root.
fn silenced(overrides: &ResolutionOverrides) -> bool {
    overrides.silences_warnings() || overrides.parent_warned
}

fn print_warning_slice(
    warnings: &[DetectionWarning],
    overrides: &ResolutionOverrides,
    sink: WarningSink<'_>,
) {
    if silenced(overrides) {
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
/// runs; `HashSet` iteration order is unspecified, which made the
/// warning block jump around between invocations of the same chain.
pub(crate) fn emit_collected_warnings(
    warnings: &std::collections::HashSet<DetectionWarning>,
    overrides: &ResolutionOverrides,
) {
    if silenced(overrides) {
        return;
    }
    let mut sorted: Vec<(String, &DetectionWarning)> =
        warnings.iter().map(|w| (w.to_string(), w)).collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, warning) in sorted {
        eprintln!("{} {warning}", "warn:".yellow().bold());
    }
}

/// Render a [`std::time::Duration`] as a compact, human-readable string for
/// per-task chain timing: sub-second values as whole milliseconds (`342ms`),
/// sub-minute values as seconds with one decimal (`1.2s`), and anything
/// longer as minutes plus zero-padded seconds (`1m 04s`).
pub(crate) fn format_duration(elapsed: std::time::Duration) -> String {
    let millis = elapsed.as_millis();
    if millis < 1000 {
        return format!("{millis}ms");
    }
    // Pick the band from the same rounded tenth-of-a-second that we actually
    // print. Deciding on the truncated whole-second value while rendering a
    // rounded one lets a duration in [59.95s, 60.0s) stay in the seconds band
    // yet round up to a bogus "60.0s"; rounding here promotes it to "1m 00s".
    // Half-up rounding via integer math also keeps this free of the
    // float-to-int cast lints a `(secs_f64 * 10.0).round() as u64` would trip.
    let tenths = (millis + 50) / 100;
    if tenths >= 600 {
        // Round straight from millis to whole seconds (half-up) in one step.
        // The band decision stays on the rounded tenth so a duration in
        // [59.95s, 60.0s) still promotes to "1m 00s"; for any millis in this
        // band `(millis + 500) / 1000 >= 60`, so no "0m"/"60s" can leak.
        // Rounding the already-rounded `tenths` again (`(tenths + 5) / 10`)
        // would cascade two half-ups and shift the seconds boundary from 0.50
        // to 0.45, over-reporting any [0.45s, 0.50s) fraction by a whole
        // second (e.g. 60_450ms would print "1m 01s" instead of "1m 00s").
        let secs = (millis + 500) / 1000;
        return format!("{}m {:02}s", secs / 60, secs % 60);
    }
    format!("{}.{}s", tenths / 10, tenths % 10)
}

/// One-line completion summary shared by every chain output mode, e.g.
/// `finished in 1.2s (exit 0)`. Sequential and live (streaming) parallel
/// output prepend the task name via [`emit_task_timing`]; grouped parallel
/// output folds this summary into each task's block footer.
pub(crate) fn task_timing_summary(elapsed: std::time::Duration, code: i32) -> String {
    format!("finished in {} (exit {code})", format_duration(elapsed))
}

/// Whether per-task chain timing is shown. Timing is diagnostic meta-output,
/// so it follows the same mute switches as the dispatch arrow and warnings:
/// `--quiet` / `RUNNER_QUIET` and `--no-warnings` / `RUNNER_NO_WARNINGS` each
/// suppress it.
pub(crate) fn timing_enabled(overrides: &ResolutionOverrides) -> bool {
    !overrides.silences_runner() && !overrides.silences_warnings()
}

/// Print a per-task timing line to stderr for the sequential and live
/// (streaming) parallel paths, e.g. `· build finished in 1.2s (exit 0)`.
/// Mirrors the dimmed `·` meta-line style used by the `--explain` trace and
/// is suppressed by [`timing_enabled`]. Grouped parallel output instead folds
/// the summary into each task's block footer (see the chain executor).
pub(crate) fn emit_task_timing(
    overrides: &ResolutionOverrides,
    name: &str,
    elapsed: std::time::Duration,
    code: i32,
) {
    if !timing_enabled(overrides) {
        return;
    }
    eprintln!(
        "{} {} {}",
        "·".dimmed(),
        name.bold(),
        task_timing_summary(elapsed, code).dimmed(),
    );
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    use super::{
        GroupSuppression, configure_command, group_emission, node_bin_dirs, prepended_path,
    };
    use crate::resolver::ResolutionOverrides;
    use crate::tool::test_support::TempDir;

    #[test]
    fn group_emission_gates_on_nesting() {
        // Exercise the pure core directly (no live GITHUB_ACTIONS needed): the
        // nested flag is the load-bearing gate. Holding grouping on + under
        // Actions, flipping parent_group_open flips the decision, proving the
        // nesting suppression actually fires (a tautological test that only
        // checked the env-false path would pass even without the gate).
        assert!(
            group_emission(true, true, GroupSuppression::None),
            "grouping on + under Actions + not nested → emit"
        );
        assert!(
            !group_emission(true, true, GroupSuppression::ParentGroupOpen),
            "...but a parent's open group suppresses it (GHA groups don't nest)"
        );
        // The other factors still gate independently.
        assert!(
            !group_emission(false, true, GroupSuppression::None),
            "grouping opted out"
        );
        assert!(
            !group_emission(true, false, GroupSuppression::None),
            "not under Actions"
        );
        assert!(
            !group_emission(true, true, GroupSuppression::Quiet),
            "--quiet keeps ::group:: off a stdout the caller is parsing",
        );
    }

    #[test]
    fn quiet_outranks_a_parents_open_group() {
        use super::suppression;

        // Both suppress, but they are not interchangeable: quiet means
        // "write nothing", while a parent's group means "your output is
        // already inside one". Reporting quiet first keeps the reason
        // honest when a nested runner is also quiet.
        assert_eq!(suppression(false, false), GroupSuppression::None);
        assert_eq!(suppression(true, false), GroupSuppression::ParentGroupOpen);
        assert_eq!(suppression(false, true), GroupSuppression::Quiet);
        assert_eq!(suppression(true, true), GroupSuppression::Quiet);
    }

    #[test]
    fn admit_frame_accepts_distinct_tasks() {
        use super::{admit_frame, task_frame};
        use crate::types::TaskSource;

        let root = PathBuf::from("/repo");
        let stack = admit_frame(Vec::new(), task_frame(&root, TaskSource::PackageJson, "a"))
            .expect("first frame");
        let stack = admit_frame(stack, task_frame(&root, TaskSource::PackageJson, "b"))
            .expect("a different task is not a cycle");

        assert_eq!(stack.len(), 2);
    }

    #[test]
    fn admit_frame_reports_the_whole_cycle() {
        use super::{admit_frame, task_frame};
        use crate::types::TaskSource;

        let root = PathBuf::from("/repo");
        let frame = |name: &str| task_frame(&root, TaskSource::PackageJson, name);
        let stack = admit_frame(Vec::new(), frame("a")).expect("first frame");
        let stack = admit_frame(stack, frame("b")).expect("second frame");

        let err = admit_frame(stack, frame("a")).expect_err("a -> b -> a is a cycle");
        let msg = format!("{err:#}");

        assert!(
            msg.contains("package.json:a -> package.json:b -> package.json:a"),
            "the diagnostic must name every task in the loop. msg: {msg}",
        );
    }

    #[test]
    fn admit_frame_separates_the_same_task_in_two_roots() {
        use super::{admit_frame, task_frame};
        use crate::types::TaskSource;

        // A workspace member dispatching its own `build` from the root's
        // `build` is a normal fan-out, not a loop.
        let stack = admit_frame(
            Vec::new(),
            task_frame(&PathBuf::from("/repo"), TaskSource::PackageJson, "build"),
        )
        .expect("root frame");

        assert!(
            admit_frame(
                stack,
                task_frame(
                    &PathBuf::from("/repo/apps/web"),
                    TaskSource::PackageJson,
                    "build",
                ),
            )
            .is_ok(),
            "same task name under a different root must still dispatch",
        );
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

        // `apps/` has no node_modules; levels without an installed
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
        // `node_modules/.bin` has to spawn; this is exactly the
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
        // absolute shim path, with args and env tweaks surviving.
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
    fn format_duration_uses_millis_below_one_second() {
        use std::time::Duration;

        use super::format_duration;

        assert_eq!(format_duration(Duration::from_millis(0)), "0ms");
        assert_eq!(format_duration(Duration::from_millis(5)), "5ms");
        assert_eq!(format_duration(Duration::from_millis(342)), "342ms");
        // 999ms stays in the millisecond band; 1000ms crosses into seconds.
        assert_eq!(format_duration(Duration::from_millis(999)), "999ms");
    }

    #[test]
    fn format_duration_uses_seconds_with_one_decimal_under_a_minute() {
        use std::time::Duration;

        use super::format_duration;

        assert_eq!(format_duration(Duration::from_secs(1)), "1.0s");
        assert_eq!(format_duration(Duration::from_millis(1234)), "1.2s");
        assert_eq!(format_duration(Duration::from_millis(4200)), "4.2s");
        assert_eq!(format_duration(Duration::from_millis(59_400)), "59.4s");
    }

    #[test]
    fn format_duration_uses_minutes_and_padded_seconds_at_a_minute() {
        use std::time::Duration;

        use super::format_duration;

        // 60s is the boundary into the minute band; sub-minute seconds are
        // zero-padded to two digits so columns stay aligned.
        assert_eq!(format_duration(Duration::from_mins(1)), "1m 00s");
        assert_eq!(format_duration(Duration::from_secs(64)), "1m 04s");
        assert_eq!(format_duration(Duration::from_secs(125)), "2m 05s");
        assert_eq!(format_duration(Duration::from_secs(3661)), "61m 01s");
    }

    #[test]
    fn format_duration_rounds_seconds_to_nearest_inside_minute_band() {
        use std::time::Duration;

        use super::format_duration;

        // Non-integer seconds inside the minute band must round half-up to the
        // nearest whole second, not floor. Flooring would under-report by up to
        // ~0.95s near band edges (the bug these inputs guard against).
        assert_eq!(format_duration(Duration::from_millis(60_900)), "1m 01s");
        assert_eq!(format_duration(Duration::from_millis(90_700)), "1m 31s");
        assert_eq!(format_duration(Duration::from_millis(119_940)), "2m 00s");
        // Well below the half-second boundary (0.449s) rounds down.
        assert_eq!(format_duration(Duration::from_millis(90_449)), "1m 30s");
        // Just inside [0.45s, 0.50s): rounding must stay half-up (boundary at
        // 0.50, not 0.45). A cascaded double-rounding would bump these up a
        // whole second (0.45 -> 0.5 tenth -> 1 second), so guard the window.
        assert_eq!(format_duration(Duration::from_millis(60_450)), "1m 00s");
        assert_eq!(format_duration(Duration::from_millis(60_499)), "1m 00s");
        assert_eq!(format_duration(Duration::from_millis(90_450)), "1m 30s");
        // The double-round bug visibly flipped the minute here (1m 59s -> 2m 00s).
        assert_eq!(format_duration(Duration::from_millis(119_450)), "1m 59s");
        // The true half-second boundary rounds up.
        assert_eq!(format_duration(Duration::from_millis(60_500)), "1m 01s");
    }

    #[test]
    fn format_duration_rounds_into_minute_band_near_sixty_seconds() {
        use std::time::Duration;

        use super::format_duration;

        // Durations in [59.95s, 60.0s) have as_secs() == 59 but round up to
        // 60.0s. The band must be chosen on the rounded value, so these promote
        // into the minute band instead of printing a contract-violating "60.0s".
        for millis in [59_950, 59_990, 59_999] {
            let rendered = format_duration(Duration::from_millis(millis));
            assert_eq!(rendered, "1m 00s", "{millis}ms should round into minutes");
            assert_ne!(rendered, "60.0s", "{millis}ms must never print as 60.0s");
        }
        // The tenth just below the rounding boundary stays in the seconds band.
        assert_eq!(format_duration(Duration::from_millis(59_940)), "59.9s");
    }

    #[test]
    fn task_timing_summary_includes_duration_and_exit_code() {
        use std::time::Duration;

        use super::task_timing_summary;

        assert_eq!(
            task_timing_summary(Duration::from_millis(1500), 0),
            "finished in 1.5s (exit 0)"
        );
        assert_eq!(
            task_timing_summary(Duration::from_millis(200), 7),
            "finished in 200ms (exit 7)"
        );
    }

    #[test]
    fn timing_enabled_respects_quiet_and_no_warnings() {
        use super::timing_enabled;
        use crate::resolver::ResolutionOverrides;

        assert!(timing_enabled(&ResolutionOverrides::default()));
        assert!(!timing_enabled(&ResolutionOverrides {
            quiet_level: crate::tool::QuietLevel::Quiet,
            ..ResolutionOverrides::default()
        }));
        assert!(!timing_enabled(&ResolutionOverrides {
            no_warnings: true,
            ..ResolutionOverrides::default()
        }));
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
