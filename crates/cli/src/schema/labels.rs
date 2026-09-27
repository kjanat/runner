//! Registry labels as JSON strings, and the task identities built from them.

use std::borrow::Cow;

use runner_core::{ProviderId, TaskTable};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Serialize, Serializer};

use crate::provider::Named;
use crate::types::Task;

/// The tool family that executes a task source's tasks: its own program, or
/// its ecosystem when other tools read it.
pub(crate) fn family(source: ProviderId) -> &'static str {
    let provider = source.provider();
    provider
        .program
        .map_or_else(|| source.ecosystem().label(), |_| provider.label)
}

fn enum_schema(labels: impl IntoIterator<Item = &'static str>) -> Schema {
    let mut values: Vec<&str> = Vec::new();
    for label in labels {
        if !values.contains(&label) {
            values.push(label);
        }
    }
    json_schema!({ "type": "string", "enum": values })
}

macro_rules! provider_label {
    ($(#[$doc:meta])* $name:ident, $schema:literal, $members:path, $label:path) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub(crate) struct $name(pub(crate) ProviderId);

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str($label(self.0))
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                $schema.into()
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                enum_schema($members().into_iter().map($label))
            }
        }
    };
}

provider_label!(
    /// A task source, as its label.
    SourceLabel,
    "TaskSourceLabel",
    crate::provider::task_sources,
    Named::label
);
provider_label!(
    /// A package manager, as its label.
    PmLabel,
    "PackageManagerLabel",
    crate::provider::package_managers,
    Named::label
);
provider_label!(
    /// A JavaScript runtime, as its label.
    RuntimeLabel,
    "JsRuntimeLabel",
    crate::provider::js_runtimes,
    Named::label
);
provider_label!(
    /// A task source, as the [`family`] that executes it.
    FamilyLabel,
    "ProviderLabel",
    crate::provider::task_sources,
    family
);

/// Build a task's fully-qualified name: `<scope>:<source>#<name>`, where
/// `scope` is `root` or the workspace member's label.
///
/// The `#` boundary separates the colon-joined prefix from the verbatim task
/// name, which may itself contain `:` (an npm script `fmt:update`).
pub(crate) fn fqn(task: &Task) -> String {
    fqn_of(task.scope(), task.source, &task.name)
}

/// [`fqn`] for a `(scope, source, name)` triple that has no [`Task`] yet,
/// e.g. when matching `[tasks."root:just#fmt"]` config keys.
pub(crate) fn fqn_of(scope: &str, source: ProviderId, name: &str) -> String {
    format!("{scope}:{kind}#{name}", kind = source.label())
}

/// The key its source file keeps tasks under, e.g. `scripts`.
pub(crate) fn task_container_key(source: ProviderId) -> Option<&'static str> {
    match source.provider().caps.task_table {
        TaskTable::Key(key) => Some(key),
        TaskTable::Name | TaskTable::None => None,
    }
}

/// Where the task sits inside its source file: a key path for structured
/// configs (`scripts.test`), the target name for flat files.
pub(crate) fn source_pointer(task: &Task) -> Option<String> {
    match task.source.provider().caps.task_table {
        TaskTable::Key(key) => Some(format!("{key}.{}", task.name)),
        TaskTable::Name => Some(task.name.clone()),
        TaskTable::None => None,
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
