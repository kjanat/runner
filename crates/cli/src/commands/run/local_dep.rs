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
    pub(super) plan: runner_core::Plan,
    /// `--explain` trace body naming the package directory and binary.
    pub(super) describe: String,
}

/// Observe an installed package's declared binary without choosing how to run it.
pub(super) fn installed_binary(ctx: &ProjectContext, token: &str) -> Result<Option<PathBuf>> {
    if !is_package_name(token) {
        return Ok(None);
    }
    let Some(dir) = installed_dir(&ctx.cwd, token) else {
        return Ok(None);
    };
    let manifest: Manifest =
        serde_json::from_str(&std::fs::read_to_string(dir.join("package.json"))?)?;
    let (name, path) = select_bin(token, &manifest)?;
    let path = dir.join(path);
    if !path.is_file() {
        bail!(
            "{token} declares a `{name}` binary at {}, but nothing is there.\nhint: reinstall \
             dependencies.",
            path.display()
        );
    }
    Ok(Some(path))
}

/// `--package <package> <bin>`: the binary `bin` that the installed
/// `package` declares in its own manifest, so a same-named `.bin` link that
/// another package won is never consulted.
///
/// Returns `Ok(None)` when the package is not installed; the caller then
/// hands the same selection to the package manager. Everything else that
/// goes wrong is an error, since the user named the package.
pub(super) fn try_selected_package(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    package: &str,
    bin: &str,
    args: &[String],
) -> Result<Option<ResolvedBin>> {
    if !is_package_name(package) {
        bail!("--package takes a package name (`typescript`, `@scope/name`), got {package:?}");
    }
    let Some(dir) = installed_dir(&ctx.cwd, package) else {
        return pnp_selected_package(ctx, overrides, package, bin, args);
    };
    let manifest: Manifest =
        serde_json::from_str(&std::fs::read_to_string(dir.join("package.json"))?)?;
    let bin_path = declared_bin(package, bin, &manifest)?;
    let path = dir.join(&bin_path);
    if !path.is_file() {
        bail!(
            "{package} declares `{bin}` at {}, but nothing is there.\nhint: reinstall \
             dependencies.",
            path.display()
        );
    }
    let prepared = super::core::prepare(ctx, overrides, bin)?;
    let dep = |_: &str| Ok(None);
    let plan = runner_core::file_plan(&prepared.cascade(&dep, None), &path, args)
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    Ok(Some(ResolvedBin {
        describe: format!("{bin} from {} (package {package})", dir.display()),
        plan,
    }))
}

/// [`try_selected_package`] for a Yarn Plug'n'Play install, where a
/// dependency has no `node_modules` directory: `yarn bin --json` names every
/// binary and its providing package, and `yarn run <bin>` runs it through
/// the loader. `Ok(None)` when the project is not Plug'n'Play, yarn is unavailable,
/// or the package provides no binary at all.
fn pnp_selected_package(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    package: &str,
    bin: &str,
    args: &[String],
) -> Result<Option<ResolvedBin>> {
    if !crate::tool::yarn::is_pnp(&ctx.root) {
        return Ok(None);
    }
    let Some(bins) = crate::tool::yarn::accessible_bins(&ctx.root)? else {
        return Ok(None);
    };
    let Some(found) = pnp_bin(&bins, package, bin)? else {
        return Ok(None);
    };
    let prepared = super::core::prepare(ctx, overrides, bin)?;
    let present = prepared
        .project
        .present
        .iter()
        .find(|p| p.provider == runner_core::ProviderId::Yarn)
        .ok_or_else(|| anyhow::anyhow!("Plug'n'Play requires an observed Yarn provider"))?;
    let plan = runner_core::plan_with(
        &prepared.tree,
        &prepared.project,
        &prepared.policy,
        present,
        &runner_core::Op::Exec { name: bin, args },
        &runner_providers::REGISTRY,
    )
    .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    Ok(Some(ResolvedBin {
        describe: format!("{bin} from {} (package {package}, Plug'n'Play)", found.path),
        plan,
    }))
}

