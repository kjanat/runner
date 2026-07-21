//! Resolve a token naming a locally installed npm dependency to the
//! executable that dependency declares.
//!
//! Sits between the local-file fallback and the PM-exec fallback: a token
//! like `@typescript/native` is neither a task nor a file, and handing it to
//! `npx` treats an already-installed package as a registry spec (a 404 for an
//! npm alias, whose directory name exists in no registry). The manifest under
//! `node_modules/<token>` names the binary directly, so resolve it there and
//! run it without touching the network.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::local_file::{LocalDispatch, bare_file_in};
use crate::resolver::ResolutionOverrides;
use crate::types::ProjectContext;

/// The subset of a dependency's `package.json` this resolver reads.
#[derive(serde::Deserialize)]
struct Manifest {
    name: Option<String>,
    bin: Option<serde_json::Value>,
}

/// An installed dependency and the binary it was resolved to.
pub(super) struct ResolvedBin {
    pub(super) dispatch: LocalDispatch,
    /// `--explain` trace body naming the package directory and binary.
    pub(super) describe: String,
}

/// Try to interpret `token` as a locally installed dependency and dispatch
/// the binary it declares.
///
/// Returns:
/// - `Ok(None)`, `token` is not a bare package name or is not installed
///   under any `node_modules` from the project root upwards; the caller
///   continues to the PM-exec fallback.
/// - `Ok(Some(_))`, the package declares exactly one usable binary (or one
///   whose name matches the package), resolved to a real file on disk.
/// - `Err(_)`, the package is installed but its binaries are ambiguous,
///   absent, or missing from disk. Reported directly rather than degraded
///   into an `npx` call that would re-download and fail differently.
pub(super) fn try_installed_package(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    token: &str,
    args: &[String],
) -> Result<Option<ResolvedBin>> {
    if !is_package_name(token) {
        return Ok(None);
    }
    let Some(dir) = installed_dir(&ctx.root, token) else {
        return Ok(None);
    };
    let manifest: Manifest = match std::fs::read_to_string(dir.join("package.json")) {
        Ok(raw) => serde_json::from_str(&raw)?,
        Err(_) => return Ok(None),
    };

    let (bin_name, bin_path) = select_bin(token, &manifest)?;
    let Some(dispatch) = bare_file_in(ctx, overrides, &dir, &bin_path, args)? else {
        bail!(
            "{token} declares a `{bin_name}` binary at {}, but nothing is there.\nhint: reinstall \
             dependencies.",
            dir.join(&bin_path).display(),
        );
    };
    Ok(Some(ResolvedBin {
        describe: format!("{bin_name} from {} (local dependency)", dir.display()),
        dispatch,
    }))
}

/// Whether `token` can name an npm package: `name` or `@scope/name`, with no
/// version suffix and no extra path segments. Excludes remote specs the
/// PM-exec fallback still owns (`typescript@7`, `user/repo#ref`,
/// `github.com/owner/tool`).
fn is_package_name(token: &str) -> bool {
    let scoped = token.starts_with('@');
    let body = token.strip_prefix('@').unwrap_or(token);
    !body.is_empty()
        && !body.contains('@')
        && !body.contains('#')
        && !body.contains('\\')
        && body.matches('/').count() == usize::from(scoped)
}

/// The nearest `node_modules/<token>` directory at or above `root`, matching
/// the lookup order Node itself uses.
fn installed_dir(root: &Path, token: &str) -> Option<PathBuf> {
    root.ancestors()
        .map(|ancestor| ancestor.join("node_modules").join(token))
        .find(|dir| dir.join("package.json").is_file())
}

