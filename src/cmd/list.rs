//! `runner list` — display available tasks from all detected sources.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use colored::Colorize;
use terminal_size::Height;

use crate::resolver::ResolutionOverrides;
use crate::schema::Project;
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
    verbose: bool,
    source: Option<&str>,
    schema_version: u32,
) -> Result<()> {
    let parsed_source = match source {
        None => None,
        Some(label) => Some(TaskSource::from_label(label).ok_or_else(|| {
            anyhow!(
                "--source {label:?}: unknown source label (expected one of: package.json, \
                 make, just, task, turbo, deno, cargo, go, bacon, mise — legacy filename \
                 forms like justfile/bacon.toml/Makefile are also accepted)",
            )
        })?),
    };

    if json {
        let view = Project::build_with_schema(ctx, overrides, schema_version)
            .into_list_view(parsed_source);
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }
    let _ = schema_version;

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
        let forced = verbose.then_some(RenderMode::Rich);
        let mode = select_render_mode(&filtered, forced);
        print_tasks_grouped_with_mode(&filtered, &ctx.root, mode);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderMode {
    Rich,
    Compact,
}

fn select_render_mode(tasks: &[&Task], forced: Option<RenderMode>) -> RenderMode {
    let height = terminal_size::terminal_size().map(|(_, Height(rows))| usize::from(rows));
    select_render_mode_for(tasks, forced, std::io::stdout().is_terminal(), height)
}

const fn select_render_mode_for(
    tasks: &[&Task],
    forced: Option<RenderMode>,
    stdout_is_terminal: bool,
    terminal_height: Option<usize>,
) -> RenderMode {
    if let Some(mode) = forced {
        return mode;
    }
    if !stdout_is_terminal {
        return RenderMode::Rich;
    }
    match terminal_height {
        Some(rows) if predicted_rich_rows(tasks) + 2 > rows => RenderMode::Compact,
        _ => RenderMode::Rich,
    }
}

const fn predicted_rich_rows(tasks: &[&Task]) -> usize {
    tasks.len()
}

/// Print tasks grouped by [`TaskSource`] with full per-task detail.
///
/// Operates over a borrowed task slice + the project root — the renderer
/// never reads other [`ProjectContext`] fields, so callers that already
/// have a filtered task list pass the slice directly instead of forging
/// a synthetic context.
pub(super) fn print_tasks_grouped(tasks: &[&Task], root: &Path) {
    print_tasks_grouped_with_mode(tasks, root, RenderMode::Rich);
}

fn print_tasks_grouped_with_mode(tasks: &[&Task], root: &Path, mode: RenderMode) {
    let stdout_is_terminal = std::io::stdout().is_terminal();
    print!(
        "{}",
        render_tasks_grouped(tasks, root, mode, stdout_is_terminal)
    );
}

fn render_tasks_grouped(
    tasks: &[&Task],
    root: &Path,
    mode: RenderMode,
    stdout_is_terminal: bool,
) -> String {
    match mode {
        RenderMode::Rich => render_tasks_grouped_rich(tasks, root, stdout_is_terminal),
        RenderMode::Compact => render_tasks_grouped_compact(tasks, stdout_is_terminal),
    }
}

fn render_tasks_grouped_rich(tasks: &[&Task], root: &Path, stdout_is_terminal: bool) -> String {
    let mut out = String::new();
    let sources = [
        TaskSource::PackageJson,
        TaskSource::TurboJson,
        TaskSource::Makefile,
        TaskSource::Justfile,
        TaskSource::Taskfile,
        TaskSource::DenoJson,
        TaskSource::CargoAliases,
        TaskSource::GoPackage,
        TaskSource::BaconToml,
        TaskSource::MiseToml,
    ];
    for source in sources {
        let source_tasks = tasks_for_source(tasks, source);
        if source_tasks.is_empty() {
            continue;
        }

        let label = source_label(source, root, stdout_is_terminal);
        for task in source_tasks {
            if let Some(value) = task.alias_of.as_deref().or(task.description.as_deref()) {
                let value = display_value(value, stdout_is_terminal);
                let _ = writeln!(out, "  {label}{:<20} {value}", task.name);
            } else {
                let _ = writeln!(out, "  {label}{}", task.name);
            }
        }
    }
    out
}

fn render_tasks_grouped_compact(tasks: &[&Task], stdout_is_terminal: bool) -> String {
    let mut out = String::new();
    let sources = [
        TaskSource::PackageJson,
        TaskSource::TurboJson,
        TaskSource::Makefile,
        TaskSource::Justfile,
        TaskSource::Taskfile,
        TaskSource::DenoJson,
        TaskSource::CargoAliases,
        TaskSource::GoPackage,
        TaskSource::BaconToml,
        TaskSource::MiseToml,
    ];
    for source in sources {
        let source_tasks = tasks_for_source(tasks, source);
        if source_tasks.is_empty() {
            continue;
        }
        let names: Vec<&str> = source_tasks.iter().map(|task| task.name.as_str()).collect();
        let label = compact_source_label(source, stdout_is_terminal);
        let _ = writeln!(out, "  {label}{}", names.join(", "));
    }
    out
}

fn tasks_for_source<'a>(tasks: &[&'a Task], source: TaskSource) -> Vec<&'a Task> {
    let mut source_tasks: Vec<&Task> = tasks
        .iter()
        .copied()
        .filter(|task| task.source == source)
        .collect();
    source_tasks.sort_by(|a, b| a.name.cmp(&b.name));
    source_tasks
}

