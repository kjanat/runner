//! Shared Node.js helpers used by all Node package managers.

use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Command;

use anyhow::Context as _;
use serde::Deserialize;
use yaml_rust2::YamlLoader;

use crate::tool::files;
#[cfg(test)]
use crate::tool::program;
use crate::types::PackageManager;

/// Node manifest filename.
pub(crate) const PACKAGE_JSON_FILENAME: &str = "package.json";

/// Supported Node manifest filenames, in resolution order.
pub(crate) const MANIFEST_FILENAMES: &[&str] =
    &[PACKAGE_JSON_FILENAME, "package.json5", "package.yaml"];

/// Returns `true` if `dir` contains a supported package manifest.
pub(crate) fn has_package_json(dir: &Path) -> bool {
    find_manifest(dir).is_some()
}

/// `node --run <task> [-- args...]` (Node 22+), Node's own `package.json`
/// script runner.
///
/// The `--` is mandatory: without it node parses a leading `--flag` as one of
/// its own CLI options and exits with `bad option`. Node forwards everything
/// after it to the script and does **not** interpret it, so `-- --watch` is the
/// script's argument, never node's watch mode.
///
/// `node --run` writes nothing of its own, so both verbosity axes no-op.
#[cfg(test)]
pub(crate) fn run_cmd(task: &str, args: &[String], _verbosity: super::HostVerbosity) -> Command {
    let mut c = program::command("node");
    c.arg("--run").arg(task);
    if !args.is_empty() {
        c.arg("--").args(args);
    }
    c
}

/// Resolve the first supported package manifest path.
pub(crate) fn find_manifest(dir: &Path) -> Option<PathBuf> {
    files::find_first(dir, MANIFEST_FILENAMES).filter(|path| path.is_file())
}

/// Resolve the nearest supported package manifest path while walking upward.
pub(crate) fn find_manifest_upwards(dir: &Path) -> Option<PathBuf> {
    files::find_first_upwards(dir, MANIFEST_FILENAMES).filter(|path| path.is_file())
}

/// Returns `true` if `dir` sits inside a JS monorepo, i.e. some ancestor
/// (within the VCS root) declares a workspace via `pnpm-workspace.yaml`,
/// `lerna.json`, or a `package.json` carrying a `"workspaces"` key.
///
/// Guards upward script discovery: a manifest-less subdirectory adopts a
/// parent's scripts only when it provably belongs to that workspace.
pub(crate) fn within_workspace_upwards(dir: &Path) -> bool {
    files::find_in_ancestors(dir, |ancestor| {
        if ancestor.join("pnpm-workspace.yaml").is_file() || ancestor.join("lerna.json").is_file() {
            return Some(());
        }
        // npm / yarn / bun declare workspaces inside package.json itself.
        let has_workspaces = std::fs::read_to_string(ancestor.join(PACKAGE_JSON_FILENAME))
            .ok()
            .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
            .is_some_and(|json| json.get("workspaces").is_some());
        has_workspaces.then_some(())
    })
    .is_some()
}

/// Detect the package manager named by the `"packageManager"` field in the
/// supported package manifest.
pub(crate) fn detect_pm_from_field(dir: &Path) -> Option<PackageManager> {
    detect_pm(parse_package_json(dir))
}

/// Detect the `packageManager` field AND surface a diagnostic when the
/// field is present but unparseable (typo, unsupported PM, malformed
/// spec). The returned `raw` value is the verbatim string the user
/// wrote in `package.json`, suitable for echoing back in the warning.
///
/// Returns:
/// - `(Some(pm), None)`, field present and parses to a script-dispatching PM.
/// - `(None, Some(raw))`, field present but unparseable; caller emits a
///   `DetectionWarning::UnparseablePackageManager { raw }`.
/// - `(None, None)`, field absent / empty / whitespace.
pub(crate) fn detect_pm_field_with_diagnostics(
    dir: &Path,
) -> (Option<PackageManager>, Option<String>) {
    let Some(parsed) = parse_package_json(dir) else {
        return (None, None);
    };
    let Some(raw) = parsed
        .package_manager
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return (None, None);
    };
    match parse_package_manager_spec(Some(raw)) {
        Some((pm, _)) => (Some(pm), None),
        None => (None, Some(raw.to_string())),
    }
}

