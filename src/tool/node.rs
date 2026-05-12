//! Shared Node.js helpers used by all Node package managers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::Deserialize;
use yaml_rust2::YamlLoader;

use crate::tool::files;
use crate::types::PackageManager;

/// Node manifest filename.
pub(crate) const PACKAGE_JSON_FILENAME: &str = "package.json";

/// Supported Node manifest filenames, in resolution order.
pub(crate) const MANIFEST_FILENAMES: &[&str] =
    &[PACKAGE_JSON_FILENAME, "package.json5", "package.yaml"];

/// Directories commonly produced by Node.js toolchains.
pub(crate) const DEFAULT_CLEAN_DIRS: &[&str] = &["node_modules", ".cache", "dist"];

/// Framework-specific Node build directories removed only on explicit opt-in.
pub(crate) const FRAMEWORK_CLEAN_DIRS: &[&str] = &[".next", ".parcel-cache", ".svelte-kit"];

/// Returns `true` if `dir` contains a supported package manifest.
pub(crate) fn has_package_json(dir: &Path) -> bool {
    find_manifest(dir).is_some()
}

/// Resolve the first supported package manifest path.
pub(crate) fn find_manifest(dir: &Path) -> Option<PathBuf> {
    files::find_first(dir, MANIFEST_FILENAMES).filter(|path| path.is_file())
}

/// Resolve the nearest supported package manifest path while walking upward.
pub(crate) fn find_manifest_upwards(dir: &Path) -> Option<PathBuf> {
    files::find_first_upwards(dir, MANIFEST_FILENAMES).filter(|path| path.is_file())
}

/// Detect the package manager named by the `"packageManager"` field in the
/// supported package manifest.
pub(crate) fn detect_pm_from_field(dir: &Path) -> Option<PackageManager> {
    detect_pm(parse_package_json(dir))
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
/// optional version string. Returns `None` for unknown names.
fn parse_package_manager_spec(spec: Option<&str>) -> Option<(PackageManager, Option<String>)> {
    let raw = spec?.trim();
    let (name, version) = match raw.split_once('@') {
        Some((n, v)) => (n, (!v.is_empty()).then(|| v.to_string())),
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

/// What the user wants for `onFail` when a `devEngines.packageManager` entry
/// cannot be satisfied.
///
/// Per the `OpenJS` proposal: `ignore | warn | error | download`. `download`
/// is folded into `Warn` here because runner is not an installer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnFail {
    /// Silently use the declared PM regardless of presence/version.
    Ignore,
    /// Use the declared PM but emit a warning if it doesn't satisfy
    /// presence/version constraints.
    Warn,
    /// Refuse to dispatch if the declared PM is missing or version-bad.
    Error,
}

/// Which manifest field provided a [`ManifestPmDecl`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManifestSource {
    /// Legacy `"packageManager"` field (Corepack-compatible spec).
    PackageManager,
    /// New `"devEngines": { "packageManager": ... }` field
    /// (the `OpenJS` proposal, npm 10.9+).
    DevEngines,
}

/// Package-manager declaration extracted from `package.json`.
///
/// Returned by [`detect_pm_from_manifest`] — combines either the legacy
/// `packageManager` field or the new `devEngines.packageManager` field
/// (when the legacy field is absent), tagged with provenance and the
/// proposal-default `onFail` policy.
#[derive(Debug, Clone)]
pub(crate) struct ManifestPmDecl {
    /// The package manager named in the manifest.
    pub pm: PackageManager,
    /// Which manifest field produced this declaration.
    pub source: ManifestSource,
    /// Optional semver range carried alongside the declaration. Captured
    /// for diagnostics in Phase 6; not enforced today.
    #[allow(
        dead_code,
        reason = "consumed by --explain and version checks in Phase 5+"
    )]
    pub version: Option<String>,
    /// Effective `onFail` policy. For `packageManager`, always `Ignore`
    /// (the legacy field has no failure mode). For `devEngines`, taken
    /// from the entry, defaulting per the `OpenJS` proposal.
    #[allow(dead_code, reason = "honored once the PATH probe lands in Phase 5")]
    pub on_fail: OnFail,
}

