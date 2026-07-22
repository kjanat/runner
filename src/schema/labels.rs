//! Source-label dispatcher.
//!
//! Two surfaces disagree on one label: `doctor`/`why`'s structured
//! reports name a cargo alias task's mechanism `"cargo-alias"` (the
//! `provider` field already carries `"cargo"`), while `list`/`info`'s
//! flat shape uses plain tool names throughout. [`flat_source_label`]
//! and [`structured_source_label`] are the two call points; everything
//! else defers to [`TaskSource::label`].

use std::path::{Path, PathBuf};

use crate::types::{JsRuntime, Task, TaskSource};

/// Source label for the flat `list`/`info` shape ([`super::project`]).
pub(crate) const fn flat_source_label(source: TaskSource) -> &'static str {
    source.label()
}

/// Source label for the structured `doctor`/`why` reports. Only
/// [`TaskSource::CargoAliases`] diverges from [`flat_source_label`], see
/// module docs.
pub(crate) const fn structured_source_label(source: TaskSource) -> &'static str {
    match source {
        TaskSource::CargoAliases => "cargo-alias",
        _ => flat_source_label(source),
    }
}

/// Build a task's fully-qualified name: `<scope>:<kind>#<name>`.
///
/// The `#` boundary separates the colon-joined structured prefix
/// (`scope:kind`, both colon-free) from the verbatim task name, which may
/// itself contain `:` (e.g. an npm script `fmt:update`). Consumers split
/// once on `#`: everything after is the name, unescaped. Centralised here
/// so `why` and `doctor` can't drift apart on the format.
pub(crate) fn fqn(source: TaskSource, name: &str) -> String {
    format!("root:{kind}#{name}", kind = structured_source_label(source))
}

/// Key path (structured configs) or target name (flat files) locating the
/// task inside its source file. Shared by `why` and `doctor` v3 so the two
/// surfaces can't drift apart on the format.
pub(crate) fn source_pointer(task: &Task) -> Option<String> {
    let name = &task.name;
    match task.source {
        TaskSource::CargoAliases => Some(format!("alias.{name}")),
        TaskSource::PackageJson => Some(format!("scripts.{name}")),
        TaskSource::DenoJson
        | TaskSource::TurboJson
        | TaskSource::Taskfile
        | TaskSource::MiseToml => Some(format!("tasks.{name}")),
        TaskSource::BaconToml => Some(format!("jobs.{name}")),
        TaskSource::PyprojectScripts => Some(format!("project.scripts.{name}")),
        TaskSource::Makefile | TaskSource::Justfile => Some(name.clone()),
        TaskSource::GoPackage => None,
    }
}

/// Config file anchoring a task source (file paths, not parent dirs).
/// Shared by `why` and `doctor` v3.
pub(crate) fn source_anchor(source: TaskSource, root: &Path) -> Option<PathBuf> {
    use crate::tool;

    match source {
        TaskSource::PackageJson => tool::node::find_manifest_upwards(root),
        TaskSource::DenoJson => tool::deno::find_config_upwards(root),
        TaskSource::TurboJson => tool::turbo::find_config(root),
        TaskSource::Makefile => tool::files::find_first(root, tool::make::FILENAMES),
        TaskSource::Justfile => tool::just::find_file(root),
        TaskSource::Taskfile => tool::files::find_first(root, tool::go_task::FILENAMES),
        TaskSource::CargoAliases => tool::cargo_aliases::find_anchor(root),
        TaskSource::GoPackage => tool::go_pm::find_file(root),
        TaskSource::BaconToml => tool::files::find_first(root, tool::bacon::FILENAMES),
        TaskSource::MiseToml => tool::mise::find_file(root),
        TaskSource::PyprojectScripts => tool::python::find_pyproject_upwards(root),
    }
}

/// Effective command preview shared by `why` and `doctor` v3. Sources with
/// a fixed executing binary resolve deterministically; `package.json` and
/// `pyproject.toml` scripts depend on package-manager resolution, which the
/// two callers perform differently (why: only for the selected candidate;
/// doctor: project-wide). `node_pm`/`python_pm` take that result already
/// resolved to a label. A forced `runtime` that dispatches the task reads
/// its script through its own runner, so its preview outranks the resolved
/// package manager, matching `cmd::run`'s dispatch.
pub(crate) fn resolved_command(
    task: &Task,
    runtime: Option<JsRuntime>,
    node_pm: Option<&str>,
    python_pm: Option<&str>,
) -> Option<String> {
    let name = &task.name;
    if let Some(rt) = runtime
        && let Some(preview) = crate::cmd::run::runtime_script_preview(rt, task.source, name)
    {
        return Some(preview);
    }
    match task.source {
        TaskSource::CargoAliases => Some(task.alias_of.as_deref().map_or_else(
            || format!("cargo {name}"),
            |expansion| format!("cargo {expansion}"),
        )),
        TaskSource::DenoJson => Some(format!("deno task {name}")),
        TaskSource::TurboJson => Some(format!("turbo run {name}")),
        TaskSource::Makefile => Some(format!("make {name}")),
        TaskSource::Justfile => Some(format!("just {name}")),
        TaskSource::Taskfile => Some(format!("task {name}")),
        TaskSource::BaconToml => Some(format!("bacon {name}")),
        TaskSource::MiseToml => Some(format!("mise run {name}")),
        TaskSource::GoPackage => Some(format!(
            "go run {target}",
            target = task.run_target.as_deref().unwrap_or(name)
        )),
        TaskSource::PackageJson => node_pm.map(|pm| format!("{pm} run {name}")),
        TaskSource::PyprojectScripts => python_pm.map(|pm| format!("{pm} run {name}")),
    }
}