/// Pick the binary to run from a dependency's `bin` field.
///
/// A string `bin` is named after the package itself. An object with one
/// entry is unambiguous. With several, the entry named after the package
/// wins (npm's own rule for `npx <pkg>`); anything else needs the user to
/// say which, and the binary name is itself a runnable token because
/// `node_modules/.bin` is on the task `PATH`.
fn select_bin(token: &str, manifest: &Manifest) -> Result<(String, String)> {
    let declared = manifest.name.as_deref().map(unscoped);
    let requested = unscoped(token);

    match &manifest.bin {
        Some(serde_json::Value::String(path)) => {
            Ok((declared.unwrap_or(requested).to_string(), path.clone()))
        }
        Some(serde_json::Value::Object(map)) if map.len() == 1 => {
            let (name, path) = map.iter().next().expect("one entry");
            Ok((name.clone(), string_path(token, name, path)?))
        }
        Some(serde_json::Value::Object(map)) if !map.is_empty() => {
            let matched = map
                .iter()
                .find(|(name, _)| *name == requested || Some(name.as_str()) == declared);
            let Some((name, path)) = matched else {
                let mut names: Vec<&str> = map.keys().map(String::as_str).collect();
                names.sort_unstable();
                bail!(
                    "{token} exposes {} binaries ({}); none is named after the package.\nhint: \
                     run the binary directly, e.g. `runner run {}`.",
                    map.len(),
                    names.join(", "),
                    names[0],
                );
            };
            Ok((name.clone(), string_path(token, name, path)?))
        }
        _ => bail!("{token} is installed but declares no binary in its package.json"),
    }
}

fn string_path(token: &str, name: &str, path: &serde_json::Value) -> Result<String> {
    match path.as_str() {
        Some(path) => Ok(path.to_string()),
        None => bail!("{token} declares a non-string path for its `{name}` binary"),
    }
}

/// Drop the `@scope/` prefix, leaving the name npm would install into
/// `node_modules/.bin`.
fn unscoped(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::{Manifest, is_package_name, select_bin, unscoped};

    fn manifest(json: &str) -> Manifest {
        serde_json::from_str(json).expect("manifest should parse")
    }

    #[test]
    fn package_names_accept_plain_and_scoped_forms() {
        assert!(is_package_name("typescript"));
        assert!(is_package_name("@typescript/native"));
    }

    #[test]
    fn package_names_reject_remote_specs() {
        // Version-qualified specs, GitHub shorthands and import paths stay
        // with the PM-exec fallback that already resolves them.
        assert!(!is_package_name("typescript@7"));
        assert!(!is_package_name("@scope/pkg@1.2.3"));
        assert!(!is_package_name("user/repo#ref"));
        assert!(!is_package_name("github.com/foo/tool"));
        assert!(!is_package_name(""));
        assert!(!is_package_name("@"));
    }

    #[test]
    fn string_bin_is_named_after_the_package() {
        // The npm-alias shape from #91: the directory is `@typescript/native`
        // but the manifest still calls itself `typescript`.
        let got = select_bin(
            "@typescript/native",
            &manifest(r#"{ "name": "typescript", "bin": "./bin/tsc" }"#),
        )
        .expect("string bin resolves");

        assert_eq!(got, ("typescript".to_string(), "./bin/tsc".to_string()));
    }

    #[test]
    fn single_object_bin_needs_no_selector() {
        let got = select_bin(
            "@typescript/native",
            &manifest(r#"{ "name": "typescript", "bin": { "tsc": "./bin/tsc" } }"#),
        )
        .expect("single bin resolves");

        assert_eq!(got, ("tsc".to_string(), "./bin/tsc".to_string()));
    }

    #[test]
    fn multi_bin_prefers_the_entry_named_after_the_package() {
        let got = select_bin(
            "eslint",
            &manifest(
                r#"{ "name": "eslint", "bin": { "eslint": "./bin/eslint.js", "x": "./x.js" } }"#,
            ),
        )
        .expect("named bin resolves");

        assert_eq!(got.0, "eslint");
    }

    #[test]
    fn multi_bin_without_a_match_is_an_error_naming_the_options() {
        let err = select_bin(
            "typescript",
            &manifest(r#"{ "name": "typescript", "bin": { "tsc": "a", "tsserver": "b" } }"#),
        )
        .expect_err("ambiguous bins must not guess");
        let msg = format!("{err:#}");

        assert!(msg.contains("tsc"), "msg: {msg}");
        assert!(msg.contains("tsserver"), "msg: {msg}");
    }

    #[test]
    fn zero_bin_package_reports_that_specifically() {
        let err = select_bin("left-pad", &manifest(r#"{ "name": "left-pad" }"#))
            .expect_err("a library has nothing to run");

        assert!(format!("{err:#}").contains("declares no binary"));
    }

    #[test]
    fn unscoped_strips_the_scope() {
        assert_eq!(unscoped("@typescript/native"), "native");
        assert_eq!(unscoped("typescript"), "typescript");
    }
}
