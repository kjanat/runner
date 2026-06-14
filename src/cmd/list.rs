//! `runner list` — display available tasks from all detected sources.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use colored::Colorize;
use terminal_size::{Height, Width};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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
    source: Option<&str>,
    schema_version: u32,
) -> Result<()> {
    let parsed_source = match source {
        None => None,
        Some(label) => Some(TaskSource::from_label(label).ok_or_else(|| {
            let expected = expected_source_labels();
            anyhow!(
                "--source {label:?}: unknown source label (expected one of: {expected} — legacy \
                 filename forms like justfile/bacon.toml/Makefile are also accepted)",
            )
        })?),
    };

    if json {
        let view = Project::build_with_schema(ctx, overrides, schema_version, false)
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
        // `runner list` is an explicit request for the task list —
        // always full detail, never collapse. The height-adaptive
        // compact path is reserved for the bare `runner` / `runner
        // info` glance view (see `print_tasks_grouped`).
        print_tasks_grouped_with_mode(&filtered, &ctx.root, RenderMode::Rich);
    }
    Ok(())
}

fn expected_source_labels() -> String {
    TaskSource::all()
        .iter()
        .copied()
        .map(TaskSource::label)
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderMode {
    Rich,
    Compact,
}

const ROW_INDENT: &str = "  ";
const SOURCE_COL_WIDTH: usize = 16;
const TASK_COL_MIN_WIDTH: usize = 20;

fn select_render_mode(tasks: &[&Task], reserved_rows: usize) -> RenderMode {
    let height = terminal_size::terminal_size().map(|(_, Height(rows))| usize::from(rows));
    select_render_mode_for(
        tasks,
        std::io::stdout().is_terminal(),
        height,
        reserved_rows,
    )
}

/// `reserved_rows` is output the caller has already emitted (or will
/// emit) above the task list and that therefore eats into the visible
/// budget — e.g. the `runner` info banner (version line, Package
/// Managers / Task Runners / Node / Monorepo rows, blank separators).
/// `runner list` has no banner and passes `0`. The `+ 2` is a fixed
/// allowance for the shell prompt that reappears after rendering plus a
/// one-row safety margin.
const fn select_render_mode_for(
    tasks: &[&Task],
    stdout_is_terminal: bool,
    terminal_height: Option<usize>,
    reserved_rows: usize,
) -> RenderMode {
    if !stdout_is_terminal {
        return RenderMode::Rich;
    }
    match terminal_height {
        Some(rows) if predicted_rich_rows(tasks) + 2 + reserved_rows > rows => RenderMode::Compact,
        _ => RenderMode::Rich,
    }
}

const fn predicted_rich_rows(tasks: &[&Task]) -> usize {
    tasks.len()
}

/// Print tasks grouped by [`TaskSource`], collapsing to compact mode
/// when the rich form would overflow the terminal.
///
/// Operates over a borrowed task slice + the project root — the renderer
/// never reads other [`ProjectContext`] fields, so callers that already
/// have a filtered task list pass the slice directly instead of forging
/// a synthetic context.
///
/// Used by both `runner list` and the bare `runner` (info) view, so the
/// height-adaptive selection must live here, not only in [`list`].
///
/// `reserved_rows` is the number of lines the caller has already printed
/// above the task list (the `runner` info banner). `runner list` passes
/// `0`.
pub(super) fn print_tasks_grouped(tasks: &[&Task], root: &Path, reserved_rows: usize) {
    print_tasks_grouped_with_mode(tasks, root, select_render_mode(tasks, reserved_rows));
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
    let terminal_width = stdout_is_terminal.then(terminal_width).flatten();

    match mode {
        RenderMode::Rich => {
            render_tasks_grouped_rich(tasks, root, stdout_is_terminal, terminal_width)
        }
        RenderMode::Compact => render_tasks_grouped_compact(tasks, stdout_is_terminal),
    }
}

fn terminal_width() -> Option<usize> {
    terminal_size::terminal_size().map(|(Width(columns), _)| usize::from(columns))
}

fn render_tasks_grouped_rich(
    tasks: &[&Task],
    root: &Path,
    stdout_is_terminal: bool,
    terminal_width: Option<usize>,
) -> String {
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
        TaskSource::PyprojectScripts,
    ];
    for source in sources {
        let source_tasks = tasks_for_source(tasks, source);
        if source_tasks.is_empty() {
            continue;
        }

        let label = source_label(source, root, stdout_is_terminal);
        let label_width = padded_column_width(source.label(), SOURCE_COL_WIDTH);
        for (task, alias_names) in fold_aliases(&source_tasks) {
            // A folded canonical carries its aliases in the name cell; a
            // standalone alias keeps showing its target in the value cell.
            let name = name_with_aliases(&task.name, &alias_names);
            let value = if alias_names.is_empty() {
                task.alias_of.as_deref().or(task.description.as_deref())
            } else {
                task.description.as_deref()
            };
            out.push_str(&render_rich_row(
                &label,
                label_width,
                &name,
                value,
                stdout_is_terminal,
                terminal_width,
            ));
        }
    }
    out
}

