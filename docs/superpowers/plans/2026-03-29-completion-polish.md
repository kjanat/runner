# Completion & Task Polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Polish the dynamic shell completion system — strip redundant labels, add descriptions everywhere data exists, add source-qualified disambiguation (`justfile:test`), and backfill test coverage.

**Architecture:** Seven independent work streams that each touch 1-3 files. Tasks 1-5 can be done in any order. Task 6 (routing) must precede Task 7 (completions for qualified names). No new crates; all changes are in the `runner` crate.

**Tech Stack:** Rust (clap 4.6, clap_complete 4.6 `unstable-dynamic`), zsh completion system, serde for JSON/YAML parsing.

**Performance note:** `detect()` shells out to `just --dump` and `task --list-all --json` on every TAB press. This is inherent to dynamic completions — data must be fresh. No optimization is included here; add caching only if measured latency warrants it.

---

### Task 1: Strip redundant source prefix in grouped zsh help

The grouped zsh adapter groups by tag (e.g. `-- justfile --`), but help text still says `justfile: Format code`. Strip the tag prefix from help in `write_complete` so zsh shows just `Format code`. Other shells (fish, powershell) still see the full `justfile: Format code` since they use the default adapters.

**Files:**

- Modify: `src/complete/mod.rs:110-130` (inside `write_complete`)

- [ ] **Step 1: Write failing test**

Add to the bottom of `src/complete/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::{escape_help, escape_value, strip_tag_prefix};

    #[test]
    fn strip_tag_prefix_removes_matching_source() {
        assert_eq!(
            strip_tag_prefix("justfile: Format code", "justfile"),
            "Format code"
        );
    }

    #[test]
    fn strip_tag_prefix_leaves_non_matching_help_unchanged() {
        assert_eq!(strip_tag_prefix("Run a task", "Commands"), "Run a task");
    }

    #[test]
    fn strip_tag_prefix_returns_none_for_bare_source() {
        assert_eq!(strip_tag_prefix("package.json", "package.json"), "");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test complete::tests::strip_tag_prefix -v`
Expected: FAIL — `strip_tag_prefix` doesn't exist.

- [ ] **Step 3: Implement `strip_tag_prefix` and wire it into `write_complete`**

Add above the `escape_value` function in `src/complete/mod.rs`:

```rust
/// Strip a leading `"TAG: "` or `"TAG"` prefix from help text when it
/// matches the completion group tag (avoids redundancy in grouped output).
fn strip_tag_prefix<'a>(help: &'a str, tag: &str) -> &'a str {
    help.strip_prefix(tag)
        .map(|rest| rest.strip_prefix(": ").unwrap_or(rest))
        .unwrap_or(help)
        .trim()
}
```

Then, inside `write_complete`, change the help-writing block from:

```rust
if let Some(help) = candidate.get_help() {
    write!(
        buf,
        ":{}",
        escape_help(help.to_string().lines().next().unwrap_or_default()),
    )?;
}
```

to:

```rust
if let Some(help) = candidate.get_help() {
    let raw = help.to_string();
    let line = raw.lines().next().unwrap_or_default();
    let stripped = strip_tag_prefix(line, &tag);
    if !stripped.is_empty() {
        write!(buf, ":{}", escape_help(stripped))?;
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test complete::tests -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/complete/mod.rs
git commit -m "fix(complete): strip redundant source prefix in grouped zsh help"
```

---

### Task 2: Test coverage for completion utilities

Backfill tests for `detect_shell`, `escape_value`, `escape_help`, and the grouped zsh `write_complete` output format.

**Files:**

- Modify: `src/cmd/completions.rs` (add `detect_shell` tests)
- Modify: `src/complete/mod.rs` (add escape + format tests)

- [ ] **Step 1: Add `detect_shell` tests**

Add to the existing `tests` module in `src/cmd/completions.rs`:

```rust
use super::detect_shell;
use clap_complete::aot::Shell;

#[test]
fn detect_shell_parses_zsh_path() {
    std::env::set_var("SHELL", "/usr/bin/zsh");
    assert_eq!(detect_shell(), Some(Shell::Zsh));
}

#[test]
fn detect_shell_parses_fish_path() {
    std::env::set_var("SHELL", "/usr/local/bin/fish");
    assert_eq!(detect_shell(), Some(Shell::Fish));
}

#[test]
fn detect_shell_returns_none_for_unknown() {
    std::env::set_var("SHELL", "/usr/bin/ksh");
    assert_eq!(detect_shell(), None);
}
```

