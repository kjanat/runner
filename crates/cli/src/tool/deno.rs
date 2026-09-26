//! Deno, secure JavaScript/TypeScript runtime.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::tool::files;
use crate::tool::node;

/// Supported Deno config filenames (priority order).
pub(crate) const FILENAMES: &[&str] = &["deno.json", "deno.jsonc"];

/// Resolve the nearest supported Deno config while walking upward.
pub(crate) fn find_config_upwards(dir: &Path) -> Option<PathBuf> {
    let boundary = files::vcs_root(dir);

    for ancestor in dir.ancestors() {
        if !within_boundary(ancestor, boundary.as_deref()) {
            break;
        }

        let Some(path) = files::find_first(ancestor, FILENAMES).filter(|path| path.is_file())
        else {
            continue;
        };

        return Some(path);
    }

    None
}

fn within_boundary(path: &Path, boundary: Option<&Path>) -> bool {
    boundary.is_none_or(|boundary| path == boundary || path.starts_with(boundary))
}

/// Whether this Deno project materializes a local `node_modules/`, in which
/// case `deno install` writes the same directory a node-ecosystem PM
/// (`npm`/`yarn`/`pnpm`/`bun`) would.
///
/// An explicit `nodeModulesDir` decides it: `auto`/`manual` (Deno 2.x) or the
/// legacy boolean `true` write the directory, `none`/`false` keep dependencies
/// in Deno's global cache. Absent, the answer follows Deno's own default, which
/// depends on the project: Deno's docs say "projects with a `package.json`
/// default to the manual `node_modules` mode, which is why the explicit `deno
/// install` step is needed", and everything else resolves npm packages from the
/// global cache.
pub(crate) fn writes_node_modules(dir: &Path) -> bool {
    declared_node_modules_dir(dir).unwrap_or_else(|| node::has_package_json(dir))
}