/// Detect a manifest-level PM declaration: legacy `packageManager` first,
/// falling back to `devEngines.packageManager`. Returns `None` if neither
/// field is present or parseable.
pub(crate) fn detect_pm_from_manifest(dir: &Path) -> Option<ManifestPmDecl> {
    let parsed = parse_package_json(dir)?;

    if let Some((pm, version)) = parse_package_manager_spec(parsed.package_manager.as_deref()) {
        return Some(ManifestPmDecl {
            pm,
            source: ManifestSource::PackageManager,
            version,
            on_fail: OnFail::Ignore,
        });
    }

    let dev_engines = parsed.dev_engines?;
    let pm_field = dev_engines.package_manager?;
    let entries: Vec<DevEngineDep> = match pm_field {
        DevEnginesPmField::One(dep) => vec![dep],
        DevEnginesPmField::Many(deps) => deps,
    };
    if entries.is_empty() {
        return None;
    }

    // OpenJS default rules: a single entry defaults to `onFail = error`;
    // for an array the last entry defaults to `error`, prior entries to
    // `ignore`. We pick the last resolvable entry — it is the most
    // recently authored preference and has the strict default.
    let total = entries.len();
    let mut last_resolvable: Option<(usize, ManifestPmDecl)> = None;
    for (idx, entry) in entries.into_iter().enumerate() {
        let Some(pm) = PackageManager::from_label(&entry.name) else {
            continue;
        };
        let on_fail = entry.on_fail.map_or_else(
            || default_on_fail_for_array_position(idx, total),
            OnFail::from_proposal,
        );
        last_resolvable = Some((
            idx,
            ManifestPmDecl {
                pm,
                source: ManifestSource::DevEngines,
                version: entry.version,
                on_fail,
            },
        ));
    }

    last_resolvable.map(|(_, decl)| decl)
}

const fn default_on_fail_for_array_position(idx: usize, total: usize) -> OnFail {
    if idx + 1 == total {
        OnFail::Error
    } else {
        OnFail::Ignore
    }
}

