//! `runner list` — display available tasks from all detected sources.

use std::collections::HashSet;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use colored::Colorize;

use crate::report::Project;
use crate::resolver::ResolutionOverrides;
use crate::tool;
use crate::types::{ProjectContext, Task, TaskSource};

/// Print tasks to stdout.
///
/// In `raw` mode, prints deduplicated task names one per line (for piping
/// into scripts or shell completions). Otherwise prints a human-readable
/// table grouped by source file.
///
/// # Errors
///
/// Returns an error when `source` doesn't name a known [`TaskSource`],
/// or when `--json` serialization fails. The human-output path never
/// errors.
pub(crate) fn list(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    raw: bool,
    json: bool,
    source: Option<&str>,
) -> Result<()> {
    let parsed_source = match source {
        None => None,
        Some(label) => Some(TaskSource::from_label(label).ok_or_else(|| {
            anyhow!(
                "--source {label:?}: unknown source label (expected one of: package.json, \
                 Makefile, justfile, Taskfile, turbo.json, deno.json, cargo, bacon.toml)",
            )
        })?),
    };

    if json {
        let view = Project::build(ctx, overrides).into_list_view(parsed_source);
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }

    super::print_warnings(ctx, overrides, None);

    let filtered: Vec<&Task> = ctx
        .tasks
        .iter()
        .filter(|t| parsed_source.is_none_or(|s| t.source == s))
        .collect();

    if raw {
        let mut seen = HashSet::new();
        for task in &filtered {
            if seen.insert(task.name.as_str()) {
                println!("{}", task.name);
            }
        }
    } else if filtered.is_empty() {
        println!("{}", "No tasks found.".dimmed());
    } else {
        print_tasks_grouped(&filtered, &ctx.root);
    }
    Ok(())
}

/// Print tasks grouped by [`TaskSource`], one line per source.
///
/// Operates over a borrowed task slice + the project root — the renderer
/// never reads other [`ProjectContext`] fields, so callers that already
/// have a filtered task list pass the slice directly instead of forging
/// a synthetic context.
pub(super) fn print_tasks_grouped(tasks: &[&Task], root: &Path) {
    let stdout_is_terminal = std::io::stdout().is_terminal();

    let sources = [
        TaskSource::PackageJson,
        TaskSource::TurboJson,
        TaskSource::Makefile,
        TaskSource::Justfile,
        TaskSource::Taskfile,
        TaskSource::DenoJson,
        TaskSource::CargoAliases,
        TaskSource::BaconToml,
        TaskSource::MiseToml,
    ];
    for source in sources {
        let (recipes, aliases): (Vec<&Task>, Vec<&Task>) = tasks
            .iter()
            .copied()
            .filter(|t| t.source == source)
            .partition(|t| t.alias_of.is_none());
        if recipes.is_empty() && aliases.is_empty() {
            continue;
        }

        let label = source_label(source, root, stdout_is_terminal);
        if !recipes.is_empty() {
            let has_any_desc = recipes.iter().any(|t| t.description.is_some());
            if has_any_desc {
                for task in &recipes {
                    if let Some(desc) = task.description.as_deref() {
                        println!("  {label}{:<20} {}", task.name, desc.dimmed());
                    } else {
                        println!("  {label}{}", task.name);
                    }
                }
            } else {
                let names: Vec<&str> = recipes.iter().map(|t| t.name.as_str()).collect();
                println!("  {}{}", label, names.join(", "));
            }
        }
        if !aliases.is_empty() {
            println!(
                "{}",
                format_aliases_line(source, &aliases, stdout_is_terminal)
            );
        }
    }
}

fn format_aliases_line(source: TaskSource, aliases: &[&Task], stdout_is_terminal: bool) -> String {
    let aliases_label = alias_label(source, stdout_is_terminal);
    let parts: Vec<String> = aliases
        .iter()
        .map(|t| {
            let target = t.alias_of.as_deref().unwrap_or("?");
            if stdout_is_terminal {
                format!("{} {} {}", t.name, "→".dimmed(), target.dimmed())
            } else {
                format!("{} → {}", t.name, target)
            }
        })
        .collect();
    format!("  {aliases_label}{}", parts.join(", "))
}

fn alias_label(source: TaskSource, stdout_is_terminal: bool) -> String {
    let text = format!("{} (aliases)", source.label());
    // Alias labels like "package.json (aliases)" exceed the 16-col recipe
    // label, so fall through to a single trailing space rather than running
    // directly into the first alias.
    let label = if text.chars().count() < 16 {
        format!("{text:<16}")
    } else {
        format!("{text} ")
    };
    if stdout_is_terminal {
        label.bold().to_string()
    } else {
        label
    }
}

