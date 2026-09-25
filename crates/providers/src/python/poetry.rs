//! Poetry.

use runner_core::{
    BinDirs, BinsCap, Capabilities, Declared, Discovery, Ecosystem, Field, Frozen, Hooks,
    InstallCap, Kind, Provider, ProviderId, QuietSupport, RunTaskCap, ScriptSupport, Signal,
    TestCap, t,
};

fn tool_table(field: &Field<'_>) -> Option<Declared> {
    field.value.is_object().then_some(Declared::Named)
}

fn build_backend(field: &Field<'_>) -> Option<Declared> {
    field
        .value
        .as_str()
        .is_some_and(|backend| backend.contains("poetry.core.masonry.api"))
        .then_some(Declared::Named)
}

/// Poetry.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Poetry,
    label: "poetry",
    aliases: &[],
    ecosystem: Ecosystem::Python,
    kind: Kind::PACKAGE_MANAGER,
    program: Some("poetry"),
    signals: &[
        Signal::Lockfile("poetry.lock"),
        Signal::ManifestField {
            files: &["pyproject.toml"],
            path: "tool.poetry",
            parse: tool_table,
        },
        Signal::ManifestField {
            files: &["pyproject.toml"],
            path: "build-system.build-backend",
            parse: build_backend,
        },
        Signal::Probe("poetry"),
    ],
    writes: &[".venv"],
    caps: Capabilities {
        install: Some(InstallCap {
            argv: t!["install"],
            frozen: Frozen::Unsupported,
            scripts: ScriptSupport::NONE,
            locked_only_with: &[],
        }),
        run_task: Some(RunTaskCap {
            argv: t![Quiet, "run", Task, Args],
            sources: &[ProviderId::Pyproject],
        }),
        test: Some(TestCap {
            program: None,
            argv: t!["run", Args],
            discovery: Discovery::Detect(super::test_runner),
            file_flags: None,
        }),
        bins: Some(BinsCap {
            dirs: BinDirs::Ask(super::venv::bin_dirs),
        }),
        clean: Some(super::CLEAN),
        quiet: QuietSupport::flag(t!["--quiet"]),
        ..Capabilities::NONE
    },
    tasks: None,
    version: None,
    hooks: Hooks::NONE,
};

#[cfg(test)]
mod tests {
    use runner_core::{Declared, Field};
    use serde_json::{Value, json};

    static LONE: Value = Value::Null;

    fn tool_table(value: &Value) -> Option<Declared> {
        super::tool_table(&Field {
            value,
            manifest: &LONE,
        })
    }

    fn build_backend(value: &Value) -> Option<Declared> {
        super::build_backend(&Field {
            value,
            manifest: &LONE,
        })
    }

    #[test]
    fn poetry_is_named_by_its_table_or_its_backend() {
        assert_eq!(
            tool_table(&json!({ "name": "demo" })),
            Some(Declared::Named)
        );
        assert_eq!(tool_table(&json!("demo")), None);
        assert_eq!(
            build_backend(&json!("poetry.core.masonry.api")),
            Some(Declared::Named)
        );
        assert_eq!(build_backend(&json!("hatchling.build")), None);
    }
}
