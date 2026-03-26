//! `runner list` — display available tasks from all detected sources.

use std::collections::HashSet;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use colored::Colorize;

use crate::tool;
use crate::types::{ProjectContext, TaskSource};

/// Print tasks to stdout.
///
/// In `raw` mode, prints deduplicated task names one per line (for piping
/// into scripts or shell completions). Otherwise prints a human-readable
/// table grouped by source file.
pub(crate) fn list(ctx: &ProjectContext, raw: bool) {
    super::print_warnings(ctx);

    if raw {
        let mut seen = HashSet::new();
        for task in &ctx.tasks {
            if seen.insert(&task.name) {
                println!("{}", task.name);
            }
        }
    } else if ctx.tasks.is_empty() {
        println!("{}", "No tasks found.".dimmed());
    } else {
        print_tasks_grouped(ctx);
    }
}

/// Print tasks grouped by [`TaskSource`], one line per source.
pub(super) fn print_tasks_grouped(ctx: &ProjectContext) {
    let stdout_is_terminal = std::io::stdout().is_terminal();

    let sources = [
        TaskSource::PackageJson,
        TaskSource::TurboJson,
        TaskSource::Makefile,
        TaskSource::Justfile,
        TaskSource::Taskfile,
        TaskSource::DenoJson,
    ];
    for source in sources {
        let names: Vec<&str> = ctx
            .tasks
            .iter()
            .filter(|t| t.source == source)
            .map(|t| t.name.as_str())
            .collect();
        if names.is_empty() {
            continue;
        }

        let label = source_label(source, &ctx.root, stdout_is_terminal);
        println!("  {}{}", label, names.join(", "));
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

    osc8_link(&label, &url)
}

fn source_path(source: TaskSource, root: &Path) -> Option<PathBuf> {
    let path = match source {
        TaskSource::PackageJson => tool::node::find_manifest(root),
        TaskSource::TurboJson => {
            let candidate = root.join(tool::turbo::FILENAME);
            candidate.is_file().then_some(candidate)
        }
        TaskSource::Makefile => {
            tool::files::find_first(root, tool::make::FILENAMES).filter(|path| path.is_file())
        }
        TaskSource::Justfile => tool::just::find_file(root),
        TaskSource::Taskfile => {
            tool::files::find_first(root, tool::go_task::FILENAMES).filter(|path| path.is_file())
        }
        TaskSource::DenoJson => {
            tool::files::find_first(root, tool::deno::FILENAMES).filter(|path| path.is_file())
        }
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

    use super::{file_uri, source_label, source_path};
    use crate::tool::test_support::TempDir;
    use crate::types::TaskSource;

    #[test]
    fn source_path_finds_existing_config_variant() {
        let dir = TempDir::new("list-source-path");
        fs::write(dir.path().join("deno.jsonc"), "{}").expect("deno.jsonc should be written");

        let path = source_path(TaskSource::DenoJson, dir.path())
            .expect("deno task source path should be resolved");

        assert!(path.ends_with("deno.jsonc"));
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
}