impl OnFail {
    const fn from_proposal(raw: ProposalOnFail) -> Self {
        match raw {
            ProposalOnFail::Ignore => Self::Ignore,
            ProposalOnFail::Warn | ProposalOnFail::Download => Self::Warn,
            ProposalOnFail::Error => Self::Error,
        }
    }
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum ProposalOnFail {
    Ignore,
    Warn,
    Error,
    Download,
}

/// Parse the supported package manifest and return each script as a
/// `(name, command)` pair. The command body is needed downstream to
/// classify passthrough wrappers (e.g. `"build": "turbo run build"`).
pub(crate) fn extract_scripts(dir: &Path) -> anyhow::Result<Vec<(String, String)>> {
    let Some((path, content)) = read_manifest(dir)? else {
        return Ok(vec![]);
    };

    let package_json = parse_manifest(&path, &content)
        .with_context(|| format!("{} is not valid {}", path.display(), manifest_format(&path)))?;

    Ok(package_json
        .scripts
        .map_or_else(Vec::new, |scripts| scripts.into_iter().collect()))
}

/// Parse scripts from the nearest supported package manifest while walking upward.
pub(crate) fn extract_scripts_upwards(dir: &Path) -> anyhow::Result<Vec<(String, String)>> {
    let Some((path, content)) = read_manifest_upwards(dir)? else {
        return Ok(vec![]);
    };

    let package_json = parse_manifest(&path, &content)
        .with_context(|| format!("{} is not valid {}", path.display(), manifest_format(&path)))?;

    Ok(package_json
        .scripts
        .map_or_else(Vec::new, |scripts| scripts.into_iter().collect()))
}

#[derive(Deserialize)]
struct PackageJson {
    #[serde(rename = "packageManager")]
    package_manager: Option<String>,
    #[serde(rename = "devEngines", default)]
    dev_engines: Option<DevEngines>,
    scripts: Option<HashMap<String, String>>,
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
    #[serde(default)]
    version: Option<String>,
    #[serde(rename = "onFail", default)]
    on_fail: Option<ProposalOnFail>,
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

fn read_manifest_upwards(dir: &Path) -> anyhow::Result<Option<(PathBuf, String)>> {
    let Some(path) = find_manifest_upwards(dir) else {
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

    let scripts = root
        .iter()
        .find_map(|(key, value)| (key.as_str() == Some("scripts")).then_some(value))
        .and_then(yaml_rust2::Yaml::as_hash)
        .map(|table| {
            table
                .iter()
                .filter_map(|(name, body)| {
                    let name = name.as_str()?.to_owned();
                    let body = body.as_str().unwrap_or_default().to_owned();
                    Some((name, body))
                })
                .collect::<HashMap<_, _>>()
        })
        .filter(|table| !table.is_empty());

    Some(PackageJson {
        package_manager,
        dev_engines: None,
        scripts,
    })
}

fn manifest_format(path: &Path) -> &'static str {
    if path
        .file_name()
        .is_some_and(|name| name == std::ffi::OsStr::new("package.json5"))
    {
        "JSON5"
    } else if path
        .file_name()
        .is_some_and(|name| name == std::ffi::OsStr::new("package.yaml"))
    {
        "YAML"
    } else {
        "JSON"
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        detect_pm_from_field, extract_scripts, extract_scripts_upwards, find_manifest_upwards,
    };
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
    fn extract_scripts_supports_package_json5() {
        let dir = TempDir::new("node-package-json5-scripts");
        fs::write(
            dir.path().join("package.json5"),
            "{ scripts: { build: 'vite build', test: 'vitest' } }",
        )
        .expect("package.json5 should be written");

        let mut scripts =
            extract_scripts(dir.path()).expect("scripts should parse from package.json5");
        scripts.sort_unstable();

        assert_eq!(
            scripts,
            [
                ("build".to_owned(), "vite build".to_owned()),
                ("test".to_owned(), "vitest".to_owned()),
            ]
        );
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
    fn extract_scripts_supports_package_yaml() {
        let dir = TempDir::new("node-package-yaml-scripts");
        fs::write(
            dir.path().join("package.yaml"),
            "scripts:\n  build: vite build\n  test: vitest\n",
        )
        .expect("package.yaml should be written");

        let mut scripts =
            extract_scripts(dir.path()).expect("scripts should parse from package.yaml");
        scripts.sort_unstable();

        assert_eq!(
            scripts,
            [
                ("build".to_owned(), "vite build".to_owned()),
                ("test".to_owned(), "vitest".to_owned()),
            ]
        );
    }

    #[test]
    fn extract_scripts_supports_inline_yaml_script_map() {
        let dir = TempDir::new("node-package-yaml-inline-scripts");
        fs::write(
            dir.path().join("package.yaml"),
            "scripts: { build: vite build, test: vitest }\n",
        )
        .expect("package.yaml should be written");

        let mut scripts =
            extract_scripts(dir.path()).expect("scripts should parse from inline YAML map");
        scripts.sort_unstable();

        assert_eq!(
            scripts,
            [
                ("build".to_owned(), "vite build".to_owned()),
                ("test".to_owned(), "vitest".to_owned()),
            ]
        );
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
        use super::{ManifestSource, OnFail, detect_pm_from_manifest};

        let dir = TempDir::new("node-manifest-decl-package-manager");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "packageManager": "yarn@4.3.0",
                 "devEngines": { "packageManager": { "name": "pnpm", "version": "9", "onFail": "error" } } }"#,
        )
        .expect("package.json should be written");

        let decl = detect_pm_from_manifest(dir.path()).expect("decl should be present");
        assert_eq!(decl.pm, PackageManager::Yarn);
        assert_eq!(decl.source, ManifestSource::PackageManager);
        assert_eq!(decl.version.as_deref(), Some("4.3.0"));
        assert_eq!(decl.on_fail, OnFail::Ignore);
    }