/// The binary `package` provides under the name `bin`, from a `yarn bin
/// --json` listing. `Ok(None)` when the package provides nothing, an error
/// naming what it does provide when `bin` is not among them.
fn pnp_bin<'a>(
    bins: &'a [crate::tool::yarn::AccessibleBin],
    package: &str,
    bin: &str,
) -> Result<Option<&'a crate::tool::yarn::AccessibleBin>> {
    let provided: Vec<&crate::tool::yarn::AccessibleBin> = bins
        .iter()
        .filter(|entry| entry.source == package)
        .collect();
    if provided.is_empty() {
        return Ok(None);
    }
    if let Some(found) = provided.iter().find(|entry| entry.name == bin) {
        return Ok(Some(found));
    }
    let mut names: Vec<&str> = provided.iter().map(|entry| entry.name.as_str()).collect();
    names.sort_unstable();
    bail!(
        "{package} declares no `{bin}` binary; it exposes {}",
        names.join(", ")
    )
}

/// The path `package` declares for the binary named `bin`.
fn declared_bin(package: &str, bin: &str, manifest: &Manifest) -> Result<String> {
    match &manifest.bin {
        Some(serde_json::Value::String(path)) => {
            let declared = manifest
                .name
                .as_deref()
                .map_or_else(|| unscoped(package), unscoped);
            if declared == bin {
                Ok(path.clone())
            } else {
                bail!("{package} declares one binary, `{declared}`, and no `{bin}`")
            }
        }
        Some(serde_json::Value::Object(map)) if !map.is_empty() => {
            if let Some(path) = map.get(bin) {
                return string_path(package, bin, path);
            }
            let mut names: Vec<&str> = map.keys().map(String::as_str).collect();
            names.sort_unstable();
            bail!(
                "{package} declares no `{bin}` binary; it exposes {}",
                names.join(", ")
            )
        }
        _ => bail!("{package} is installed but declares no binary in its package.json"),
    }
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
        .find(|dir| dir.is_dir())
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
                     pick one with `run --package {token} {}`.",
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
    use super::{Manifest, is_package_name, pnp_bin, select_bin, unscoped};
    use crate::tool::yarn::AccessibleBin;

    fn manifest(json: &str) -> Manifest {
        serde_json::from_str(json).expect("manifest should parse")
    }

    fn accessible(name: &str, source: &str) -> AccessibleBin {
        AccessibleBin {
            name: name.to_string(),
            source: source.to_string(),
            path: format!("/repo/.yarn/cache/{source}.zip/{name}"),
        }
    }

    #[test]
    fn pnp_bin_matches_the_providing_package_only() {
        let bins = [
            accessible("tsc", "typescript"),
            accessible("tsserver", "typescript"),
            accessible("tsx", "tsx"),
        ];

        let found = pnp_bin(&bins, "typescript", "tsc")
            .expect("declared")
            .expect("provided");
        assert_eq!(found.source, "typescript");

        assert!(
            pnp_bin(&bins, "esbuild", "esbuild")
                .expect("no error for an absent package")
                .is_none()
        );

        let err = pnp_bin(&bins, "typescript", "tsx").expect_err("tsx belongs to tsx");
        let text = format!("{err:#}");
        assert!(text.contains("no `tsx` binary"), "{text}");
        assert!(text.contains("tsc, tsserver"), "{text}");
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
    fn a_selected_package_yields_the_named_bin_or_lists_what_it_has() {
        use super::declared_bin;
        let ts =
            manifest(r#"{"name":"typescript","bin":{"tsc":"bin/tsc","tsserver":"bin/tsserver"}}"#);
        assert_eq!(
            declared_bin("typescript", "tsc", &ts).expect("declared"),
            "bin/tsc"
        );
        let err = declared_bin("typescript", "tsx", &ts).expect_err("undeclared");
        assert!(err.to_string().contains("exposes tsc, tsserver"), "{err}");

        let single = manifest(r#"{"name":"@scope/tool","bin":"cli.js"}"#);
        assert_eq!(
            declared_bin("@scope/tool", "tool", &single).expect("declared"),
            "cli.js"
        );
        let err = declared_bin("@scope/tool", "other", &single).expect_err("wrong name");
        assert!(err.to_string().contains("one binary, `tool`"), "{err}");
    }

    #[test]
    fn unscoped_strips_the_scope() {
        assert_eq!(unscoped("@typescript/native"), "native");
        assert_eq!(unscoped("typescript"), "typescript");
    }
}
