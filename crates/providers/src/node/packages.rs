//! Installed npm packages and the binaries their manifests declare.

use std::path::Path;

use runner_core::{BinRuns, Installed, InstalledBin, ProviderId, Tree, Warning};
use serde::Deserialize;
use serde_json::Value;

/// The package `name` in the nearest `node_modules` at or above `dir`, the
/// lookup order Node itself uses.
///
/// # Errors
/// Returns an unreadable or malformed package manifest.
pub fn node_modules(_: &Tree, dir: &Path, name: &str) -> Result<Option<Installed>, Warning> {
    if !is_package_name(name) {
        return Ok(None);
    }
    let Some(at) = dir
        .ancestors()
        .map(|ancestor| ancestor.join("node_modules").join(name))
        .find(|dir| dir.is_dir())
    else {
        return Ok(None);
    };
    let path = at.join("package.json");
    let text = std::fs::read_to_string(&path).map_err(|error| failed(&path, &error))?;
    let manifest: Value = serde_json::from_str(&text).map_err(|error| failed(&path, &error))?;
    declared(name, &at, &manifest).map(Some)
}

/// A package a Yarn Plug'n'Play install keeps out of `node_modules`, from
/// `yarn bin --json` in the scope directory. Its binaries run through Yarn,
/// which loads the dependency.
///
/// # Errors
/// Returns Yarn's failure to list binaries.
pub fn plug_n_play(tree: &Tree, dir: &Path, name: &str) -> Result<Option<Installed>, Warning> {
    let scope = runner_core::plan::scope_at(tree, dir);
    let scope_dir = runner_core::scope_dir(tree, &scope);
    if !is_pnp(&scope_dir) && !is_pnp(&tree.root) {
        return Ok(None);
    }
    let Some(yarn) = runner_core::probe_with("yarn", &[]) else {
        return Ok(None);
    };
    let output = std::process::Command::new(yarn)
        .args(["bin", "--json"])
        .current_dir(&scope_dir)
        .output()
        .map_err(|error| Warning::about(ProviderId::Yarn, error.to_string()))?;
    if !output.status.success() {
        return Err(Warning::about(
            ProviderId::Yarn,
            format!(
                "yarn bin --json in {} failed ({}): {}",
                scope_dir.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    let bins = accessible_bins(&String::from_utf8_lossy(&output.stdout))?;
    Ok(provided_by(&bins, name, &scope_dir))
}

/// One line of `yarn bin --json`.
#[derive(Debug, Deserialize, PartialEq, Eq)]
struct AccessibleBin {
    name: String,
    /// The providing package, `name` or `@scope/name`.
    source: String,
}

fn is_pnp(dir: &Path) -> bool {
    dir.join(".pnp.cjs").is_file() || dir.join(".pnp.js").is_file()
}

/// Parse the NDJSON stream of `yarn bin --json`, skipping Yarn's own info
/// and warning records.
fn accessible_bins(stdout: &str) -> Result<Vec<AccessibleBin>, Warning> {
    let invalid = |error: serde_json::Error| {
        Warning::about(ProviderId::Yarn, format!("yarn bin --json: {error}"))
    };
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line).map_err(invalid))
        .filter_map(|value| match value {
            Ok(value) if value.get("type").is_some() && value.get("source").is_none() => None,
            Ok(value) => Some(serde_json::from_value(value).map_err(invalid)),
            Err(error) => Some(Err(error)),
        })
        .collect()
}

fn provided_by(bins: &[AccessibleBin], name: &str, at: &Path) -> Option<Installed> {
    let bins: Vec<InstalledBin> = bins
        .iter()
        .filter(|bin| bin.source == name)
        .map(|bin| InstalledBin {
            name: bin.name.clone(),
            runs: BinRuns::Exec,
        })
        .collect();
    (!bins.is_empty()).then(|| Installed {
        at: at.to_owned(),
        default_bin: bins
            .iter()
            .find(|bin| bin.name == unscoped(name))
            .map(|bin| bin.name.clone()),
        bins,
    })
}

/// The binaries a package manifest declares. A string `bin` is named after
/// the package; the default binary is the one named after it, npm's rule for
/// `npx <pkg>`.
fn declared(name: &str, at: &Path, manifest: &Value) -> Result<Installed, Warning> {
    let own = manifest["name"]
        .as_str()
        .map_or_else(|| unscoped(name), unscoped);
    let bins: Vec<InstalledBin> = match &manifest["bin"] {
        Value::String(path) => vec![InstalledBin {
            name: own.to_owned(),
            runs: BinRuns::File(at.join(path)),
        }],
        Value::Object(map) => map
            .iter()
            .map(|(bin, path)| {
                let path = path.as_str().ok_or_else(|| {
                    Warning::about(
                        ProviderId::PackageJson,
                        format!("{name} declares a non-string path for its `{bin}` binary"),
                    )
                })?;
                Ok(InstalledBin {
                    name: bin.clone(),
                    runs: BinRuns::File(at.join(path)),
                })
            })
            .collect::<Result<_, Warning>>()?,
        _ => Vec::new(),
    };
    Ok(Installed {
        at: at.to_owned(),
        default_bin: bins
            .iter()
            .find(|bin| bin.name == unscoped(name) || bin.name == own)
            .map(|bin| bin.name.clone()),
        bins,
    })
}

/// Whether `token` can name an npm package: `name` or `@scope/name`, with no
/// version suffix and no extra path segments.
fn is_package_name(token: &str) -> bool {
    let scoped = token.starts_with('@');
    let body = token.strip_prefix('@').unwrap_or(token);
    !body.is_empty()
        && !body.contains('@')
        && !body.contains('#')
        && !body.contains('\\')
        && body.matches('/').count() == usize::from(scoped)
}

/// The name without its `@scope/`, the name npm links into `node_modules/.bin`.
fn unscoped(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

fn failed(path: &Path, error: &dyn std::fmt::Display) -> Warning {
    Warning::about(
        ProviderId::PackageJson,
        format!("{}: {error}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use runner_core::{BinRuns, InstalledBin};
    use serde_json::json;

    use super::{AccessibleBin, accessible_bins, declared, is_package_name, provided_by, unscoped};

    fn names(bins: &[InstalledBin]) -> Vec<&str> {
        bins.iter().map(|bin| bin.name.as_str()).collect()
    }

    #[test]
    fn package_names_accept_plain_and_scoped_forms_only() {
        assert!(is_package_name("typescript"));
        assert!(is_package_name("@typescript/native"));
        assert!(!is_package_name("typescript@7"));
        assert!(!is_package_name("@scope/pkg@1.2.3"));
        assert!(!is_package_name("user/repo#ref"));
        assert!(!is_package_name("github.com/foo/tool"));
        assert!(!is_package_name(""));
        assert!(!is_package_name("@"));
    }

    #[test]
    fn a_string_bin_is_named_after_the_manifest_name() {
        let at = Path::new("/nm/@typescript/native");
        let installed = declared(
            "@typescript/native",
            at,
            &json!({ "name": "typescript", "bin": "./bin/tsc" }),
        )
        .expect("declared");
        assert_eq!(names(&installed.bins), ["typescript"]);
        assert_eq!(installed.bins[0].runs, BinRuns::File(at.join("./bin/tsc")));
        assert_eq!(installed.default_bin.as_deref(), Some("typescript"));
    }

    #[test]
    fn the_default_bin_is_the_one_named_after_the_package() {
        let installed = declared(
            "eslint",
            Path::new("/nm/eslint"),
            &json!({ "name": "eslint", "bin": { "eslint": "a.js", "x": "x.js" } }),
        )
        .expect("declared");
        assert_eq!(installed.default_bin.as_deref(), Some("eslint"));

        let installed = declared(
            "typescript",
            Path::new("/nm/typescript"),
            &json!({ "name": "typescript", "bin": { "tsc": "a", "tsserver": "b" } }),
        )
        .expect("declared");
        assert_eq!(names(&installed.bins), ["tsc", "tsserver"]);
        assert_eq!(installed.default_bin, None);
    }

    #[test]
    fn a_library_declares_no_bins() {
        let installed = declared(
            "left-pad",
            Path::new("/nm/left-pad"),
            &json!({ "name": "left-pad" }),
        )
        .expect("declared");
        assert_eq!(installed.bins, []);
        assert!(declared("odd", Path::new("/nm/odd"), &json!({ "bin": { "odd": 1 } })).is_err());
    }

    #[test]
    fn yarn_bin_json_parses_and_filters_by_providing_package() {
        let stdout = concat!(
            r#"{"name":"tsc","source":"typescript","path":"/c/typescript.zip/bin/tsc"}"#,
            "\n",
            r#"{"type":"info","name":0,"displayName":"YN0000","indent":"","data":"tsc"}"#,
            "\n",
            r#"{"name":"tsserver","source":"typescript","path":"/c/typescript.zip/bin/s"}"#,
            "\n",
            r#"{"name":"tsx","source":"tsx","path":"/c/tsx.zip/cli.js"}"#,
            "\n",
        );
        let bins = accessible_bins(stdout).expect("parses");
        assert_eq!(
            bins[0],
            AccessibleBin {
                name: "tsc".into(),
                source: "typescript".into()
            }
        );
        let at = PathBuf::from("/repo");
        let installed = provided_by(&bins, "typescript", &at).expect("provided");
        assert_eq!(names(&installed.bins), ["tsc", "tsserver"]);
        assert!(installed.bins.iter().all(|bin| bin.runs == BinRuns::Exec));
        assert!(provided_by(&bins, "esbuild", &at).is_none());
    }

    #[test]
    fn unscoped_strips_the_scope() {
        assert_eq!(unscoped("@typescript/native"), "native");
        assert_eq!(unscoped("typescript"), "typescript");
    }
}