    #[test]
    fn detect_pm_from_manifest_uses_dev_engines_when_package_manager_absent() {
        use super::{ManifestSource, OnFail, detect_pm_from_manifest};

        let dir = TempDir::new("node-manifest-decl-dev-engines");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "devEngines": { "packageManager": { "name": "pnpm", "version": "9.0.0", "onFail": "error" } } }"#,
        )
        .expect("package.json should be written");

        let decl = detect_pm_from_manifest(dir.path()).expect("decl should be present");
        assert_eq!(decl.pm, PackageManager::Pnpm);
        assert_eq!(decl.source, ManifestSource::DevEngines);
        assert_eq!(decl.version.as_deref(), Some("9.0.0"));
        assert_eq!(decl.on_fail, OnFail::Error);
    }

    #[test]
    fn detect_pm_from_manifest_default_on_fail_for_single_object_is_error() {
        use super::{OnFail, detect_pm_from_manifest};

        let dir = TempDir::new("node-manifest-decl-default-on-fail");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "devEngines": { "packageManager": { "name": "bun" } } }"#,
        )
        .expect("package.json should be written");

        let decl = detect_pm_from_manifest(dir.path()).expect("decl should be present");
        assert_eq!(decl.pm, PackageManager::Bun);
        // Single-object form defaults to onFail=error per proposal — implemented
        // here as "last entry of a 1-element array defaults to error".
        assert_eq!(decl.on_fail, OnFail::Error);
    }

    #[test]
    fn detect_pm_from_manifest_uses_last_array_entry_with_error_default() {
        use super::{OnFail, detect_pm_from_manifest};

        let dir = TempDir::new("node-manifest-decl-array");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "devEngines": { "packageManager": [
                { "name": "yarn", "version": "1" },
                { "name": "pnpm", "version": "9" }
            ] } }"#,
        )
        .expect("package.json should be written");

        let decl = detect_pm_from_manifest(dir.path()).expect("decl should be present");
        assert_eq!(decl.pm, PackageManager::Pnpm);
        assert_eq!(decl.on_fail, OnFail::Error);
    }

    #[test]
    fn detect_pm_from_manifest_honors_explicit_on_fail_warn() {
        use super::{OnFail, detect_pm_from_manifest};

        let dir = TempDir::new("node-manifest-decl-warn");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "devEngines": { "packageManager": { "name": "yarn", "onFail": "warn" } } }"#,
        )
        .expect("package.json should be written");

        let decl = detect_pm_from_manifest(dir.path()).expect("decl should be present");
        assert_eq!(decl.on_fail, OnFail::Warn);
    }

    #[test]
    fn detect_pm_from_manifest_treats_download_as_warn() {
        use super::{OnFail, detect_pm_from_manifest};

        let dir = TempDir::new("node-manifest-decl-download");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "devEngines": { "packageManager": { "name": "yarn", "onFail": "download" } } }"#,
        )
        .expect("package.json should be written");

        let decl = detect_pm_from_manifest(dir.path()).expect("decl should be present");
        // runner is not an installer; download collapses to warn.
        assert_eq!(decl.on_fail, OnFail::Warn);
    }

    #[test]
    fn detect_pm_from_manifest_returns_none_for_unknown_name() {
        use super::detect_pm_from_manifest;

        let dir = TempDir::new("node-manifest-decl-unknown");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "devEngines": { "packageManager": { "name": "zoot" } } }"#,
        )
        .expect("package.json should be written");

        assert!(detect_pm_from_manifest(dir.path()).is_none());
    }

    #[test]
    fn extract_scripts_upwards_reads_nearest_manifest() {
        let dir = TempDir::new("node-scripts-upwards");
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

        let tasks = extract_scripts_upwards(&nested).expect("nearest scripts should parse");

        assert_eq!(tasks, [("member".to_owned(), "1".to_owned())]);
    }
}