fn detect_pm(package_json: Option<PackageJson>) -> Option<PackageManager> {
    parse_package_manager_spec(
        package_json
            .and_then(|package_json| package_json.package_manager)
            .as_deref(),
    )
    .map(|(pm, _)| pm)
}

/// Parse a Corepack-style `name@version` spec into a [`PackageManager`] and
/// optional version string. Bare `"pnpm"` parses with `None` version;
/// the malformed `name@` form (empty version) is rejected so the
/// [`UnparseablePackageManager`] warning surfaces the typo rather than
/// silently dropping the constraint. Unknown names return `None`.
fn parse_package_manager_spec(spec: Option<&str>) -> Option<(PackageManager, Option<String>)> {
    let raw = spec?.trim();
    let (name, version) = match raw.split_once('@') {
        Some((_, "")) => return None,
        Some((n, v)) => (n, Some(v.to_string())),
        None => (raw, None),
    };
    let pm = match name {
        "npm" => PackageManager::Npm,
        "pnpm" => PackageManager::Pnpm,
        "yarn" => PackageManager::Yarn,
        "bun" => PackageManager::Bun,
        "deno" => PackageManager::Deno,
        _ => return None,
    };
    Some((pm, version))
}

/// The package manager the manifest declares: the legacy `packageManager`
/// field when present, else the last known `devEngines.packageManager` entry.
///
/// A non-empty `packageManager` that does not parse yields `None`: the
/// legacy field is authoritative, so `devEngines` never substitutes for it.
pub(crate) fn detect_pm_from_manifest(dir: &Path) -> Option<PackageManager> {
    let parsed = parse_package_json(dir)?;
    let pm_spec = parsed
        .package_manager
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(spec) = pm_spec {
        return parse_package_manager_spec(Some(spec)).map(|(pm, _)| pm);
    }
    let entries = match parsed.dev_engines?.package_manager? {
        DevEnginesPmField::One(dep) => vec![dep],
        DevEnginesPmField::Many(deps) => deps,
    };
    entries
        .into_iter()
        .filter_map(|entry| script_dispatching_pm(&entry.name))
        .next_back()
}

/// Parse a `devEngines.packageManager` entry's `name` field, accepting
/// only PMs that can run `package.json` scripts.
fn script_dispatching_pm(label: &str) -> Option<PackageManager> {
    let pm = PackageManager::from_label(label)?;
    matches!(
        pm,
        PackageManager::Npm
            | PackageManager::Pnpm
            | PackageManager::Yarn
            | PackageManager::Bun
            | PackageManager::Deno
    )
    .then_some(pm)
}

#[derive(Deserialize)]
struct PackageJson {
    #[serde(default)]
    name: Option<String>,
    #[serde(rename = "packageManager")]
    package_manager: Option<String>,
    #[serde(
        rename = "devEngines",
        default,
        deserialize_with = "lenient_dev_engines"
    )]
    dev_engines: Option<DevEngines>,
    #[serde(default, deserialize_with = "lenient_workspaces")]
    workspaces: Option<Vec<String>>,
}

/// `"workspaces"` as the npm/bun array form or the yarn v1 object form
/// (`{ "packages": [...] }`). Unrecognised shapes degrade to `None`.
fn lenient_workspaces<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Workspaces {
        Globs(Vec<String>),
        Config { packages: Vec<String> },
    }
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value::<Option<Workspaces>>(value)
        .ok()
        .flatten()
        .map(|field| match field {
            Workspaces::Globs(globs) | Workspaces::Config { packages: globs } => globs,
        }))
}

