//! Shared Node.js helpers used by all Node package managers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::Deserialize;
use yaml_rust2::YamlLoader;

use crate::tool::files;
use crate::tool::program;
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

/// Returns `true` if `dir` sits inside a JS monorepo — i.e. some ancestor
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
/// - `(Some(pm), None)` — field present and parses to a script-dispatching PM.
/// - `(None, Some(raw))` — field present but unparseable; caller emits a
///   `DetectionWarning::UnparseablePackageManager { raw }`.
/// - `(None, None)` — field absent / empty / whitespace.
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

impl OnFail {
    /// Canonical lowercase label used in `--json` output. Stable across
    /// `Debug` changes — consumers can branch on the exact string.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Ignore => "ignore",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
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
    pub version: Option<String>,
    /// Effective `onFail` policy. For `packageManager`, always `Ignore`
    /// (the legacy field has no failure mode). For `devEngines`, taken
    /// from the entry, defaulting per the `OpenJS` proposal.
    pub on_fail: OnFail,
}

/// Detect a manifest-level PM declaration: legacy `packageManager` first,
/// falling back to `devEngines.packageManager`. Returns `None` if neither
/// field is present or parseable.
///
/// Entries naming a PM that cannot dispatch `package.json` scripts
/// (e.g. `cargo`) are dropped at parse time so a non-script PM never
/// wins as a manifest declaration and fails later at spawn time.
pub(crate) fn detect_pm_from_manifest(dir: &Path) -> Option<ManifestPmDecl> {
    let parsed = parse_package_json(dir)?;

    // `packageManager` is authoritative when present (the Corepack
    // contract). A non-empty value short-circuits here even when
    // unparseable: letting `devEngines` win would substitute a PM the
    // user never wrote, so we return `None` and drop to the lockfile /
    // PATH-probe steps instead. Whitespace-only counts as "not set".
    let pm_spec = parsed
        .package_manager
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(spec) = pm_spec {
        return parse_package_manager_spec(Some(spec)).map(|(pm, version)| ManifestPmDecl {
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

    // Filter to script-dispatching PMs *first*, then enumerate the
    // resolvable subsequence so onFail defaults track the picked entry's
    // position among resolvable peers, not its raw array index. Without
    // this, a trailing `{"name": "cargo"}` would downgrade the previous
    // entry's default from `error` to `ignore` even though it would have
    // been the user-visible winner.
    let resolvable: Vec<(PackageManager, DevEngineDep)> = entries
        .into_iter()
        .filter_map(|entry| script_dispatching_pm(&entry.name).map(|pm| (pm, entry)))
        .collect();
    let total = resolvable.len();
    if total == 0 {
        return None;
    }

    let mut last_decl: Option<ManifestPmDecl> = None;
    for (idx, (pm, entry)) in resolvable.into_iter().enumerate() {
        let on_fail = entry.on_fail.map_or_else(
            || default_on_fail_for_array_position(idx, total),
            OnFail::from_proposal,
        );
        last_decl = Some(ManifestPmDecl {
            pm,
            source: ManifestSource::DevEngines,
            version: entry.version,
            on_fail,
        });
    }
    last_decl
}

/// Parse a `devEngines.packageManager` entry's `name` field, accepting
/// only PMs that can run `package.json` scripts. Non-script ecosystems
/// (Cargo, uv, …) are rejected at parse time rather than failing later
/// on the dispatch path.
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

/// Outcome of comparing a declared semver constraint against an installed
/// PM's reported version. Returned by [`check_version_constraint`] so
/// callers can fold the mismatch into a warning or a fatal error.
#[derive(Debug, Clone)]
pub(crate) enum VersionCheck {
    /// The declared range matched the installed version.
    Satisfied,
    /// The declared range did not match. Carries human-readable detail
    /// for the resolver to surface verbatim.
    Mismatch {
        /// What was declared (e.g. `^9.0.0`, `>=20`).
        declared: String,
        /// What `<pm> --version` returned, normalized.
        actual: String,
    },
    /// The check could not run — typically because the PM binary is
    /// missing from `$PATH` or the declared range is unparseable. Treated
    /// as "no constraint to enforce" by callers, mirroring proposal
    /// guidance that unparseable ranges should not block dispatch.
    Unverifiable {
        /// One-line reason for the skip, suitable for diagnostics.
        #[allow(
            dead_code,
            reason = "consumed by --explain / runner doctor traces in Phase 6+"
        )]
        reason: String,
    },
}

/// Compare a declared semver constraint to the installed version of `pm`.
///
/// Spawns `<pm> --version`, parses the output, and runs
/// [`semver::VersionReq::matches`]. Errors during any of those steps
/// collapse to [`VersionCheck::Unverifiable`] so a partially-broken
/// environment never blocks dispatch unnecessarily — `onFail = error` is
/// expected to handle the missing-binary case via the PATH probe; this
/// helper is the *version* gate.
pub(crate) fn check_version_constraint(pm: PackageManager, declared: &str) -> VersionCheck {
    let req = match semver::VersionReq::parse(declared) {
        Ok(req) => req,
        Err(err) => {
            return VersionCheck::Unverifiable {
                reason: format!("invalid semver range {declared:?}: {err}"),
            };
        }
    };

    let Some(raw_version) = installed_version(pm) else {
        return VersionCheck::Unverifiable {
            reason: format!(
                "`{} --version` did not produce a parseable version",
                pm.label()
            ),
        };
    };

    let actual = match semver::Version::parse(&normalize_version(&raw_version)) {
        Ok(v) => v,
        Err(err) => {
            return VersionCheck::Unverifiable {
                reason: format!("could not parse `{raw_version}` as semver: {err}"),
            };
        }
    };

    if req.matches(&actual) {
        VersionCheck::Satisfied
    } else {
        VersionCheck::Mismatch {
            declared: declared.to_string(),
            actual: actual.to_string(),
        }
    }
}

/// Run `<pm> --version` and return its trimmed stdout, stripping a `v`
/// prefix. Returns `None` when the spawn fails or the process exits
/// non-zero.
///
/// Spawns via `tool::program::command` so Windows `npm.cmd`/`pnpm.cmd`/
/// `yarn.cmd` shims resolve through `PATHEXT`; otherwise
/// `devEngines.version` enforcement silently degrades to `Unverifiable`.
fn installed_version(pm: PackageManager) -> Option<String> {
    let out = program::command(pm.label())
        .arg("--version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let trimmed = raw.trim();
    let first_line = trimmed.lines().next().unwrap_or(trimmed).trim();
    parse_version_token(first_line)
}

/// Pull the first valid semver token out of a `<pm> --version` first
/// line. Tolerates Deno's verbose output
/// (`deno 2.7.12 (stable, release, x86_64-unknown-linux-gnu)`) as well
/// as the bare-version forms emitted by npm/pnpm/yarn/bun. A leading
/// `v` prefix is stripped per Corepack's display convention.
///
/// Extracted from [`installed_version`] so the parsing logic stays
/// testable without spawning a subprocess.
fn parse_version_token(line: &str) -> Option<String> {
    line.split_whitespace().find_map(|token| {
        let cleaned = token.strip_prefix('v').unwrap_or(token);
        semver::Version::parse(&normalize_version(cleaned))
            .ok()
            .map(|_| cleaned.to_string())
    })
}

/// Pad bare `major` or `major.minor` versions to a full semver triple so
/// `semver::Version::parse` accepts them. Anything richer (build/pre
/// suffixes) is returned untouched.
fn normalize_version(raw: &str) -> String {
    let segments: Vec<&str> = raw.split('.').collect();
    match segments.len() {
        1 => format!("{}.0.0", segments[0]),
        2 => format!("{}.{}.0", segments[0], segments[1]),
        _ => raw.to_string(),
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
    fn detect_pm_from_manifest_blocks_dev_engines_when_package_manager_unparseable() {
        // An unparseable `packageManager` must not elevate `devEngines`:
        // the legacy field is authoritative, so a parse failure returns
        // `None` rather than substituting the devEngines PM.
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
        // Whitespace-only / empty packageManager is the JSON equivalent
        // of "not set"; devEngines should still win in that case.
        use super::{ManifestSource, detect_pm_from_manifest};

        let dir = TempDir::new("node-manifest-decl-empty-pm-field");
        fs::write(
            dir.path().join("package.json"),
            r#"{
              "packageManager": "   ",
              "devEngines": { "packageManager": { "name": "yarn" } }
            }"#,
        )
        .expect("package.json should be written");

        let decl = detect_pm_from_manifest(dir.path()).expect("devEngines should still resolve");
        assert_eq!(decl.pm, PackageManager::Yarn);
        assert_eq!(decl.source, ManifestSource::DevEngines);
    }

    #[test]
    fn parse_package_manager_spec_rejects_trailing_at_sign() {
        // `"pnpm@"` is a typo (`pnpm@9` minus the version) — treating
        // it as "pnpm without a version constraint" silently hides
        // user intent. The parser must reject so the detection-layer
        // warning surfaces the verbatim value.
        use super::parse_package_manager_spec;

        assert!(parse_package_manager_spec(Some("pnpm@")).is_none());
        assert!(parse_package_manager_spec(Some("npm@")).is_none());
        // Whitespace inside the version still counts as empty after trim
        // — split_once('@') doesn't trim, but the leading raw is trimmed
        // before splitting, so `"  pnpm@  "` becomes `"pnpm@"` then
        // splits to ("pnpm", " ") — non-empty whitespace remains as the
        // version literal, intentionally NOT a typo signal.
        assert!(parse_package_manager_spec(Some(" pnpm@ ".trim())).is_none());
    }

    #[test]
    fn parse_package_manager_spec_accepts_bare_name() {
        // Bare `"pnpm"` (no `@` at all) is distinct from `"pnpm@"`:
        // the former is an explicit "any version", the latter is a
        // truncated `name@version` spec. Keep the former working.
        use super::parse_package_manager_spec;

        let (pm, version) =
            parse_package_manager_spec(Some("pnpm")).expect("bare name still parses");
        assert_eq!(pm, PackageManager::Pnpm);
        assert!(version.is_none());
    }

    #[test]
    fn detect_pm_from_manifest_surfaces_trailing_at_as_unparseable() {
        // End-to-end: `"packageManager": "pnpm@"` reaches the resolver
        // as a None decl, dropping to lockfile/probe (and the
        // detection layer fires `UnparseablePackageManager` so the
        // user sees the typo).
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
    fn detect_pm_from_manifest_rejects_non_script_dispatching_pm() {
        // `cargo` is a valid PackageManager variant but it can't dispatch
        // package.json scripts, so devEngines should drop it at parse
        // time rather than letting it bubble up and fail at spawn.
        use super::detect_pm_from_manifest;

        let dir = TempDir::new("node-manifest-decl-cargo");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "devEngines": { "packageManager": { "name": "cargo" } } }"#,
        )
        .expect("package.json should be written");

        assert!(detect_pm_from_manifest(dir.path()).is_none());
    }

    #[test]
    fn detect_pm_from_manifest_array_trailing_unresolvable_does_not_downgrade_on_fail() {
        // Earlier resolvable entry plus a trailing unresolvable name —
        // the resolvable one must inherit the "last entry" default
        // (Error) because it *is* the last resolvable entry.
        use super::{OnFail, detect_pm_from_manifest};

        let dir = TempDir::new("node-manifest-decl-array-trailing-unresolvable");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "devEngines": { "packageManager": [
                { "name": "pnpm", "version": "9" },
                { "name": "zoot-unknown" }
            ] } }"#,
        )
        .expect("package.json should be written");

        let decl = detect_pm_from_manifest(dir.path()).expect("decl should be present");
        assert_eq!(decl.pm, PackageManager::Pnpm);
        assert_eq!(decl.on_fail, OnFail::Error);
    }

    #[test]
    fn check_version_constraint_satisfied_for_matching_range() {
        // Use the host's `cargo --version` since cargo ships with the
        // toolchain; rust-toolchain.toml pins to 1.95 which means
        // cargo is >=1.95. Any range that 1.95+ satisfies works.
        use super::{VersionCheck, check_version_constraint};

        // cargo's ecosystem is Rust, but check_version_constraint only
        // spawns `<label> --version` and parses semver, so it works for
        // any binary on PATH regardless of ecosystem categorization.
        let res = check_version_constraint(PackageManager::Cargo, ">=1.0.0");
        match res {
            VersionCheck::Satisfied => {}
            VersionCheck::Mismatch { declared, actual } => {
                panic!("expected satisfaction, got mismatch: {declared} vs {actual}");
            }
            VersionCheck::Unverifiable { reason } => {
                // `cargo` may not be on PATH in some CI environments —
                // accept the skip rather than fail the suite.
                eprintln!("skipping: {reason}");
            }
        }
    }

    #[test]
    fn parse_version_token_handles_bare_semver() {
        use super::parse_version_token;

        assert_eq!(parse_version_token("10.9.2"), Some("10.9.2".to_string()));
        assert_eq!(parse_version_token("9.0.0"), Some("9.0.0".to_string()));
        assert_eq!(parse_version_token("1.22.22"), Some("1.22.22".to_string()));
    }

    #[test]
    fn parse_version_token_strips_v_prefix() {
        use super::parse_version_token;

        assert_eq!(parse_version_token("v20.11.0"), Some("20.11.0".to_string()));
    }

    #[test]
    fn parse_version_token_finds_version_in_deno_verbose_output() {
        use super::parse_version_token;

        // Deno's `--version` first line on a stable release:
        //   `deno 2.7.12 (stable, release, x86_64-unknown-linux-gnu)`
        // The token-scan should skip "deno" and pick up "2.7.12".
        let line = "deno 2.7.12 (stable, release, x86_64-unknown-linux-gnu)";
        assert_eq!(parse_version_token(line), Some("2.7.12".to_string()));
    }

    #[test]
    fn parse_version_token_handles_deno_short_flag_output() {
        use super::parse_version_token;

        // `deno -v` skips the trailing parens but still has the "deno"
        // prefix token: `deno 2.7.12`. Same parse, no special-casing.
        assert_eq!(
            parse_version_token("deno 2.7.12"),
            Some("2.7.12".to_string())
        );
    }

    #[test]
    fn parse_version_token_handles_bare_major_minor() {
        use super::parse_version_token;

        // normalize_version pads bare `9` / `9.5` to a parseable triple
        // so the token-scan accepts them. Common in CI logs where the
        // PM's --version surface might collapse to a short string.
        assert_eq!(parse_version_token("9"), Some("9".to_string()));
        assert_eq!(parse_version_token("9.5"), Some("9.5".to_string()));
    }

    #[test]
    fn parse_version_token_returns_none_for_garbage() {
        use super::parse_version_token;

        assert_eq!(parse_version_token(""), None);
        assert_eq!(parse_version_token("not a version"), None);
        assert_eq!(parse_version_token("---"), None);
    }

    #[test]
    fn check_version_constraint_returns_unverifiable_for_invalid_range() {
        use super::{VersionCheck, check_version_constraint};

        let res = check_version_constraint(PackageManager::Cargo, "not-a-range");
        assert!(matches!(res, VersionCheck::Unverifiable { .. }));
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
