//! Yarn Classic and Berry, distinguished by observed project evidence.

use runner_core::{
    Capabilities, Declared, Ecosystem, ExecCap, Frozen, Hooks, InstallCap, Kind, NameShape,
    Provider, ProviderId, QuietSupport, Reach, RunTaskCap, ScriptMechanism, ScriptSupport, Signal,
    Weight, WorkspaceCap, t,
};
use serde_json::Value;

use super::manifest;

fn package_manager(value: &Value) -> Option<Declared> {
    manifest::package_manager(value, ProviderId::Yarn)
}

fn dev_engines(value: &Value) -> Option<Declared> {
    manifest::dev_engines(value, ProviderId::Yarn)
}

const MANIFEST: [Signal; 2] = super::manifest_signals(package_manager, dev_engines);

/// Yarn.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Yarn,
    label: "yarn",
    aliases: &[],
    ecosystem: Ecosystem::Node,
    kind: Kind::PACKAGE_MANAGER,
    program: Some("yarn"),
    signals: &[
        Signal::Lockfile("yarn.lock"),
        MANIFEST[0],
        MANIFEST[1],
        Signal::File(".yarnrc.yml"),
        Signal::File(".yarnrc"),
        Signal::Probe("yarn"),
    ],
    writes: super::WRITES,
    caps: Capabilities {
        probe_priority: 3,
        variants: &[("classic", CLASSIC), ("berry", BERRY)],
        variant_of_version: Some(line_of_version),
        install: Some(InstallCap {
            argv: t!["install", Frozen, Scripts],
            frozen: Frozen::Flag("--frozen-lockfile"),
            scripts: ScriptSupport {
                deny: ScriptMechanism::FlagAndEnv(
                    "--ignore-scripts",
                    "YARN_ENABLE_SCRIPTS",
                    "false",
                ),
                allow: ScriptMechanism::Default,
            },
            locked_only_with: &[],
        }),
        ..CLASSIC
    },
    tasks: None,
    version: Some(crate::version::read),
    hooks: Hooks {
        before_plan: Some(manifest::before_plan),
        after_observe: Some(after_observe),
    },
};

const CLASSIC: Capabilities = Capabilities {
    install: Some(InstallCap {
        argv: t!["install", Frozen, Scripts],
        frozen: Frozen::Flag("--frozen-lockfile"),
        scripts: ScriptSupport {
            deny: ScriptMechanism::Flag("--ignore-scripts"),
            allow: ScriptMechanism::Default,
        },
        locked_only_with: &[],
    }),
    run_task: Some(RunTaskCap {
        argv: t![Quiet, Task, Args],
        sources: &[ProviderId::PackageJson],
    }),
    exec: Some(ExecCap {
        program: None,
        argv: t!["run", Name, Args],
        reach: Reach::Local,
        accepts: NameShape::BARE,
    }),
    test: Some(super::TEST),
    bins: Some(super::BINS),
    workspaces: Some(WorkspaceCap {
        members: super::workspace::members,
    }),
    clean: Some(super::CLEAN),
    quiet: QuietSupport::flag(t!["--silent"]),
    ..Capabilities::NONE
};

const BERRY: Capabilities = Capabilities {
    package_exec: Some(ExecCap {
        program: None,
        argv: t!["dlx", "--package", Package, Name, Args],
        reach: Reach::Network,
        accepts: NameShape::BARE,
    }),
    exec: Some(ExecCap {
        program: None,
        argv: t!["exec", Name, Args],
        reach: Reach::Local,
        accepts: NameShape::BARE,
    }),
    install: Some(InstallCap {
        argv: t!["install", Frozen, Scripts],
        frozen: Frozen::Flag("--immutable"),
        scripts: ScriptSupport {
            deny: ScriptMechanism::Env("YARN_ENABLE_SCRIPTS", "false"),
            allow: ScriptMechanism::Env("YARN_ENABLE_SCRIPTS", "true"),
        },
        locked_only_with: &[],
    }),
    run_task: Some(RunTaskCap {
        argv: t!["run", Task, Args],
        sources: &[ProviderId::PackageJson],
    }),
    quiet: QuietSupport::unsupported("--silent is Yarn Classic-only"),
    variants: &[],
    test: Some(super::TEST),
    bins: Some(super::BINS),
    workspaces: Some(WorkspaceCap {
        members: super::workspace::members,
    }),
    clean: Some(super::CLEAN),
    ..Capabilities::NONE
};