/// The `"workspaces"` globs declared by the manifest in `dir`, if any.
pub(crate) fn workspace_globs(dir: &Path) -> Option<Vec<String>> {
    parse_package_json(dir)?.workspaces
}

/// The `"name"` declared by the manifest in `dir`, if any.
pub(crate) fn manifest_name(dir: &Path) -> Option<String> {
    parse_package_json(dir)?.name
}

/// Deserialize `devEngines` without letting a malformed value poison the
/// whole manifest. The field rides in the same struct as `scripts`, so a
/// strict parse of e.g. `"devEngines": "pnpm@9"` (spec requires an
/// object) used to abort the entire deserialize, dropping every script
/// and the `packageManager` signal, and mislabeling valid JSON as "not
/// valid JSON". A shape we don't recognize degrades to `None` (same
/// outcome as an absent key) instead.
fn lenient_dev_engines<'de, D>(deserializer: D) -> Result<Option<DevEngines>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value::<Option<DevEngines>>(value).unwrap_or_default())
}

#[derive(Deserialize)]
struct DevEngines {
    #[serde(rename = "packageManager", default)]
    package_manager: Option<DevEnginesPmField>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum DevEnginesPmField {
    Many(Vec<DevEngineDep>),
    One(DevEngineDep),
}

#[derive(Deserialize)]
struct DevEngineDep {
    name: String,
}

fn parse_package_json(dir: &Path) -> Option<PackageJson> {
    let (path, content) = read_manifest(dir).ok()??;
    parse_manifest(&path, &content)
}

fn read_manifest(dir: &Path) -> anyhow::Result<Option<(PathBuf, String)>> {
    let Some(path) = find_manifest(dir) else {
        return Ok(None);
    };

    read_manifest_file(&path)
}

fn read_manifest_file(path: &Path) -> anyhow::Result<Option<(PathBuf, String)>> {
    std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))
        .map(|content| Some((path.to_path_buf(), content)))
}

fn parse_manifest(path: &Path, content: &str) -> Option<PackageJson> {
    if path
        .file_name()
        .is_some_and(|name| name == std::ffi::OsStr::new("package.json5"))
    {
        json5::from_str(content).ok()
    } else if path
        .file_name()
        .is_some_and(|name| name == std::ffi::OsStr::new("package.yaml"))
    {
        parse_package_yaml(content)
    } else {
        serde_json::from_str(content).ok()
    }
}

