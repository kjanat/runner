//! Source-label dispatcher.
//!
//! Two surfaces disagree on one label: `doctor`/`why`'s structured
//! reports name a cargo alias task's mechanism `"cargo-alias"` (the
//! `provider` field already carries `"cargo"`), while `list`/`info`'s
//! flat shape uses plain tool names throughout. [`flat_source_label`]
//! and [`structured_source_label`] are the two call points; everything
//! else defers to [`TaskSource::label`].

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Serialize, Serializer};

use crate::types::{Task, TaskSource};

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

/// Tool family that executes tasks from this source. Distinct from the
/// structured `kind` label, which names the extraction mechanism.
pub(crate) const fn provider_label(source: TaskSource) -> &'static str {
    match source {
        TaskSource::PackageJson => "node",
        TaskSource::DenoJson => "deno",
        TaskSource::TurboJson => "turbo",
        TaskSource::Makefile => "make",
        TaskSource::Justfile => "just",
        TaskSource::Taskfile => "task",
        TaskSource::CargoAliases => "cargo",
        TaskSource::GoPackage => "go",
        TaskSource::BaconToml => "bacon",
        TaskSource::MiseToml => "mise",
        TaskSource::PyprojectScripts => "python",
    }
}

fn label_schema(label: fn(TaskSource) -> &'static str) -> Schema {
    let labels: Vec<&str> = TaskSource::all()
        .iter()
        .map(|&source| label(source))
        .collect();
    json_schema!({ "type": "string", "enum": labels })
}

/// A task source serialized as its [`flat_source_label`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FlatSource(pub(crate) TaskSource);

impl Serialize for FlatSource {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(flat_source_label(self.0))
    }
}

impl JsonSchema for FlatSource {
    fn schema_name() -> Cow<'static, str> {
        "TaskSourceLabel".into()
    }

    fn schema_id() -> Cow<'static, str> {
        "runner::FlatSource".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        label_schema(flat_source_label)
    }
}

/// A task source serialized as its [`structured_source_label`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StructuredSource(pub(crate) TaskSource);

impl Serialize for StructuredSource {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(structured_source_label(self.0))
    }
}

impl JsonSchema for StructuredSource {
    fn schema_name() -> Cow<'static, str> {
        "TaskSourceLabel".into()
    }

    fn schema_id() -> Cow<'static, str> {
        "runner::StructuredSource".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        label_schema(structured_source_label)
    }
}

/// A task source serialized as its [`provider_label`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Provider(pub(crate) TaskSource);

impl Serialize for Provider {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(provider_label(self.0))
    }
}

impl JsonSchema for Provider {
    fn schema_name() -> Cow<'static, str> {
        "ProviderLabel".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        label_schema(provider_label)
    }
}

/// Build a task's fully-qualified name: `<scope>:<kind>#<name>`, where
/// `scope` is `root` or the workspace member's name.
///
/// The `#` boundary separates the colon-joined structured prefix
/// (`scope:kind`, both colon-free) from the verbatim task name, which may
/// itself contain `:` (e.g. an npm script `fmt:update`). Consumers split
/// once on `#`: everything after is the name, unescaped. Centralised here
/// so `why` and `doctor` can't drift apart on the format.
pub(crate) fn fqn(task: &Task) -> String {
    fqn_of(task.scope(), task.source, &task.name)
}

/// [`fqn`] for a `(scope, source, name)` triple that has no [`Task`] yet,
/// e.g. when matching `[tasks."root:just#fmt"]` config keys.
pub(crate) fn fqn_of(scope: &str, source: TaskSource, name: &str) -> String {
    format!(
        "{scope}:{kind}#{name}",
        kind = structured_source_label(source)
    )
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

/// Display argv from the completed execution plan.
pub(crate) fn planned_command(plan: &runner_core::Plan) -> String {
    plan.argv
        .iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}