/// The Yarn line each scope's own evidence establishes, as one more piece
/// of evidence with the weight and location of the file that said so.
///
/// Only evidence that speaks for Yarn counts; a manifest naming another
/// manager establishes nothing about Yarn.
fn after_observe(
    tree: &runner_core::Tree,
    evidence: &[runner_core::Evidence],
) -> std::io::Result<Vec<runner_core::Evidence>> {
    let mut derived = Vec::new();
    let mut scopes = Vec::new();
    for item in evidence.iter().filter(|e| {
        e.provider == Some(ProviderId::Yarn)
            && !matches!(e.declared, Some(Declared::Alternative(_)))
    }) {
        if scopes.contains(&item.scope) {
            continue;
        }
        scopes.push(item.scope.clone());
        let dir = runner_core::scope_dir(tree, &item.scope);
        let Some(hint) = variant_of(&dir, evidence, item)? else {
            continue;
        };
        derived.push(runner_core::Evidence {
            provider: Some(ProviderId::Yarn),
            signal: signal_for(&hint.at),
            at: hint.at,
            scope: item.scope.clone(),
            weight: hint.weight,
            declared: Some(Declared::Variant(hint.line.into())),
        });
    }
    Ok(derived)
}

/// One file's word on the Yarn line and how strongly it speaks.
struct Hint {
    at: std::path::PathBuf,
    weight: Weight,
    line: &'static str,
}

/// The Yarn line `dir` uses, strongest evidence first: a manifest
/// declaration, then `yarn.lock`'s header, then `.yarnrc.yml`, which only
/// Berry writes.
///
/// Every source is read before any is trusted, so a broken file is an error
/// even when a stronger source answers.
fn variant_of(
    dir: &std::path::Path,
    evidence: &[runner_core::Evidence],
    item: &runner_core::Evidence,
) -> std::io::Result<Option<Hint>> {
    let manifest = dir.join("package.json");
    let from_manifest = match read_optional(&manifest)? {
        Some(text) => serde_json::from_str::<Value>(&text)
            .map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("{}: {error}", manifest.display()),
                )
            })?
            .get("packageManager")
            .and_then(package_manager)
            .as_ref()
            .and_then(line),
        None => None,
    };
    let config = dir.join(".yarnrc.yml");
    let configured = match std::fs::metadata(&config) {
        Ok(metadata) => metadata.is_file(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(std::io::Error::new(
                error.kind(),
                format!("{}: {error}", config.display()),
            ));
        }
    };
    let lock = dir.join("yarn.lock");
    let from_lock = read_optional(&lock)?.and_then(|text| {
        if text.lines().any(|line| line == "__metadata:") {
            Some("berry")
        } else {
            text.lines()
                .any(|line| line == "# yarn lockfile v1")
                .then_some("classic")
        }
    });
    let declared = evidence
        .iter()
        .filter(|e| {
            e.provider == Some(ProviderId::Yarn)
                && e.scope == item.scope
                && !matches!(e.declared, Some(Declared::Alternative(_)))
        })
        .find_map(|e| {
            let line = e.declared.as_ref().and_then(line)?;
            Some(Hint {
                at: e.at.clone(),
                weight: Weight::Declared,
                line,
            })
        });
    Ok(declared
        .or_else(|| {
            from_manifest.map(|line| Hint {
                at: manifest,
                weight: Weight::Declared,
                line,
            })
        })
        .or_else(|| {
            from_lock.map(|line| Hint {
                at: lock,
                weight: Weight::Locked,
                line,
            })
        })
        .or_else(|| {
            configured.then_some(Hint {
                at: config,
                weight: Weight::Configured,
                line: "berry",
            })
        }))
}

/// The index of the Yarn signal that names `at`, so reports can say which
/// field or file spoke.
fn signal_for(at: &std::path::Path) -> Option<runner_core::SignalId> {
    let name = at.file_name()?.to_str()?;
    PROVIDER
        .signals
        .iter()
        .position(|signal| signal.file_names().contains(&name))
        .map(runner_core::SignalId)
}

/// `classic` for a Yarn 1 version, `berry` for 2 and later.
fn line(declared: &Declared) -> Option<&'static str> {
    line_of_version(declared.version()?)
}

/// The line a version or a lower-bounded range names. `>=4`, `^4.1.0`,
/// `~1.22` and `4.x` all answer; a range with an upper bound alone or
/// several alternatives does not.
fn line_of_version(spec: &str) -> Option<&'static str> {
    let spec = spec.trim();
    if spec.contains("||") || spec.contains(" - ") || spec.starts_with('<') {
        return None;
    }
    let digits = spec
        .trim_start_matches(['^', '~', '>', '=', 'v', ' '])
        .split(['.', ' '])
        .next()?;
    let major = digits.parse::<u32>().ok()?;
    Some(if major >= 2 { "berry" } else { "classic" })
}

fn read_optional(path: &std::path::Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(std::io::Error::new(
            error.kind(),
            format!("{}: {error}", path.display()),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::line_of_version;

    #[test]
    fn a_version_or_a_lower_bounded_range_names_the_line() {
        for (spec, expected) in [
            ("1.22.0", Some("classic")),
            ("4.1.0", Some("berry")),
            (">=4", Some("berry")),
            ("^1.22", Some("classic")),
            ("~4.0.0", Some("berry")),
            ("4.x", Some("berry")),
            ("v2.4.3", Some("berry")),
            ("<2", None),
            ("1 || 4", None),
            ("1.0.0 - 2.0.0", None),
            ("latest", None),
        ] {
            assert_eq!(line_of_version(spec), expected, "{spec}");
        }
    }
}