/// The `nodeModulesDir` setting from the nearest Deno config, as a yes/no.
/// `None` when there is no config, none that parses, or no such key in it.
fn declared_node_modules_dir(dir: &Path) -> Option<bool> {
    #[derive(Deserialize)]
    struct Partial {
        #[serde(rename = "nodeModulesDir")]
        node_modules_dir: Option<serde_json::Value>,
    }
    let path = find_config_upwards(dir)?;
    let content = std::fs::read_to_string(&path).ok()?;
    let parsed = json5::from_str::<Partial>(&content).ok()?;
    match parsed.node_modules_dir? {
        serde_json::Value::Bool(enabled) => Some(enabled),
        serde_json::Value::String(mode) => match mode.as_str() {
            "auto" | "manual" => Some(true),
            "none" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::find_config_upwards;
    use crate::tool::test_support::TempDir;

    #[test]
    fn writes_node_modules_reads_node_modules_dir() {
        use super::writes_node_modules;
        let cases = [
            (r#"{ "nodeModulesDir": "auto" }"#, true),
            (r#"{ "nodeModulesDir": "manual" }"#, true),
            (r#"{ "nodeModulesDir": "none" }"#, false),
            (r#"{ "nodeModulesDir": true }"#, true),
            (r#"{ "nodeModulesDir": false }"#, false),
            (r#"{ "tasks": {} }"#, false), // unset
            (r"{ /* jsonc */ }", false),
        ];
        for (i, (body, expected)) in cases.iter().enumerate() {
            let dir = TempDir::new(&format!("deno-nmd-{i}"));
            fs::write(dir.path().join("deno.json"), body).expect("write config");
            assert_eq!(writes_node_modules(dir.path()), *expected, "body: {body}");
        }
    }

    #[test]
    fn unset_node_modules_dir_follows_denos_own_default() {
        use super::writes_node_modules;

        // "Projects with a package.json default to the manual node_modules
        // mode, which is why the explicit `deno install` step is needed."
        let with_manifest = TempDir::new("deno-nmd-package-json");
        fs::write(with_manifest.path().join("deno.json"), r#"{ "tasks": {} }"#).expect("config");
        fs::write(with_manifest.path().join("package.json"), r#"{"name":"x"}"#)
            .expect("package.json");
        assert!(
            writes_node_modules(with_manifest.path()),
            "`deno install` populates node_modules for a package.json project",
        );

        // Without one, npm packages resolve from the global cache.
        let without = TempDir::new("deno-nmd-no-package-json");
        fs::write(without.path().join("deno.json"), r#"{ "tasks": {} }"#).expect("config");
        assert!(!writes_node_modules(without.path()));

        // An explicit `none` still wins over the package.json default.
        let opted_out = TempDir::new("deno-nmd-opted-out");
        fs::write(
            opted_out.path().join("deno.json"),
            r#"{ "nodeModulesDir": "none" }"#,
        )
        .expect("config");
        fs::write(opted_out.path().join("package.json"), r#"{"name":"x"}"#).expect("package.json");
        assert!(!writes_node_modules(opted_out.path()));

        // Unknown values do not silently opt out; they defer to the project
        // default just like an absent declaration.
        for (i, value) in [r#""future-mode""#, "42"].iter().enumerate() {
            let invalid = TempDir::new(&format!("deno-nmd-invalid-{i}"));
            fs::write(
                invalid.path().join("deno.json"),
                format!(r#"{{ "nodeModulesDir": {value} }}"#),
            )
            .expect("config");
            fs::write(invalid.path().join("package.json"), r#"{"name":"x"}"#)
                .expect("package.json");
            assert!(writes_node_modules(invalid.path()), "value: {value}");
        }
    }

    #[test]
    fn writes_node_modules_false_without_config() {
        use super::writes_node_modules;
        let dir = TempDir::new("deno-no-config");
        assert!(!writes_node_modules(dir.path()));
    }

    #[test]
    fn a_config_less_deno_project_with_a_package_json_still_writes_node_modules() {
        // Deno needs no config at all: `deno.lock` alone detects it, and
        // `deno install` reads the package.json.
        use super::writes_node_modules;
        let dir = TempDir::new("deno-no-config-package-json");
        fs::write(dir.path().join("package.json"), r#"{"name":"x"}"#).expect("package.json");
        assert!(writes_node_modules(dir.path()));
    }

    #[test]
    fn find_config_upwards_prefers_nearest_config() {
        let dir = TempDir::new("deno-config-upwards");
        let nested = dir.path().join("apps").join("site").join("src");
        fs::create_dir_all(&nested).expect("nested dir should be created");
        fs::write(
            dir.path().join("deno.jsonc"),
            "{ tasks: { root: 'deno task root' } }",
        )
        .expect("root deno.jsonc should be written");
        fs::write(
            dir.path().join("apps").join("site").join("deno.json"),
            r#"{ "tasks": { "member": "deno task member" } }"#,
        )
        .expect("member deno.json should be written");

        let path = find_config_upwards(&nested).expect("nearest config should resolve");

        assert!(path.ends_with("apps/site/deno.json"));
    }

    #[test]
    fn find_config_upwards_reaches_the_root_config_from_any_directory_beneath_it() {
        let dir = TempDir::new("deno-config-workspace-excluded");
        let nested = dir.path().join("apps").join("site").join("src");
        fs::create_dir_all(&nested).expect("nested dir should be created");
        fs::write(
            dir.path().join("deno.json"),
            r#"{ "workspace": ["./packages/*"] }"#,
        )
        .expect("root deno.json should be written");

        assert_eq!(
            find_config_upwards(&nested),
            Some(dir.path().join("deno.json"))
        );
    }

    #[test]
    fn find_config_upwards_accepts_workspace_member_paths() {
        let dir = TempDir::new("deno-config-workspace-member");
        let nested = dir.path().join("packages").join("site").join("src");
        fs::create_dir_all(&nested).expect("nested dir should be created");
        fs::write(
            dir.path().join("deno.json"),
            r#"{ "workspace": { "members": ["packages/*"] } }"#,
        )
        .expect("root deno.json should be written");

        let path = find_config_upwards(&nested).expect("workspace member should resolve");

        assert!(path.ends_with("deno.json"));
    }
}
