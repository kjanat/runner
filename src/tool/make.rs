//! GNU Make — build automation via `Makefile`.

use std::path::Path;
use std::process::Command;

use anyhow::Context as _;

use crate::tool::files;

const FILENAMES: &[&str] = &["Makefile", "GNUmakefile", "makefile"];

/// Detected via `Makefile`, `GNUmakefile`, or `makefile`.
pub(crate) fn detect(dir: &Path) -> bool {
    FILENAMES.iter().any(|n| dir.join(n).exists())
}

/// Parse Makefile targets.
///
/// Extracts lines matching `target:` while skipping recipe lines (tab-
/// indented), special targets (`.PHONY` etc.), variable assignments (`:=`,
/// `:::=`), and pattern rules (`%`).
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<String>> {
    let Some(path) = files::find_first(dir, FILENAMES) else {
        return Ok(vec![]);
    };
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
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
        if after.starts_with("::=") || after.starts_with(":=") || after.starts_with(":::=") {
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
    Ok(targets)
}

/// `make <task> [args...]`
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("make");
    c.arg(task).args(args);
    c
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::extract_tasks;
    use crate::tool::test_support::TempDir;

    #[test]
    fn extract_tasks_keeps_double_colon_rules() {
        let dir = TempDir::new("make-double-colon");
        fs::write(
            dir.path().join("Makefile"),
            "build::\n\t@echo first\nvalue :::= thing\n",
        )
        .expect("Makefile should be written");

        assert_eq!(
            extract_tasks(dir.path()).expect("Makefile targets should parse"),
            ["build"]
        );
    }
}