fn source_label(source: TaskSource, root: &Path, stdout_is_terminal: bool) -> String {
    let path = source_path(source, root);
    let display = path
        .as_deref()
        .and_then(|path| path.file_name())
        .map_or_else(
            || source.label().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
    let padding = 16usize.saturating_sub(display.chars().count());
    let label = format!("{display:<16}").bold().to_string();

    if !stdout_is_terminal {
        return label;
    }

    let Some(path) = path else {
        return label;
    };
    let Some(url) = file_uri(&path) else {
        return label;
    };

    format!(
        "{}{}",
        osc8_link(&display.bold().to_string(), &url),
        " ".repeat(padding)
    )
}

fn source_path(source: TaskSource, root: &Path) -> Option<PathBuf> {
    let path = match source {
        TaskSource::PackageJson => tool::node::find_manifest_upwards(root),
        TaskSource::TurboJson => tool::turbo::find_config(root),
        TaskSource::Makefile => {
            tool::files::find_first(root, tool::make::FILENAMES).filter(|path| path.is_file())
        }
        TaskSource::Justfile => tool::just::find_file(root),
        TaskSource::Taskfile => {
            tool::files::find_first(root, tool::go_task::FILENAMES).filter(|path| path.is_file())
        }
        TaskSource::DenoJson => tool::deno::find_config_upwards(root),
        TaskSource::CargoAliases => tool::cargo_aliases::find_anchor(root),
        TaskSource::BaconToml => {
            tool::files::find_first(root, tool::bacon::FILENAMES).filter(|path| path.is_file())
        }
        TaskSource::MiseToml => tool::mise::find_file(root),
    }?;

    Some(path.canonicalize().unwrap_or(path))
}

fn file_uri(path: &Path) -> Option<String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        let encoded = percent_encode_path(absolute.as_os_str().as_bytes());
        Some(format!("file://{encoded}"))
    }

    #[cfg(not(unix))]
    {
        let raw = absolute.to_string_lossy().replace('\\', "/");
        Some(format!("file:///{raw}"))
    }
}

#[cfg(unix)]
fn percent_encode_path(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let mut encoded = String::with_capacity(bytes.len());
    for &byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0F)]));
        }
    }

    encoded
}

fn osc8_link(label: &str, url: &str) -> String {
    format!("\u{1b}]8;;{url}\u{1b}\\{label}\u{1b}]8;;\u{1b}\\")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{file_uri, format_aliases_line, source_label, source_path};
    use crate::tool::test_support::TempDir;
    use crate::types::{Task, TaskSource};

    #[test]
    fn source_path_finds_existing_config_variant() {
        let dir = TempDir::new("list-source-path");
        fs::write(dir.path().join("deno.jsonc"), "{}").expect("deno.jsonc should be written");

        let path = source_path(TaskSource::DenoJson, dir.path())
            .expect("deno task source path should be resolved");

        assert!(path.ends_with("deno.jsonc"));
    }

    #[test]
    fn source_path_finds_turbo_jsonc_variant() {
        let dir = TempDir::new("list-source-path-turbo-jsonc");
        fs::write(dir.path().join("turbo.jsonc"), "{}").expect("turbo.jsonc should be written");

        let path = source_path(TaskSource::TurboJson, dir.path())
            .expect("turbo task source path should be resolved");

        assert!(path.ends_with("turbo.jsonc"));
    }

    #[test]
    fn source_path_supports_taskfile_dist_variants() {
        let dir = TempDir::new("list-taskfile-dist");
        fs::write(
            dir.path().join("Taskfile.dist.yml"),
            "version: '3'\ntasks: {}\n",
        )
        .expect("Taskfile.dist.yml should be written");

        let path = source_path(TaskSource::Taskfile, dir.path())
            .expect("taskfile path should resolve from dist variant");

        assert!(path.ends_with("Taskfile.dist.yml"));
    }

    #[test]
    fn source_label_uses_osc8_when_terminal_and_file_exists() {
        let dir = TempDir::new("list-source-label-link");
        fs::write(dir.path().join("package.json"), "{}").expect("package.json should be written");

        let label = source_label(TaskSource::PackageJson, dir.path(), true);

        assert!(label.contains("\u{1b}]8;;file://"));
        assert!(label.contains("package.json"));
    }

    #[test]
    fn source_label_keeps_padding_outside_osc8_link() {
        let dir = TempDir::new("list-source-label-padding");
        fs::write(dir.path().join("package.json"), "{}").expect("package.json should be written");

        let label = source_label(TaskSource::PackageJson, dir.path(), true);

        let close = label
            .rfind("\u{1b}]8;;\u{1b}\\")
            .expect("label should contain OSC8 close sequence");

        assert!(label[..close].contains("package.json"));
        assert_eq!(&label[close + "\u{1b}]8;;\u{1b}\\".len()..], "    ");
    }

    #[test]
    fn source_label_uses_resolved_manifest_filename() {
        let dir = TempDir::new("list-source-label-manifest");
        fs::write(
            dir.path().join("package.yaml"),
            "scripts:\n  build: vite build\n",
        )
        .expect("package.yaml should be written");

        let label = source_label(TaskSource::PackageJson, dir.path(), false);

        assert!(label.contains("package.yaml"));
    }

    #[test]
    fn source_label_is_plain_when_not_terminal() {
        let dir = TempDir::new("list-source-label-plain");
        fs::write(dir.path().join("package.json"), "{}").expect("package.json should be written");

        let label = source_label(TaskSource::PackageJson, dir.path(), false);

        assert!(label.contains("package.json"));
        assert!(!label.contains("\u{1b}]8;;"));
    }

    #[test]
    fn file_uri_uses_file_scheme() {
        let dir = TempDir::new("list-file-uri");
        fs::write(dir.path().join("package.json"), "{}").expect("package.json should be written");

        let uri = file_uri(&dir.path().join("package.json")).expect("file URI should be generated");

        assert!(uri.starts_with("file://"));
    }

    #[test]
    fn format_aliases_line_joins_aliases_with_arrow() {
        let aliases = [
            Task {
                name: "b".into(),
                source: TaskSource::Justfile,
                description: None,
                alias_of: Some("build".into()),
                passthrough_to: None,
            },
            Task {
                name: "br".into(),
                source: TaskSource::Justfile,
                description: None,
                alias_of: Some("build-release".into()),
                passthrough_to: None,
            },
        ];
        let refs: Vec<&Task> = aliases.iter().collect();
        let line = format_aliases_line(TaskSource::Justfile, &refs, false);
        assert_eq!(line, "  justfile (aliases) b → build, br → build-release");
    }
}
