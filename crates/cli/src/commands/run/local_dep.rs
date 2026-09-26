//! Resolve a token naming an installed dependency to the executable that
//! dependency declares.
//!
//! Sits between the local-file fallback and the package-manager exec
//! fallback: a token like `@typescript/native` is neither a task nor a file,
//! and handing it to the exec primitive treats an installed package as a
//! registry spec. The providers that know where packages are installed name
//! its binaries directly, so it runs without touching the network.

use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use runner_core::{BinRuns, Installed, InstalledBin, ProviderId};

use crate::provider::Named;
use crate::resolver::ResolutionOverrides;
use crate::types::ProjectContext;

/// An installed dependency and the binary it was resolved to.
pub(super) struct ResolvedBin {
    pub(super) plan: runner_core::Plan,
    /// `--dry-run` trace body naming the package and binary.
    pub(super) describe: String,
}

/// The package `name` as the first provider that knows it installed it.
fn installed(ctx: &ProjectContext, name: &str) -> Result<Option<(ProviderId, Installed)>> {
    let tree = super::core::tree(ctx);
    for provider in runner_providers::REGISTRY.iter() {
        let Some(cap) = provider.caps.packages else {
            continue;
        };
        if let Some(found) =
            (cap.installed)(&tree, &ctx.cwd, name).map_err(|warning| anyhow!(warning.message))?
        {
            return Ok(Some((provider.id, found)));
        }
    }
    Ok(None)
}

/// The file an installed package's own binary runs, without choosing how.
pub(super) fn installed_binary(ctx: &ProjectContext, token: &str) -> Result<Option<PathBuf>> {
    let Some((_, package)) = installed(ctx, token)? else {
        return Ok(None);
    };
    let bin = default_bin(token, &package)?;
    match &bin.runs {
        BinRuns::File(path) => existing(token, bin, path).map(Some),
        BinRuns::Exec => Ok(None),
    }
}

/// `--package <package> <bin>`: the binary `bin` that the installed
/// `package` declares, so a same-named binary of another package is never
/// consulted.
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
    let Some((provider, installed)) = installed(ctx, package)? else {
        return Ok(None);
    };
    let found = named_bin(package, bin, &installed)?;
    let prepared = super::core::prepare(ctx, overrides, bin)?;
    let through = match &found.runs {
        BinRuns::File(_) => String::new(),
        BinRuns::Exec => format!(", through {}", provider.label()),
    };
    let plan = match &found.runs {
        BinRuns::File(path) => {
            let path = existing(package, found, path)?;
            let dep = |_: &str| Ok(None);
            runner_core::dependency_plan(&prepared.cascade(&dep, None), &path, args)
                .map_err(|error| anyhow!("{error:?}"))?
        }
        BinRuns::Exec => {
            let scope = runner_core::plan::scope_at(&prepared.tree, &prepared.tree.cwd);
            let present = prepared
                .project
                .present_in(provider, &scope)
                .ok_or_else(|| {
                    anyhow!(
                        "{package} is installed through {}, which was not observed",
                        provider.label()
                    )
                })?;
            runner_core::plan_with(
                &prepared.tree,
                &prepared.project,
                &prepared.policy,
                present,
                &runner_core::Op::Exec { name: bin, args },
                &runner_providers::REGISTRY,
            )
            .map_err(|error| anyhow!("{error:?}"))?
        }
    };
    Ok(Some(ResolvedBin {
        describe: format!(
            "{bin} from {} (package {package}{through})",
            installed.at.display()
        ),
        plan,
    }))
}

/// The binary to run for a token naming the package itself: its only one,
/// or the one named after it.
fn default_bin<'a>(token: &str, package: &'a Installed) -> Result<&'a InstalledBin> {
    if let [only] = package.bins.as_slice() {
        return Ok(only);
    }
    if let Some(bin) = package
        .default_bin
        .as_deref()
        .and_then(|name| package.bins.iter().find(|bin| bin.name == name))
    {
        return Ok(bin);
    }
    let names = sorted_names(package);
    let Some(first) = names.first() else {
        bail!("{token} is installed but declares no binary");
    };
    bail!(
        "{token} exposes {} binaries ({}); none is named after the package.\nhint: pick one with \
         `run --package {token} {first}`.",
        names.len(),
        names.join(", "),
    )
}

/// The binary `package` declares under the name `bin`.
fn named_bin<'a>(package: &str, bin: &str, installed: &'a Installed) -> Result<&'a InstalledBin> {
    if let Some(found) = installed.bins.iter().find(|found| found.name == bin) {
        return Ok(found);
    }
    let names = sorted_names(installed);
    if names.is_empty() {
        bail!("{package} is installed but declares no binary");
    }
    bail!(
        "{package} declares no `{bin}` binary; it exposes {}",
        names.join(", ")
    )
}

fn sorted_names(installed: &Installed) -> Vec<&str> {
    let mut names: Vec<&str> = installed.bins.iter().map(|bin| bin.name.as_str()).collect();
    names.sort_unstable();
    names
}

fn existing(package: &str, bin: &InstalledBin, path: &std::path::Path) -> Result<PathBuf> {
    if !path.is_file() {
        bail!(
            "{package} declares a `{}` binary at {}, but nothing is there.\nhint: reinstall \
             dependencies.",
            bin.name,
            path.display()
        );
    }
    Ok(path.to_owned())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use runner_core::{BinRuns, Installed, InstalledBin};

    use super::{default_bin, named_bin};

    fn package(bins: &[&str], default_bin: Option<&str>) -> Installed {
        Installed {
            at: PathBuf::from("/nm/pkg"),
            bins: bins
                .iter()
                .map(|name| InstalledBin {
                    name: (*name).to_owned(),
                    runs: BinRuns::File(PathBuf::from(format!("/nm/pkg/{name}"))),
                })
                .collect(),
            default_bin: default_bin.map(ToOwned::to_owned),
        }
    }

    #[test]
    fn a_single_bin_needs_no_selector() {
        let installed = package(&["tsc"], None);
        assert_eq!(
            default_bin("@typescript/native", &installed)
                .expect("single")
                .name,
            "tsc"
        );
    }

    #[test]
    fn several_bins_take_the_default_or_name_the_options() {
        let installed = package(&["eslint", "x"], Some("eslint"));
        assert_eq!(
            default_bin("eslint", &installed).expect("default").name,
            "eslint"
        );

        let installed = package(&["tsserver", "tsc"], None);
        let text = format!(
            "{:#}",
            default_bin("typescript", &installed).expect_err("ambiguous")
        );
        assert!(text.contains("tsc, tsserver"), "{text}");
        assert!(text.contains("run --package typescript tsc"), "{text}");
    }

    #[test]
    fn a_library_reports_that_it_declares_no_binary() {
        let installed = package(&[], None);
        let text = format!(
            "{:#}",
            default_bin("left-pad", &installed).expect_err("library")
        );
        assert!(text.contains("declares no binary"), "{text}");
    }

    #[test]
    fn a_selected_package_yields_the_named_bin_or_lists_what_it_has() {
        let installed = package(&["tsc", "tsserver"], None);
        assert_eq!(
            named_bin("typescript", "tsc", &installed)
                .expect("declared")
                .name,
            "tsc"
        );
        let text = format!(
            "{:#}",
            named_bin("typescript", "tsx", &installed).expect_err("undeclared")
        );
        assert!(text.contains("exposes tsc, tsserver"), "{text}");
    }
}