fn parse_package_yaml(content: &str) -> Option<PackageJson> {
    let docs = YamlLoader::load_from_str(content).ok()?;
    let doc = docs.first()?;
    let root = doc.as_hash()?;

    let package_manager = root
        .iter()
        .find_map(|(key, value)| (key.as_str() == Some("packageManager")).then_some(value))
        .and_then(yaml_rust2::Yaml::as_str)
        .map(ToOwned::to_owned);

    let name = root
        .iter()
        .find_map(|(key, value)| (key.as_str() == Some("name")).then_some(value))
        .and_then(yaml_rust2::Yaml::as_str)
        .map(ToOwned::to_owned);

    let workspaces = root
        .iter()
        .find_map(|(key, value)| (key.as_str() == Some("workspaces")).then_some(value))
        .and_then(|value| {
            let globs = value.as_vec().or_else(|| {
                value
                    .as_hash()?
                    .iter()
                    .find_map(|(key, packages)| {
                        (key.as_str() == Some("packages")).then_some(packages)
                    })?
                    .as_vec()
            })?;
            Some(
                globs
                    .iter()
                    .filter_map(yaml_rust2::Yaml::as_str)
                    .map(ToOwned::to_owned)
                    .collect(),
            )
        });

    Some(PackageJson {
        name,
        package_manager,
        dev_engines: None,
        workspaces,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{detect_pm_from_field, find_manifest_upwards};
    use crate::tool::test_support::TempDir;
    use crate::types::PackageManager;

    #[test]
    fn detect_pm_from_field_supports_package_json5() {
        let dir = TempDir::new("node-package-json5-pm");
        fs::write(
            dir.path().join("package.json5"),
            "{ packageManager: 'pnpm@9.0.0' }",
        )
        .expect("package.json5 should be written");

        assert_eq!(detect_pm_from_field(dir.path()), Some(PackageManager::Pnpm));
    }

    #[test]
    fn malformed_dev_engines_does_not_poison_the_package_manager() {
        let dir = TempDir::new("node-malformed-devengines");
        fs::write(
            dir.path().join("package.json"),
            r#"{
                "packageManager": "pnpm@9.0.0",
                "devEngines": { "packageManager": "pnpm@9.0.0" },
                "scripts": { "build": "vite build" }
            }"#,
        )
        .expect("package.json should be written");

        assert_eq!(detect_pm_from_field(dir.path()), Some(PackageManager::Pnpm));
    }

    #[test]
    fn detect_pm_from_field_supports_package_yaml() {
        let dir = TempDir::new("node-package-yaml-pm");
        fs::write(
            dir.path().join("package.yaml"),
            "packageManager: yarn@4.3.0\n",
        )
        .expect("package.yaml should be written");

        assert_eq!(detect_pm_from_field(dir.path()), Some(PackageManager::Yarn));
    }

    #[test]
    fn detect_pm_from_field_supports_deno_package_manager() {
        let dir = TempDir::new("node-package-json-deno-pm");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "packageManager": "deno@2.7.12" }"#,
        )
        .expect("package.json should be written");

        assert_eq!(detect_pm_from_field(dir.path()), Some(PackageManager::Deno));
    }

    #[test]
    fn find_manifest_upwards_prefers_nearest_manifest() {
        let dir = TempDir::new("node-manifest-upwards");
        let nested = dir.path().join("apps").join("site").join("src");
        fs::create_dir_all(&nested).expect("nested dir should be created");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "scripts": { "root": "1" } }"#,
        )
        .expect("root package.json should be written");
        fs::write(
            dir.path().join("apps").join("site").join("package.json"),
            r#"{ "scripts": { "member": "1" } }"#,
        )
        .expect("member package.json should be written");

        let path = find_manifest_upwards(&nested).expect("nearest manifest should resolve");

        assert!(path.ends_with("apps/site/package.json"));
    }

    #[test]
    fn detect_pm_from_manifest_prefers_package_manager_field() {
        use super::detect_pm_from_manifest;

        let dir = TempDir::new("node-manifest-decl-package-manager");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "packageManager": "yarn@4.3.0",
                 "devEngines": { "packageManager": { "name": "pnpm", "version": "9", "onFail": "error" } } }"#,
        )
        .expect("package.json should be written");

        assert_eq!(
            detect_pm_from_manifest(dir.path()),
            Some(PackageManager::Yarn)
        );
    }

    #[test]
    fn detect_pm_from_manifest_uses_dev_engines_when_package_manager_absent() {
        use super::detect_pm_from_manifest;

        let dir = TempDir::new("node-manifest-decl-dev-engines");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "devEngines": { "packageManager": { "name": "pnpm", "version": "9.0.0", "onFail": "error" } } }"#,
        )
        .expect("package.json should be written");

        assert_eq!(
            detect_pm_from_manifest(dir.path()),
            Some(PackageManager::Pnpm)
        );
    }

    #[test]
    fn detect_pm_from_manifest_blocks_dev_engines_when_package_manager_unparseable() {
        use super::detect_pm_from_manifest;

        let dir = TempDir::new("node-manifest-decl-unparseable-pm-field");
        fs::write(
            dir.path().join("package.json"),
            r#"{
              "packageManager": "pnpmm@9",
              "devEngines": { "packageManager": { "name": "yarn" } }
            }"#,
        )
        .expect("package.json should be written");

        assert!(
            detect_pm_from_manifest(dir.path()).is_none(),
            "unparseable packageManager must NOT silently elevate devEngines",
        );
    }

    #[test]
    fn detect_pm_from_manifest_treats_empty_package_manager_as_unset() {
        use super::detect_pm_from_manifest;

        let dir = TempDir::new("node-manifest-decl-empty-pm-field");
        fs::write(
            dir.path().join("package.json"),
            r#"{
              "packageManager": "   ",
              "devEngines": { "packageManager": { "name": "yarn" } }
            }"#,
        )
        .expect("package.json should be written");

        assert_eq!(
            detect_pm_from_manifest(dir.path()),
            Some(PackageManager::Yarn)
        );
    }

    #[test]
    fn parse_package_manager_spec_rejects_trailing_at_sign() {
        use super::parse_package_manager_spec;

        assert!(parse_package_manager_spec(Some("pnpm@")).is_none());
        assert!(parse_package_manager_spec(Some("npm@")).is_none());
        assert!(parse_package_manager_spec(Some(" pnpm@ ".trim())).is_none());
    }

    #[test]
    fn parse_package_manager_spec_accepts_bare_name() {
        use super::parse_package_manager_spec;

        let (pm, version) =
            parse_package_manager_spec(Some("pnpm")).expect("bare name still parses");
        assert_eq!(pm, PackageManager::Pnpm);
        assert!(version.is_none());
    }

    #[test]
    fn detect_pm_from_manifest_surfaces_trailing_at_as_unparseable() {
        use super::detect_pm_from_manifest;

        let dir = TempDir::new("node-manifest-decl-trailing-at");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "packageManager": "pnpm@" }"#,
        )
        .expect("package.json should be written");

        assert!(detect_pm_from_manifest(dir.path()).is_none());
    }

    #[test]
    fn detect_pm_from_manifest_uses_the_last_known_array_entry() {
        use super::detect_pm_from_manifest;

        let dir = TempDir::new("node-manifest-decl-array");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "devEngines": { "packageManager": [
                { "name": "yarn", "version": "1" },
                { "name": "pnpm", "version": "9" },
                { "name": "zoot-unknown" }
            ] } }"#,
        )
        .expect("package.json should be written");

        assert_eq!(
            detect_pm_from_manifest(dir.path()),
            Some(PackageManager::Pnpm)
        );
    }

    #[test]
    fn detect_pm_from_manifest_returns_none_for_unknown_or_non_script_names() {
        use super::detect_pm_from_manifest;

        for (name, body) in [
            (
                "unknown",
                r#"{ "devEngines": { "packageManager": { "name": "zoot" } } }"#,
            ),
            (
                "cargo",
                r#"{ "devEngines": { "packageManager": { "name": "cargo" } } }"#,
            ),
        ] {
            let dir = TempDir::new(&format!("node-manifest-decl-{name}"));
            fs::write(dir.path().join("package.json"), body)
                .expect("package.json should be written");
            assert!(detect_pm_from_manifest(dir.path()).is_none(), "{name}");
        }
    }
}

#[cfg(test)]
mod run_cmd_tests {
    use super::run_cmd;
    use crate::tool::{HostDiagnostics, HostVerbosity, Stream};

    fn argv(cmd: &std::process::Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn run_cmd_separates_user_args_with_a_double_dash() {
        // `node --run build --watch` exits with `node: bad option: --watch`.
        let args = [String::from("--watch"), String::from("src")];
        let cmd = run_cmd("build", &args, HostVerbosity::default());

        assert_eq!(cmd.get_program().to_string_lossy(), "node");
        assert_eq!(argv(&cmd), ["--run", "build", "--", "--watch", "src"]);
    }

    #[test]
    fn run_cmd_without_args_emits_no_trailing_dash_dash() {
        assert_eq!(
            argv(&run_cmd("build", &[], HostVerbosity::default())),
            ["--run", "build"]
        );
    }

    #[test]
    fn run_cmd_verbosity_axes_no_op() {
        let v = HostVerbosity {
            diagnostics: HostDiagnostics::Reduced,
            stream: Stream::Stderr,
        };
        assert_eq!(argv(&run_cmd("build", &[], v)), ["--run", "build"]);
    }
}
