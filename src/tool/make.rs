//! GNU Make — build automation via `Makefile`.

use std::path::{Path, PathBuf};
use std::process::Command;

const FILENAMES: &[&str] = &["Makefile", "GNUmakefile", "makefile"];

/// Detected via `Makefile`, `GNUmakefile`, or `makefile`.
pub(crate) fn detect(dir: &Path) -> bool {
    FILENAMES.iter().any(|n| dir.join(n).exists())
}

/// Parse Makefile targets.
///
/// Extracts lines matching `target:` while skipping recipe lines (tab-
/// indented), special targets (`.PHONY` etc.), variable assignments (`:=`,
/// `::=`), and pattern rules (`%`).
pub(crate) fn extract_tasks(dir: &Path) -> Vec<String> {
    let Some(content) = find_file(dir).and_then(|p| std::fs::read_to_string(p).ok()) else {
        return vec![];
    };
    let mut targets = Vec::new();
    for line in content.lines() {
        if line.starts_with('\t')
            || line.starts_with(' ')
            || line.starts_with('#')
            || line.starts_with('.')
        {
            continue;
        }
        let Some(colon) = line.find(':') else {
            continue;
        };
        let after = &line[colon..];
        if after.starts_with(":=") || after.starts_with("::") {
            continue;
        }
        let target = line[..colon].trim();
        if !target.is_empty()
            && !target.contains(' ')
            && !target.contains('$')
            && !target.contains('%')
        {
            targets.push(target.to_string());
        }
    }
    targets
}

/// `make <task> [args...]`
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("make");
    c.arg(task).args(args);
    c
}

fn find_file(dir: &Path) -> Option<PathBuf> {
    FILENAMES.iter().map(|n| dir.join(n)).find(|p| p.exists())
}