fn compact_source_label(source: TaskSource, stdout_is_terminal: bool) -> String {
    let label = format!("{:<16}", source.label());
    if stdout_is_terminal {
        label.bold().to_string()
    } else {
        label
    }
}

fn display_value(value: &str, stdout_is_terminal: bool) -> String {
    if stdout_is_terminal {
        value.dimmed().to_string()
    } else {
        value.to_string()
    }
}

fn source_label(source: TaskSource, root: &Path, stdout_is_terminal: bool) -> String {
    // Display text is always the canonical `TaskSource::label()` — using
    // `path.file_name()` instead collapses any source whose backing file
    // happens to be named `config.toml` (cargo, uv, pip, rust-toolchain,
    // mise variants, …) into an ambiguous single column. The OSC8 link
    // target still points at the exact resolved path, so terminal users
    // click through to the right file even when the displayed label is
    // the source's canonical name.
    let display = source.label().to_string();
    let padding = 16usize.saturating_sub(display.chars().count());
    let label = format!("{display:<16}").bold().to_string();

    if !stdout_is_terminal {
        return label;
    }

    let Some(path) = source_path(source, root) else {
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
        TaskSource::GoPackage => tool::go_pm::find_file(root),
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
    use std::path::Path;

    use super::{
        RenderMode, file_uri, render_tasks_grouped, select_render_mode_for, source_label,
        source_path,
    };
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
    fn source_label_uses_canonical_source_label_regardless_of_filename_variant() {
        // The displayed text is always `TaskSource::label()` — never
        // the resolved manifest's filename. Mixing in `file_name()`
        // would collapse the many sources whose config happens to
        // be named `config.toml` (cargo, uv, pip, mise variants, …)
        // into an ambiguous shared label, so we keep the canonical
        // source name on the display side and route the OSC8 link
        // target at the exact resolved path.
        let dir = TempDir::new("list-source-label-manifest");
        fs::write(
            dir.path().join("package.yaml"),
            "scripts:\n  build: vite build\n",
        )
        .expect("package.yaml should be written");

        let label = source_label(TaskSource::PackageJson, dir.path(), false);

        assert!(
            label.contains("package.json"),
            "label should render the canonical source name, got: {label:?}",
        );
        assert!(
            !label.contains("package.yaml"),
            "label must not leak the resolved filename variant: {label:?}",
        );
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

    fn task(name: &str, source: TaskSource) -> Task {
        Task {
            name: name.into(),
            source,
            description: None,
            alias_of: None,
            passthrough_to: None,
        }
    }

    #[test]
    fn compact_mode_emits_one_line_per_source() {
        let mut tasks = [
            task("build", TaskSource::Justfile),
            task("test", TaskSource::Justfile),
            task("b", TaskSource::CargoAliases),
            task("lint", TaskSource::CargoAliases),
        ];
        tasks[2].alias_of = Some("build".into());
        let refs: Vec<&Task> = tasks.iter().collect();

        let rendered = render_tasks_grouped(&refs, Path::new("."), RenderMode::Compact, false);

        assert_eq!(
            rendered,
            "  just            build, test\n  cargo           b, lint\n",
        );
    }

    #[test]
    fn auto_mode_picks_compact_when_predicted_height_exceeds_terminal() {
        let tasks: Vec<Task> = (0..30)
            .map(|idx| task(&format!("task-{idx}"), TaskSource::Justfile))
            .collect();
        let refs: Vec<&Task> = tasks.iter().collect();

        let mode = select_render_mode_for(&refs, None, true, Some(10));

        assert_eq!(mode, RenderMode::Compact);
    }

    #[test]
    fn auto_mode_picks_rich_when_terminal_height_fits() {
        let tasks: Vec<Task> = (0..30)
            .map(|idx| task(&format!("task-{idx}"), TaskSource::Justfile))
            .collect();
        let refs: Vec<&Task> = tasks.iter().collect();

        let mode = select_render_mode_for(&refs, None, true, Some(200));

        assert_eq!(mode, RenderMode::Rich);
    }

    #[test]
    fn auto_mode_defaults_rich_on_non_tty() {
        let tasks: Vec<Task> = (0..30)
            .map(|idx| task(&format!("task-{idx}"), TaskSource::Justfile))
            .collect();
        let refs: Vec<&Task> = tasks.iter().collect();

        let mode = select_render_mode_for(&refs, None, false, Some(10));

        assert_eq!(mode, RenderMode::Rich);
    }

    #[test]
    fn rich_mode_renders_alias_target_in_value_column() {
        let tasks = [Task {
            name: "b".into(),
            source: TaskSource::Justfile,
            description: None,
            alias_of: Some("build".into()),
            passthrough_to: None,
        }];
        let refs: Vec<&Task> = tasks.iter().collect();

        let rendered = render_tasks_grouped(&refs, Path::new("."), RenderMode::Rich, false);

        assert!(rendered.contains('b'));
        assert!(rendered.contains("build"));
    }

    #[test]
    fn verbose_flag_overrides_compact_selection() {
        let tasks: Vec<Task> = (0..30)
            .map(|idx| task(&format!("task-{idx}"), TaskSource::Justfile))
            .collect();
        let refs: Vec<&Task> = tasks.iter().collect();

        let mode = select_render_mode_for(&refs, Some(RenderMode::Rich), true, Some(10));

        assert_eq!(mode, RenderMode::Rich);
    }
}