fn render_rich_row(
    label: &str,
    label_width: usize,
    task_name: &str,
    value: Option<&str>,
    stdout_is_terminal: bool,
    terminal_width: Option<usize>,
) -> String {
    let task_width = padded_column_width(task_name, TASK_COL_MIN_WIDTH);
    let task_cell = pad_visible(task_name, task_width);

    match value {
        None => format!("{ROW_INDENT}{label}{task_cell}\n"),
        Some(value) if !stdout_is_terminal => {
            format!(
                "{ROW_INDENT}{label}{task_cell} {}\n",
                display_value(value, false)
            )
        }
        Some(value) => {
            let prefix = format!("{ROW_INDENT}{label}{task_cell} ");
            let Some(value_width) = value_column_width(label_width, task_width, terminal_width)
            else {
                return format!("{prefix}{}\n", display_value(value, true));
            };

            let continuation_prefix = format!(
                "{ROW_INDENT}{}{} ",
                " ".repeat(label_width),
                " ".repeat(task_width)
            );
            let lines = wrap_visible_text(value, value_width);
            let mut row = String::new();

            for (idx, line) in lines.iter().enumerate() {
                let prefix = if idx == 0 {
                    &prefix
                } else {
                    &continuation_prefix
                };
                let _ = writeln!(row, "{prefix}{}", display_value(line, true));
            }

            row
        }
    }
}

fn value_column_width(
    label_width: usize,
    task_width: usize,
    terminal_width: Option<usize>,
) -> Option<usize> {
    let prefix_width = ROW_INDENT.width() + label_width + task_width + 1;
    terminal_width.and_then(|width| width.checked_sub(prefix_width).filter(|width| *width > 0))
}

fn wrap_visible_text(value: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![value.to_string()];
    }

    let mut lines = Vec::new();
    let mut line = String::new();
    let mut line_width = 0usize;
    let mut pending_space = false;

    for segment in value.split_whitespace() {
        let segment_width = segment.width();
        let space_width = usize::from(pending_space && line_width > 0);

        if line_width > 0 && line_width + space_width + segment_width <= width {
            if space_width == 1 {
                line.push(' ');
                line_width += 1;
            }
            line.push_str(segment);
            line_width += segment_width;
            pending_space = true;
            continue;
        }

        if line_width == 0 && segment_width <= width {
            line.push_str(segment);
            line_width = segment_width;
            pending_space = true;
            continue;
        }

        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            line_width = 0;
        }

        let wrapped = wrap_long_segment(segment, width);
        let last_idx = wrapped.len().saturating_sub(1);
        for (idx, part) in wrapped.into_iter().enumerate() {
            if idx == last_idx {
                line_width = part.width();
                line = part;
            } else {
                lines.push(part);
            }
        }
        pending_space = true;
    }

    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }

    lines
}

fn wrap_long_segment(value: &str, width: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;

    for ch in value.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        let would_overflow = current_width > 0 && current_width + ch_width > width;
        if would_overflow {
            parts.push(std::mem::take(&mut current));
            current_width = 0;
        }

        current.push(ch);
        current_width += ch_width;

        if current_width >= width && width > 0 {
            parts.push(std::mem::take(&mut current));
            current_width = 0;
        }
    }

    if !current.is_empty() || parts.is_empty() {
        parts.push(current);
    }

    parts
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
        TaskSource::PyprojectScripts,
    ];
    for source in sources {
        let source_tasks = tasks_for_source(tasks, source);
        if source_tasks.is_empty() {
            continue;
        }
        let names: Vec<String> = fold_aliases(&source_tasks)
            .iter()
            .map(|(task, alias_names)| name_with_aliases(&task.name, alias_names))
            .collect();
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

/// Fold rename-aliases into their canonical sibling: a task whose
/// `alias_of` names another task in the same group gets no row of its
/// own; instead its name is attached to that target. Returns each
/// surfaced task with its sorted alias names. Preserves input order.
fn fold_aliases<'a>(source_tasks: &[&'a Task]) -> Vec<(&'a Task, Vec<&'a str>)> {
    use std::collections::{HashMap, HashSet};

    let names: HashSet<&str> = source_tasks.iter().map(|t| t.name.as_str()).collect();
    let mut aliases: HashMap<&str, Vec<&'a str>> = HashMap::new();
    let mut folded: HashSet<&str> = HashSet::new();
    for task in source_tasks {
        if let Some(target) = task.alias_of.as_deref()
            && target != task.name
            && names.contains(target)
        {
            aliases.entry(target).or_default().push(task.name.as_str());
            folded.insert(task.name.as_str());
        }
    }

    source_tasks
        .iter()
        .filter(|task| !folded.contains(task.name.as_str()))
        .map(|task| {
            let mut names = aliases.remove(task.name.as_str()).unwrap_or_default();
            names.sort_unstable();
            (*task, names)
        })
        .collect()
}