Note: `std::env::set_var` is unsafe in Rust 2024 edition. These tests must run with `-- --test-threads=1` or use a helper that wraps the unsafe call. Since the project has `unsafe_code = "deny"`, an alternative approach is needed.

Instead, test the inner logic by extracting a `shell_from_path` function that takes a `&Path`:

```rust
fn shell_from_path(path: &Path) -> Option<Shell> {
    let stem = path.file_stem()?.to_string_lossy();
    match stem.as_ref() {
        "bash" => Some(Shell::Bash),
        "zsh" => Some(Shell::Zsh),
        "fish" => Some(Shell::Fish),
        "elvish" => Some(Shell::Elvish),
        "pwsh" | "powershell" => Some(Shell::PowerShell),
        _ => None,
    }
}

fn detect_shell() -> Option<Shell> {
    shell_from_path(Path::new(&std::env::var_os("SHELL")?))
}
```

Then test `shell_from_path` directly:

```rust
use super::shell_from_path;
use clap_complete::aot::Shell;
use std::path::Path;

#[test]
fn shell_from_path_parses_zsh() {
    assert_eq!(shell_from_path(Path::new("/usr/bin/zsh")), Some(Shell::Zsh));
}

#[test]
fn shell_from_path_parses_fish() {
    assert_eq!(
        shell_from_path(Path::new("/usr/local/bin/fish")),
        Some(Shell::Fish)
    );
}

#[test]
fn shell_from_path_returns_none_for_unknown() {
    assert_eq!(shell_from_path(Path::new("/usr/bin/ksh")), None);
}

#[test]
fn shell_from_path_handles_pwsh() {
    assert_eq!(
        shell_from_path(Path::new("/usr/bin/pwsh")),
        Some(Shell::PowerShell)
    );
}
```

- [ ] **Step 2: Add escape function tests**

Add to the `tests` module in `src/complete/mod.rs`:

```rust
#[test]
fn escape_value_escapes_colons_and_backslashes() {
    assert_eq!(escape_value("helix:sync"), "helix\\:sync");
    assert_eq!(escape_value("path\\thing"), "path\\\\thing");
}

#[test]
fn escape_help_escapes_backslashes_only() {
    assert_eq!(
        escape_help("justfile: format \\ lint"),
        "justfile: format \\\\ lint"
    );
    assert_eq!(escape_help("no:escaping:here"), "no:escaping:here");
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test complete::tests -v && cargo test completions::tests -v`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/cmd/completions.rs src/complete/mod.rs
git commit -m "test(complete): cover detect_shell, escape fns, grouped format"
```

---

### Task 3: go-task YAML fallback `desc:` extraction

When the `task` CLI isn't available, `extract_tasks_from_source` falls back to YAML parsing but discards `desc:` fields. Capture them.

**Files:**

- Modify: `src/tool/go_task.rs:80-128`

- [ ] **Step 1: Write failing test**

Add to the `tests` module in `src/tool/go_task.rs`:

```rust
#[test]
fn extract_tasks_from_source_captures_desc() {
    let dir = TempDir::new("go-task-desc");

    fs::write(
            dir.path().join("Taskfile.yml"),
            "version: '3'\ntasks:\n  build:\n    desc: Build the project\n    cmds:\n      - cargo build\n  lint:\n    cmds:\n      - cargo clippy\n",
        )
        .expect("Taskfile.yml should be written");

    let tasks = super::extract_tasks_from_source(dir.path()).expect("Taskfile tasks should parse");

    assert_eq!(
        tasks,
        [
            ("build".to_string(), Some("Build the project".to_string())),
            ("lint".to_string(), None),
        ]
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test go_task::tests::extract_tasks_from_source_captures_desc -v`
Expected: FAIL — `build` returns `(build, None)` instead of `(build, Some("Build the project"))`.

- [ ] **Step 3: Implement desc extraction in YAML fallback**

Replace `extract_tasks_from_source` in `src/tool/go_task.rs`. The key change: after finding a task name line, scan subsequent indented lines for `desc:`.

```rust
fn extract_tasks_from_source(dir: &Path) -> anyhow::Result<Vec<(String, Option<String>)>> {
    let Some(path) = files::find_first(dir, FILENAMES) else {
        return Ok(vec![]);
    };
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let lines: Vec<&str> = content.lines().collect();
    let mut tasks: Vec<(String, Option<String>)> = Vec::new();
    let mut in_tasks = false;
    let mut task_indent: Option<String> = None;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.trim() == "tasks:" {
            in_tasks = true;
            task_indent = None;
            i += 1;
            continue;
        }
        if in_tasks {
            if !line.starts_with(' ') && !line.starts_with('\t') && !line.trim().is_empty() {
                break;
            }
            let indent: String = line
                .chars()
                .take_while(|ch| *ch == ' ' || *ch == '\t')
                .collect();
            let stripped = task_indent
                .as_deref()
                .and_then(|expected_indent| line.strip_prefix(expected_indent))
                .or_else(|| (!indent.is_empty()).then_some(&line[indent.len()..]));
            if let Some(rest) = stripped
                && !rest.starts_with(' ')
                && !rest.starts_with('\t')
                && let Some(colon) = rest.find(':')
            {
                if task_indent.is_none() {
                    task_indent = Some(indent.clone());
                }
                let name = rest[..colon].trim();
                if !name.is_empty()
                    && !name.starts_with('#')
                    && name
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
                {
                    // Scan following lines for desc: at deeper indent
                    let desc = scan_desc(&lines, i + 1, &indent);
                    tasks.push((name.to_string(), desc));
                }
            }
        }
        i += 1;
    }
    Ok(tasks)
}

/// Scan lines after a task definition for a `desc:` field at deeper indentation.
fn scan_desc(lines: &[&str], start: usize, task_indent: &str) -> Option<String> {
    for line in &lines[start..] {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Stop if we've exited the task's indentation level
        if !line.starts_with(task_indent)
            || !line[task_indent.len()..].starts_with(|c: char| c == ' ' || c == '\t')
        {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("desc:") {
            let val = rest.trim().trim_matches(|c| c == '\'' || c == '"');
            return (!val.is_empty()).then(|| val.to_string());
        }
        // Stop at the first non-desc key to avoid scanning into cmds/deps
        if trimmed.contains(':') && !trimmed.starts_with('#') {
            break;
        }
    }
    None
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test go_task::tests -v`
Expected: PASS (all existing + new test)

- [ ] **Step 5: Commit**

```bash
git add src/tool/go_task.rs
git commit -m "feat(go-task): extract desc from YAML fallback parser"
```

---

### Task 4: Makefile `##` comment descriptions

Parse the common self-documenting Makefile convention: `## Help text` on the line immediately preceding a target.

**Files:**

- Modify: `src/tool/make.rs` (change return type, parse comments)
- Modify: `src/detect.rs:245-246` (switch to `push_described_tasks`)

- [ ] **Step 1: Write failing test**

Add to `tests` module in `src/tool/make.rs`:

```rust
#[test]
fn extract_tasks_captures_double_hash_comments() {
    let dir = TempDir::new("make-comments");
    fs::write(
            dir.path().join("Makefile"),
            "## Build the project\nbuild:\n\t@echo build\n\n## Run the test suite\ntest:\n\t@echo test\n\nclean:\n\t@echo clean\n",
        )
        .expect("Makefile should be written");

    let tasks = extract_tasks(dir.path()).expect("Makefile targets should parse");

    assert_eq!(
        tasks,
        [
            ("build".to_string(), Some("Build the project".to_string())),
            ("test".to_string(), Some("Run the test suite".to_string())),
            ("clean".to_string(), None),
        ]
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test make::tests::extract_tasks_captures -v`
Expected: FAIL — return type mismatch (`Vec<String>` vs `Vec<(String, Option<String>)>`).

- [ ] **Step 3: Change `extract_tasks` return type and parse `##` comments**

In `src/tool/make.rs`, change the function signature and body:

```rust
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<(String, Option<String>)>> {
    let Some(path) = files::find_first(dir, FILENAMES) else {
        return Ok(vec![]);
    };
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let mut targets = Vec::new();
    let mut last_doc: Option<String> = None;
    for line in content.lines() {
        if let Some(comment) = line.strip_prefix("##") {
            last_doc = Some(comment.trim().to_string());
            continue;
        }
        if line.starts_with('\t') || line.starts_with(' ') || line.starts_with('#') {
            last_doc = None;
            continue;
        }
        let Some(colon) = line.find(':') else {
            last_doc = None;
            continue;
        };
        let after = &line[colon..];
        if after.starts_with("::=") || after.starts_with(":=") || after.starts_with(":::=") {
            last_doc = None;
            continue;
        }
        let target = line[..colon].trim();
        if SPECIAL_TARGETS.contains(&target) || is_suffix_rule(target) {
            last_doc = None;
            continue;
        }
        if !target.is_empty()
            && !target.contains(' ')
            && !target.contains('$')
            && !target.contains('%')
        {
            let doc = last_doc.take().filter(|d| !d.is_empty());
            targets.push((target.to_string(), doc));
        }
        last_doc = None;
    }
    Ok(targets)
}
```

- [ ] **Step 4: Update `detect.rs` to use `push_described_tasks` for Make**

In `src/detect.rs`, change:

```rust
push_named_tasks(ctx, TaskSource::Makefile, tool::make::extract_tasks(dir));
```

to:

```rust
push_described_tasks(ctx, TaskSource::Makefile, tool::make::extract_tasks(dir));
```

- [ ] **Step 5: Fix existing Make tests for new return type**

In `src/tool/make.rs`, update `extract_tasks_keeps_double_colon_rules`:

```rust
#[test]
fn extract_tasks_keeps_double_colon_rules() {
    let dir = TempDir::new("make-double-colon");
    fs::write(
        dir.path().join("Makefile"),
        "build::\n\t@echo first\nvalue :::= thing\n",
    )
    .expect("Makefile should be written");

    let tasks = extract_tasks(dir.path()).expect("Makefile targets should parse");
    let names: Vec<&str> = tasks.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["build"]);
}
```

And `extract_tasks_keeps_dot_prefixed_targets`:

```rust
#[test]
fn extract_tasks_keeps_dot_prefixed_targets() {
    let dir = TempDir::new("make-dot-target");
    fs::write(
        dir.path().join("Makefile"),
        ".PHONY: build\n.DELETE_ON_ERROR:\n.NOTPARALLEL:\n.c.o:\n.dev:\n\t@echo hi\n",
    )
    .expect("Makefile should be written");

    let tasks = extract_tasks(dir.path()).expect("Makefile targets should parse");
    let names: Vec<&str> = tasks.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, [".dev"]);
}
```

- [ ] **Step 6: Run all tests**

Run: `cargo test -v`
Expected: PASS (all 71+ tests)

- [ ] **Step 7: Commit**

```bash
git add src/tool/make.rs src/detect.rs
git commit -m "feat(make): extract ## doc comments as task descriptions"
```

---

### Task 5: Show descriptions in `runner list`

`runner list` currently shows task names grouped by source. Add descriptions inline when available.

**Files:**

- Modify: `src/cmd/list.rs:46-58` (`print_tasks_grouped`)

- [ ] **Step 1: Change `print_tasks_grouped` to show descriptions**

In `src/cmd/list.rs`, replace the body of the `for source in sources` loop inside `print_tasks_grouped`:

```rust
        let tasks: Vec<(&str, Option<&str>)> = ctx
            .tasks
            .iter()
            .filter(|t| t.source == source)
            .map(|t| (t.name.as_str(), t.description.as_deref()))
            .collect();
        if tasks.is_empty() {
            continue;
        }

        let label = source_label(source, &ctx.root, stdout_is_terminal);
        let has_any_desc = tasks.iter().any(|(_, d)| d.is_some());
        if has_any_desc {
            // One task per line with description
            for (name, desc) in &tasks {
                if let Some(desc) = desc {
                    println!("  {label}{name:<20} {}", desc.dimmed());
                } else {
                    println!("  {label}{name}");
                }
            }
        } else {
            // Compact: all names on one line (existing behavior)
            let names: Vec<&str> = tasks.iter().map(|(n, _)| *n).collect();
            println!("  {}{}", label, names.join(", "));
        }
```

Note: this preserves the compact one-line format when no task in a source has descriptions, and switches to one-per-line when any task has a description.

- [ ] **Step 2: Verify manually**

Run: `cargo run -- list` in a directory with a justfile that has doc comments.
Expected: justfile tasks show one-per-line with dimmed descriptions; package.json scripts remain compact.

- [ ] **Step 3: Run existing tests**

Run: `cargo test list::tests -v`
Expected: PASS (existing tests don't assert on exact output format of `print_tasks_grouped`)

- [ ] **Step 4: Commit**

```bash
git add src/cmd/list.rs
git commit -m "feat(list): show task descriptions when available"
```

---

### Task 6: `source:task` routing in `cmd/run.rs`

Allow `runner justfile:test` to bypass priority routing and target a specific source.

**Files:**

- Modify: `src/cmd/run.rs:18-49`
- Modify: `src/types.rs` (add `TaskSource::from_label`)

- [ ] **Step 1: Write failing tests**

Add to `tests` module in `src/cmd/run.rs`:

```rust
use super::parse_qualified_task;

#[test]
fn parse_qualified_task_splits_source_and_name() {
    let (source, name) = parse_qualified_task("justfile:fmt");
    assert_eq!(source, Some(TaskSource::Justfile));
    assert_eq!(name, "fmt");
}

#[test]
fn parse_qualified_task_returns_bare_name() {
    let (source, name) = parse_qualified_task("build");
    assert_eq!(source, None);
    assert_eq!(name, "build");
}

#[test]
fn parse_qualified_task_handles_unknown_source() {
    let (source, name) = parse_qualified_task("unknown:build");
    assert_eq!(source, None);
    assert_eq!(name, "unknown:build");
}

#[test]
fn parse_qualified_task_with_colons_in_task_name() {
    // "package.json" is a known source, "helix:sync" is the task name
    let (source, name) = parse_qualified_task("package.json:helix:sync");
    assert_eq!(source, Some(TaskSource::PackageJson));
    assert_eq!(name, "helix:sync");
}

#[test]
fn parse_qualified_task_preserves_colons_in_bare_name() {
    // "helix" is not a known source, whole string is the bare name
    let (source, name) = parse_qualified_task("helix:sync");
    assert_eq!(source, None);
    assert_eq!(name, "helix:sync");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test run::tests::parse_qualified -v`
Expected: FAIL — `parse_qualified_task` doesn't exist.

- [ ] **Step 3: Add `TaskSource::from_label` to `types.rs`**

In `src/types.rs`, add to the `impl TaskSource` block:

```rust
/// Parse a source label back to a [`TaskSource`].
pub(crate) fn from_label(label: &str) -> Option<Self> {
    match label {
        "package.json" => Some(Self::PackageJson),
        "Makefile" => Some(Self::Makefile),
        "justfile" => Some(Self::Justfile),
        "Taskfile" => Some(Self::Taskfile),
        "turbo.json" => Some(Self::TurboJson),
        "deno.json" => Some(Self::DenoJson),
        _ => None,
    }
}
```

- [ ] **Step 4: Add `parse_qualified_task` to `cmd/run.rs`**

Add above the `run` function:

```rust
/// Parse `"source:task"` syntax. Returns `(Some(source), task_name)` if the
/// prefix is a known source label, or `(None, original)` for bare names.
fn parse_qualified_task(input: &str) -> (Option<TaskSource>, &str) {
    if let Some(colon) = input.find(':') {
        let prefix = &input[..colon];
        if let Some(source) = TaskSource::from_label(prefix) {
            return (Some(source), &input[colon + 1..]);
        }
    }
    (None, input)
}
```

- [ ] **Step 5: Wire into `run()`**

Replace the top of the `run` function body (lines 19-37) with:

```rust
    super::print_warnings(ctx);

    let (qualifier, task_name) = parse_qualified_task(task);

    let found: Vec<_> = ctx
        .tasks
        .iter()
        .filter(|t| t.name == task_name)
        .collect();

    if found.is_empty() {
        if let Some(code) = run_bun_test_fallback(ctx, task_name, args)? {
            return Ok(code);
        }

        bail!("task {task:?} not found. Run `runner list` to see available tasks.");
    }

    let entry = if let Some(source) = qualifier {
        // Explicit source: find exact match
        found
            .iter()
            .find(|t| t.source == source)
            .copied()
            .ok_or_else(|| anyhow::anyhow!(
                "task {task_name:?} not found in {}", source.label()
            ))?
    } else {
        // Priority: turbo > package.json > first match
        found
            .iter()
            .find(|t| t.source == TaskSource::TurboJson)
            .or_else(|| found.iter().find(|t| t.source == TaskSource::PackageJson))
            .or_else(|| found.first())
            .unwrap()
    };
```

Also update the `eprintln!` and `build_run_command` calls to use `task_name` instead of `task`:

```rust
    eprintln!(
        "{} {} {} {}",
        "→".dimmed(),
        entry.source.label().dimmed(),
        task_name.bold(),
        args.join(" ").dimmed(),
    );

    let mut cmd = build_run_command(ctx, entry.source, task_name, args)?;
```

- [ ] **Step 6: Run tests**

Run: `cargo test run::tests -v`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add src/types.rs src/cmd/run.rs
git commit -m "feat(run): support source:task qualified syntax for disambiguation"
```

---

### Task 7: Qualified completion candidates for duplicate tasks

When a task name exists in multiple sources (e.g. `test` in both package.json and Makefile), emit extra `source:task` candidates alongside the bare name.

**Files:**

- Modify: `src/cli.rs:11-30` (`task_candidates`)

- [ ] **Step 1: Write failing test**

Add a test module to `src/cli.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::task_candidates_from;
    use crate::types::{Task, TaskSource};

    #[test]
    fn qualified_candidates_emitted_for_duplicates() {
        let tasks = vec![
            Task {
                name: "test".into(),
                source: TaskSource::PackageJson,
                description: None,
            },
            Task {
                name: "test".into(),
                source: TaskSource::Makefile,
                description: None,
            },
            Task {
                name: "build".into(),
                source: TaskSource::PackageJson,
                description: None,
            },
        ];
        let candidates = task_candidates_from(tasks);
        let values: Vec<String> = candidates
            .iter()
            .map(|c| c.get_value().to_string_lossy().into_owned())
            .collect();
        // "test" appears as bare + both qualified forms; "build" is bare only
        assert!(values.contains(&"test".to_string()));
        assert!(values.contains(&"package.json:test".to_string()));
        assert!(values.contains(&"Makefile:test".to_string()));
        assert!(values.contains(&"build".to_string()));
        assert!(!values.contains(&"package.json:build".to_string()));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test cli::tests::qualified -v`
Expected: FAIL — `task_candidates_from` doesn't exist.

- [ ] **Step 3: Extract `task_candidates_from` and add qualified candidates**

Refactor `task_candidates` in `src/cli.rs` to delegate to a testable function:

```rust
fn task_candidates() -> Vec<CompletionCandidate> {
    let Ok(dir) = std::env::current_dir() else {
        return vec![];
    };
    let ctx = crate::detect::detect(&dir);
    task_candidates_from(ctx.tasks)
}

fn task_candidates_from(tasks: Vec<crate::types::Task>) -> Vec<CompletionCandidate> {
    use std::collections::HashMap;

    // Count occurrences of each task name
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for task in &tasks {
        *counts.entry(&task.name).or_default() += 1;
    }

    let mut candidates = Vec::new();
    for task in &tasks {
        let help = match &task.description {
            Some(desc) => format!("{}: {desc}", task.source.label()),
            None => task.source.label().to_string(),
        };
        let tag = task.source.label();

        // Always emit the bare task name
        candidates.push(
            CompletionCandidate::new(&task.name)
                .help(Some(help.clone().into()))
                .tag(Some(tag.into()))
                .display_order(Some(usize::from(task.source.display_order()))),
        );

        // For duplicate names, also emit "source:name" qualified form
        if counts.get(task.name.as_str()).copied().unwrap_or(0) > 1 {
            let qualified = format!("{}:{}", task.source.label(), task.name);
            candidates.push(
                CompletionCandidate::new(qualified)
                    .help(Some(help.into()))
                    .tag(Some(tag.into()))
                    .display_order(Some(usize::from(task.source.display_order()))),
            );
        }
    }
    candidates
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test cli::tests -v`
Expected: PASS

- [ ] **Step 5: Run full suite**

Run: `cargo test -v`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/cli.rs
git commit -m "feat(complete): emit source:task candidates for duplicate names"
```

---

## Unresolved Questions

- **`runner list --raw` with descriptions?**: `--raw` currently prints bare names. Should there be a `--raw --desc` or a separate `--completion` flag for scripts that want `name\tdescription` output?
- **Compact vs expanded list layout threshold**: Task 5 switches to one-per-line when *any* task in a source has a description. Alternative: always use one-per-line when descriptions exist anywhere in the project.
