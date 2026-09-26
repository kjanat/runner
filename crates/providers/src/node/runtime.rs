//! Node.js as a runtime.

use runner_core::{
    Capabilities, Declared, Ecosystem, ExecCap, Hooks, Kind, NameShape, Provider, ProviderId,
    QuietSupport, Reach, RunFileCap, RunTaskCap, Signal, t,
};

use super::manifest::engines_node;

/// Node.js.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Node,
    label: "node",
    aliases: &[],
    ecosystem: Ecosystem::Node,
    kind: Kind::RUNTIME,
    program: Some("node"),
    signals: &[
        Signal::FileContent {
            name: ".nvmrc",
            parse: version_file,
        },
        Signal::FileContent {
            name: ".node-version",
            parse: version_file,
        },
        Signal::FileContent {
            name: ".tool-versions",
            parse: tool_versions,
        },
        Signal::ManifestField {
            files: super::MANIFESTS,
            path: "engines.node",
            parse: engines_node,
        },
        Signal::Probe("node"),
    ],
    caps: Capabilities {
        package_exec: Some(ExecCap {
            program: Some("npx"),
            argv: t!["--package", Package, "--", Name, Args],
            reach: Reach::Network,
            accepts: NameShape::BARE,
        }),
        file_fallback: true,
        file_interpreters: &["node", "nodejs", "bun", "deno"],
        run_task: Some(RunTaskCap {
            argv: t!["--run", Task, Sep("--"), Args],
            sources: &[ProviderId::PackageJson],
        }),
        exec: Some(ExecCap {
            program: Some("npx"),
            argv: t![Name, Args],
            reach: Reach::Network,
            accepts: NameShape::BARE.union(NameShape::VERSIONED),
        }),
        run_file: Some(RunFileCap {
            unsupported: &[
                ("jsx", "Node has no JSX transform"),
                ("tsx", "Node has no TSX transform"),
            ],
            program: None,
            extensions: &["js", "mjs", "cjs", "ts", "mts", "cts"],
            argv: t![File, Args],
        }),
        test: Some(super::TEST),
        quiet: QuietSupport::unsupported("node --run has no host diagnostic switch"),
        ..Capabilities::NONE
    },
    tasks: None,
    version: Some(crate::version::read),
    hooks: Hooks {
        before_plan: Some(before_plan),
        ..Hooks::NONE
    },
};

/// `node --run` runs neither `pre<task>` nor `post<task>`, which every package
/// manager runs; name the ones the manifest declares.
fn warn_skipped_lifecycle(
    tree: &runner_core::Tree,
    task: &runner_core::Task,
    warnings: &mut Vec<runner_core::Warning>,
) {
    let dir = runner_core::scope_dir(tree, &task.scope);
    let manifest = match runner_core::read_manifest(&dir, super::MANIFESTS) {
        Ok(Some((_, manifest))) => manifest,
        Ok(None) => return,
        Err(error) => {
            warnings.push(runner_core::Warning::about(
                ProviderId::Node,
                error.to_string(),
            ));
            return;
        }
    };
    let skipped: Vec<String> = [format!("pre{}", task.name), format!("post{}", task.name)]
        .into_iter()
        .filter(|name| manifest["scripts"].get(name).is_some())
        .collect();
    if !skipped.is_empty() {
        warnings.push(runner_core::Warning::about(
            ProviderId::Node,
            format!(
                "`node --run {}` does not run {} (package managers do)",
                task.name,
                skipped.join(" or "),
            ),
        ));
    }
}

fn version_file(text: &str) -> Option<Declared> {
    let version = text.trim();
    let version = version.strip_prefix('v').unwrap_or(version);
    (!version.is_empty()).then(|| Declared::Version(version.to_owned()))
}

fn tool_versions(text: &str) -> Option<Declared> {
    text.lines().find_map(|line| {
        let mut words = line.split('#').next()?.split_whitespace();
        (words.next()? == "nodejs").then_some(())?;
        words
            .next()
            .map(|version| Declared::Version(version.to_owned()))
    })
}

fn before_plan(
    tree: &runner_core::Tree,
    present: &runner_core::Present,
    op: &runner_core::Op<'_>,
    _: &runner_core::Policy,
    warnings: &mut Vec<runner_core::Warning>,
) -> Result<(), runner_core::Refusal> {
    if let runner_core::Op::Run { task, .. } = op
        && task.source == ProviderId::PackageJson
    {
        warn_skipped_lifecycle(tree, task, warnings);
    }
    if matches!(op, runner_core::Op::Run { .. })
        && let Some(version) = present.version.as_deref()
        && let Some(major) = version
            .trim_start_matches('v')
            .split('.')
            .next()
            .and_then(|s| s.parse::<u32>().ok())
        && major < 22
    {
        return Err(runner_core::Refusal::Invalid(format!(
            "node task execution needs Node 22 or newer, but the node on PATH is {version}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use runner_core::Declared;

    use super::{tool_versions, version_file};

    #[test]
    fn a_version_file_declares_its_trimmed_version() {
        assert_eq!(
            version_file("v20.11.0\n"),
            Some(Declared::Version("20.11.0".into()))
        );
        assert_eq!(version_file("  \n"), None);
    }

    #[test]
    fn tool_versions_declares_the_nodejs_line() {
        assert_eq!(
            tool_versions("python 3.12\nnodejs 20.11.1 # pinned for ci\n"),
            Some(Declared::Version("20.11.1".into()))
        );
        assert_eq!(tool_versions("nodejs20.11.1\n"), None);
        assert_eq!(tool_versions("python 3.12\n"), None);
    }
}