/// Canonical task name with any folded aliases appended as `name (a, b)`.
fn name_with_aliases(name: &str, aliases: &[&str]) -> String {
    if aliases.is_empty() {
        name.to_string()
    } else {
        format!("{name} ({})", aliases.join(", "))
    }
}

fn compact_source_label(source: TaskSource, stdout_is_terminal: bool) -> String {
    let label = pad_visible(
        source.label(),
        padded_column_width(source.label(), SOURCE_COL_WIDTH),
    );
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

fn padded_column_width(value: &str, min_width: usize) -> usize {
    value.width().max(min_width)
}

fn pad_visible(value: &str, width: usize) -> String {
    let padding = width.saturating_sub(value.width());
    format!("{value}{}", " ".repeat(padding))
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
    let width = padded_column_width(&display, SOURCE_COL_WIDTH);
    let padding = width.saturating_sub(display.width());
    let plain_label = pad_visible(&display, width);
    let label = plain_label.bold().to_string();

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
        TaskSource::PyprojectScripts => {
            tool::python::find_pyproject_upwards(root).filter(|path| path.is_file())
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
    use std::path::{Path, PathBuf};

    use super::{
        RenderMode, expected_source_labels, file_uri, render_rich_row, render_tasks_grouped,
        render_tasks_grouped_rich, select_render_mode_for, source_label, source_path,
    };
    use crate::resolver::ResolutionOverrides;
    use crate::schema::CURRENT_VERSION;
    use crate::tool::test_support::TempDir;
    use crate::types::{ProjectContext, Task, TaskSource};

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
    fn invalid_source_error_mentions_pyproject() {
        let ctx = ProjectContext {
            root: PathBuf::from("."),
            package_managers: Vec::new(),
            task_runners: Vec::new(),
            tasks: Vec::new(),
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        };

        let err = super::list(
            &ctx,
            &ResolutionOverrides::default(),
            false,
            false,
            Some("wat"),
            CURRENT_VERSION,
        )
        .expect_err("invalid source should error");

        let message = format!("{err:#}");
        assert!(message.contains("pyproject.toml"));
        assert!(expected_source_labels().contains("pyproject.toml"));
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
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: None,
        }
    }

    #[test]
    fn fold_groups_rename_alias_under_canonical_sibling() {
        let mut tasks = [
            task("build", TaskSource::CargoAliases),
            task("b", TaskSource::CargoAliases),
            task("lint", TaskSource::CargoAliases),
        ];
        tasks[1].alias_of = Some("build".into()); // b → build (sibling) folds
        tasks[2].alias_of = Some("clippy --all".into()); // not a sibling → standalone
        let refs: Vec<&Task> = tasks.iter().collect();

        let rendered = render_tasks_grouped(&refs, Path::new("."), RenderMode::Compact, false);

        assert_eq!(rendered, "  cargo           build (b), lint\n");
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

        let mode = select_render_mode_for(&refs, true, Some(10), 0);

        assert_eq!(mode, RenderMode::Compact);
    }

    #[test]
    fn reserved_rows_push_a_borderline_list_into_compact() {
        // 20 tasks fits a 24-row terminal in the list path (20 + 2 = 22
        // ≤ 24 → Rich). The same list under the info view, where a
        // 5-row banner is reserved, no longer fits (20 + 2 + 5 = 27 >
        // 24 → Compact).
        let tasks: Vec<Task> = (0..20)
            .map(|idx| task(&format!("task-{idx}"), TaskSource::Justfile))
            .collect();
        let refs: Vec<&Task> = tasks.iter().collect();

        assert_eq!(
            select_render_mode_for(&refs, true, Some(24), 0),
            RenderMode::Rich,
        );
        assert_eq!(
            select_render_mode_for(&refs, true, Some(24), 5),
            RenderMode::Compact,
        );
    }

    #[test]
    fn auto_mode_picks_rich_when_terminal_height_fits() {
        let tasks: Vec<Task> = (0..30)
            .map(|idx| task(&format!("task-{idx}"), TaskSource::Justfile))
            .collect();
        let refs: Vec<&Task> = tasks.iter().collect();

        let mode = select_render_mode_for(&refs, true, Some(200), 0);

        assert_eq!(mode, RenderMode::Rich);
    }

    #[test]
    fn auto_mode_defaults_rich_on_non_tty() {
        let tasks: Vec<Task> = (0..30)
            .map(|idx| task(&format!("task-{idx}"), TaskSource::Justfile))
            .collect();
        let refs: Vec<&Task> = tasks.iter().collect();

        let mode = select_render_mode_for(&refs, false, Some(10), 0);

        assert_eq!(mode, RenderMode::Rich);
    }

    #[test]
    fn rich_mode_renders_alias_target_in_value_column() {
        let tasks = [Task {
            name: "b".into(),
            source: TaskSource::Justfile,
            run_target: None,
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
    fn rich_tty_wraps_long_values_with_hanging_indent() {
        let tasks = [Task {
            name: "lint".into(),
            source: TaskSource::BaconToml,
            run_target: None,
            description: None,
            alias_of: Some(
                "cargo clippy --all-targets --all-features --color=always -- -D warnings".into(),
            ),
            passthrough_to: None,
        }];
        let refs: Vec<&Task> = tasks.iter().collect();

        let rendered = render_tasks_grouped_rich(&refs, Path::new("."), true, Some(68));
        let lines: Vec<&str> = rendered.lines().collect();

        assert_eq!(lines.len(), 3, "expected wrap, got: {rendered:?}");
        assert!(lines[0].contains("bacon"));
        assert!(lines[0].contains("lint"));
        assert!(lines[1].starts_with("                                      "));
        assert!(lines[2].starts_with("                                      "));
        assert!(!lines[1].starts_with("bacon"));
    }

    #[test]
    fn rich_tty_wrap_counts_unicode_display_width() {
        let rendered = render_rich_row(
            "bacon           ",
            16,
            "lint",
            Some("界界界界界 cargo"),
            true,
            Some(51),
        );
        let lines: Vec<&str> = rendered.lines().collect();

        assert_eq!(
            lines.len(),
            2,
            "expected Unicode-aware wrap, got: {rendered:?}"
        );
        assert!(lines[0].contains("界界界界界"));
        assert!(lines[1].contains("cargo"));
    }

    #[test]
    fn rich_non_tty_does_not_wrap_even_with_width() {
        let rendered = render_rich_row(
            "bacon           ",
            16,
            "lint",
            Some("cargo clippy --all-targets --all-features --color=always -- -D warnings"),
            false,
            Some(32),
        );

        assert_eq!(rendered.lines().count(), 1);
    }

    #[test]
    fn rich_tty_falls_back_to_single_line_when_terminal_too_narrow() {
        let rendered = render_rich_row(
            "bacon           ",
            16,
            "lint",
            Some("cargo clippy"),
            true,
            Some(20),
        );

        assert_eq!(rendered.lines().count(), 1);
    }

    #[test]
    fn rich_tty_keeps_osc8_label_intact_when_wrapping() {
        let dir = TempDir::new("list-rich-wrap-osc8");
        fs::write(
            dir.path().join("bacon.toml"),
            "[jobs.lint]\ncommand = ['cargo']\n",
        )
        .expect("bacon.toml should be written");
        let tasks = [Task {
            name: "lint".into(),
            source: TaskSource::BaconToml,
            run_target: None,
            description: None,
            alias_of: Some(
                "cargo clippy --all-targets --all-features --color=always -- -D warnings".into(),
            ),
            passthrough_to: None,
        }];
        let refs: Vec<&Task> = tasks.iter().collect();

        let rendered = render_tasks_grouped_rich(&refs, dir.path(), true, Some(68));

        assert_eq!(rendered.matches("\u{1b}]8;;file://").count(), 1);
        assert_eq!(rendered.matches("\u{1b}]8;;\u{1b}\\").count(), 1);
    }
}
