//! just, a handy command runner using `justfile`.

use std::collections::{HashMap, hash_map::Entry};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::Deserialize;

use super::files;

/// Configuration filenames in lookup order.
pub const FILENAMES: &[&str] = &["justfile", "Justfile", ".justfile"];

/// A task extracted from a justfile: either a public recipe or an alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractedTask {
    /// A public recipe.
    Recipe {
        /// Recipe name.
        name: String,
        /// Recipe documentation.
        doc: Option<String>,
    },
    /// A recipe alias.
    Alias {
        /// Alias name.
        name: String,
        /// Target recipe.
        target: String,
    },
}

impl ExtractedTask {
    /// The recipe or alias name.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Recipe { name, .. } | Self::Alias { name, .. } => name,
        }
    }
}

/// Detected via case-insensitive `justfile`, or hidden `.justfile`.
#[must_use]
pub fn detect(dir: &Path) -> bool {
    find_file(dir).is_some()
}

/// Parse public recipes and aliases from a justfile.
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<ExtractedTask>> {
    let Some(path) = find_file(dir) else {
        return Ok(vec![]);
    };

    extract_tasks_with_just(&path).map_or_else(|| extract_tasks_from_source(&path), Ok)
}

fn extract_tasks_with_just(path: &Path) -> Option<Vec<ExtractedTask>> {
    #[derive(Deserialize)]
    struct Dump {
        recipes: HashMap<String, Recipe>,
        #[serde(default)]
        aliases: HashMap<String, Alias>,
        #[serde(default)]
        modules: HashMap<String, Module>,
    }

    #[derive(Deserialize)]
    struct Module {
        #[serde(default)]
        recipes: HashMap<String, Recipe>,
        #[serde(default)]
        modules: HashMap<String, Self>,
    }

    #[derive(Deserialize)]
    struct Recipe {
        private: bool,
        doc: Option<String>,
    }

    #[derive(Deserialize)]
    struct Alias {
        #[serde(default)]
        private: bool,
        target: String,
    }

    fn module_recipes_named<'a>(
        modules: &'a HashMap<String, Module>,
        name: &str,
    ) -> Vec<&'a Recipe> {
        let mut found = Vec::new();
        let mut stack: Vec<&HashMap<String, Module>> = vec![modules];
        while let Some(current) = stack.pop() {
            for module in current.values() {
                if let Some(recipe) = module.recipes.get(name) {
                    found.push(recipe);
                }
                stack.push(&module.modules);
            }
        }
        found
    }

    let output = super::command("just")
        .arg("--justfile")
        .arg(path)
        .arg("--dump-format")
        .arg("json")
        .arg("--dump")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let dump = serde_json::from_slice::<Dump>(&output.stdout).ok()?;
    let mut tasks: Vec<ExtractedTask> = dump
        .recipes
        .iter()
        .filter(|(_, recipe)| !recipe.private)
        .map(|(name, recipe)| ExtractedTask::Recipe {
            name: name.clone(),
            doc: recipe.doc.clone(),
        })
        .collect();
    for (name, alias) in &dump.aliases {
        if alias.private
            || name.starts_with('_')
            || alias_target_leaf(&alias.target).starts_with('_')
        {
            continue;
        }
        // `just --dump` normalizes submodule alias targets to the leaf name
        // (e.g. `alias b := foo::bar` becomes `target: "bar"`), so we can't
        // tell from `target` alone which recipe an alias resolves to. Gather
        // every candidate (top-level + any submodule recipe sharing the leaf)
        // and hide the alias only when we can prove all candidates are
        // private. If any candidate is public, or nothing matches (dangling
        // target), surface the alias.
        let top_level = dump.recipes.get(&alias.target);
        let module_matches = module_recipes_named(&dump.modules, &alias.target);
        let has_candidate = top_level.is_some() || !module_matches.is_empty();
        let any_public =
            top_level.is_some_and(|r| !r.private) || module_matches.iter().any(|r| !r.private);
        if has_candidate && !any_public {
            continue;
        }
        tasks.push(ExtractedTask::Alias {
            name: name.clone(),
            target: alias.target.clone(),
        });
    }
    tasks.sort_unstable_by(|a, b| a.name().cmp(b.name()));
    Some(tasks)
}

/// Resolve the active justfile path in the current directory.
///
/// Honors standard filenames and falls back to ASCII case-insensitive
/// `justfile` / `.justfile` matches (e.g. `JUSTFILE`, `.JUSTFILE`).
pub fn find_file(dir: &Path) -> Option<PathBuf> {
    if let Some(path) = files::find_first(dir, FILENAMES).filter(|path| path.is_file()) {
        return Some(path);
    }

    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_justfile_name)
        })
        .collect();

    paths.sort_unstable();
    paths.into_iter().next()
}

const fn is_justfile_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("justfile") || name.eq_ignore_ascii_case(".justfile")
}

struct ParsedRecipe {
    doc: Option<String>,
    private: bool,
}

struct ParsedAlias {
    name: String,
    target: String,
    private: bool,
}

fn is_top_level_directive(trimmed: &str) -> bool {
    ["set ", "import ", "include ", "mod ", "export "]
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

fn try_upsert_recipe(
    recipes: &mut HashMap<String, ParsedRecipe>,
    trimmed: &str,
    saw_private_attr: bool,
    last_doc: Option<String>,
) {
    let recipe = trimmed.strip_prefix('@').unwrap_or(trimmed);
    let Some(colon) = recipe.find(':') else {
        return;
    };
    // `foo := "bar"` is a variable binding, not a recipe header.
    if recipe[colon..].starts_with(":=") {
        return;
    }
    let name = recipe[..colon].split_whitespace().next().unwrap_or("");
    if !is_valid_ident(name) {
        return;
    }
    let private = saw_private_attr || name.starts_with('_');
    let doc = last_doc.filter(|d| !d.is_empty());
    match recipes.entry(name.to_string()) {
        Entry::Vacant(slot) => {
            slot.insert(ParsedRecipe { doc, private });
        }
        Entry::Occupied(mut slot) => {
            // A later `[private]` annotation on the same recipe name must
            // promote the aggregate to private; losing the flag would surface
            // a recipe the author hid.
            let existing = slot.get_mut();
            existing.private |= private;
            if existing.doc.is_none() {
                existing.doc = doc;
            }
        }
    }
}

fn extract_tasks_from_source(path: &Path) -> anyhow::Result<Vec<ExtractedTask>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let mut recipes: HashMap<String, ParsedRecipe> = HashMap::new();
    let mut aliases: Vec<ParsedAlias> = Vec::new();
    let mut saw_private_attr = false;
    let mut last_doc: Option<String> = None;
    let mut doc_override: Option<DocAttr> = None;
    for line in content.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            last_doc = None;
            continue;
        }
        if let Some(comment) = trimmed.strip_prefix('#') {
            last_doc = Some(comment.trim().to_string());
            continue;
        }
        if trimmed.starts_with('[') {
            let attributes = attributes(trimmed);
            saw_private_attr |= attributes.iter().any(|attr| attr.starts_with("private"));
            if let Some(doc) = attributes.iter().find_map(|attr| doc_attr(attr)) {
                doc_override = Some(doc);
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("alias ") {
            if let Some((name, target)) = parse_alias(rest) {
                let private = saw_private_attr || name.starts_with('_');
                aliases.push(ParsedAlias {
                    name,
                    target,
                    private,
                });
            }
        } else if !is_top_level_directive(trimmed) {
            let doc = match doc_override.take() {
                Some(DocAttr::Text(text)) => Some(text),
                Some(DocAttr::Hidden) => None,
                None => last_doc.take(),
            };
            try_upsert_recipe(&mut recipes, trimmed, saw_private_attr, doc);
        }
        saw_private_attr = false;
        last_doc = None;
        doc_override = None;
    }

    let mut tasks: Vec<ExtractedTask> = recipes
        .iter()
        .filter(|(_, r)| !r.private)
        .map(|(name, r)| ExtractedTask::Recipe {
            name: name.clone(),
            doc: r.doc.clone(),
        })
        .collect();
    for alias in aliases {
        if alias.private || alias_target_leaf(&alias.target).starts_with('_') {
            continue;
        }
        match recipes.get(&alias.target) {
            Some(target) if target.private => {}
            _ => tasks.push(ExtractedTask::Alias {
                name: alias.name,
                target: alias.target,
            }),
        }
    }
    tasks.sort_unstable_by(|a, b| a.name().cmp(b.name()));
    Ok(tasks)
}

fn parse_alias(rest: &str) -> Option<(String, String)> {
    let (name_part, target_part) = rest.split_once(":=")?;
    let name = name_part.trim();
    let target = target_part.split_whitespace().next().unwrap_or("");
    if !is_valid_ident(name) {
        return None;
    }
    if !target.split("::").all(is_valid_ident) {
        return None;
    }
    Some((name.to_string(), target.to_string()))
}

fn alias_target_leaf(target: &str) -> &str {
    target.rsplit_once("::").map_or(target, |(_, leaf)| leaf)
}

fn is_valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn attributes(trimmed: &str) -> Vec<&str> {
    let Some(inner) = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    else {
        return Vec::new();
    };
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;
    for (at, c) in inner.char_indices() {
        match quote {
            Some('"') if escaped => escaped = false,
            Some('"') if c == '\\' => escaped = true,
            Some(open) if c == open => quote = None,
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == ',' => {
                parts.push(inner[start..at].trim());
                start = at + 1;
            }
            Some(_) | None => {}
        }
    }
    parts.push(inner[start..].trim());
    parts
}

#[derive(Debug, PartialEq, Eq)]
enum DocAttr {
    Text(String),
    Hidden,
}

fn doc_attr(attribute: &str) -> Option<DocAttr> {
    let rest = attribute.strip_prefix("doc")?.trim_start();
    if rest.is_empty() {
        return Some(DocAttr::Hidden);
    }
    let argument = match rest.strip_prefix(':') {
        Some(argument) => argument,
        None => rest.strip_prefix('(')?.strip_suffix(')')?,
    };
    string_literal(argument.trim()).map(DocAttr::Text)
}

fn string_literal(literal: &str) -> Option<String> {
    for fence in ["'''", "\"\"\""] {
        if let Some(body) = literal
            .strip_prefix(fence)
            .and_then(|rest| rest.strip_suffix(fence))
        {
            return Some(body.trim().to_string());
        }
    }
    if let Some(raw) = literal
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    {
        return Some(raw.to_string());
    }
    let body = literal.strip_prefix('"')?.strip_suffix('"')?;
    let mut value = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            value.push(c);
            continue;
        }
        value.push(match chars.next()? {
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            '"' => '"',
            '\\' => '\\',
            'u' => unicode_escape(&mut chars)?,
            _ => return None,
        });
    }
    Some(value)
}

fn unicode_escape(chars: &mut std::str::Chars<'_>) -> Option<char> {
    if chars.next()? != '{' {
        return None;
    }
    let mut code = 0;
    let mut digits = 0;
    loop {
        let c = chars.next()?;
        if c == '}' {
            break;
        }
        code = code * 16 + c.to_digit(16)?;
        digits += 1;
        if digits > 6 {
            return None;
        }
    }
    if digits == 0 {
        return None;
    }
    char::from_u32(code)
}

/// Tasks declared by this provider in its observed scope.
///
/// # Errors
/// Returns a warning when the source or the provider query cannot be read.
pub fn tasks(
    present: &runner_core::Present,
    tree: &runner_core::Tree,
) -> Result<runner_core::Extracted, runner_core::Warning> {
    let root = runner_core::plan::scope_dir(tree, &present.scope);
    let extracted = extract_tasks(&root)
        .map_err(|e| runner_core::Warning::about(present.provider, format!("{e:#}")))?;
    Ok(extracted
        .into_iter()
        .map(|entry| match entry {
            ExtractedTask::Recipe { name, doc } => super::task(present, name, doc),
            ExtractedTask::Alias { name, target } => {
                let mut task = super::task(present, name, None);
                task.alias_of = Some(target);
                task
            }
        })
        .collect::<Vec<_>>()
        .into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::test_support::TempDir;
    use std::fs;
    #[test]
    fn fallback_parser_skips_private_and_directive_lines() {
        let dir = TempDir::new("just-fallback");
        let path = dir.path().join("justfile");

        fs::write(
            &path,
            "set shell := [\"bash\", \"-cu\"]\ninclude \"common.just\"\n[private]\nfoo := \
             \"bar\"\n\n[private]\nsecret:\n  echo nope\n\nbuild:\n  echo build\n\n_secret:\n  \
             echo nope\n\n@quiet name=\"world\":\n  echo hi {{name}}\n",
        )
        .expect("justfile should be written");

        let tasks = extract_tasks_from_source(&path).expect("justfile source should parse");
        let names: Vec<&str> = tasks.iter().map(ExtractedTask::name).collect();
        assert_eq!(names, ["build", "quiet"]);
    }

    #[test]
    fn attributes_split_on_commas_outside_strings() {
        assert_eq!(attributes("[unix, private]"), ["unix", "private"]);
        assert_eq!(
            attributes("[private(no-cd), unix]"),
            ["private(no-cd)", "unix"]
        );
        assert_eq!(
            attributes(r#"[group("a, b"), doc('c, d'), doc("e \", f")]"#),
            [r#"group("a, b")"#, "doc('c, d')", r#"doc("e \", f")"#]
        );
    }

    #[test]
    fn doc_attr_reads_every_form_just_accepts() {
        assert_eq!(
            doc_attr("doc('a, b')"),
            Some(DocAttr::Text("a, b".to_string()))
        );
        assert_eq!(
            doc_attr("doc: 'colon'"),
            Some(DocAttr::Text("colon".to_string()))
        );
        assert_eq!(
            doc_attr(r#"doc("dq \"x\" \\t")"#),
            Some(DocAttr::Text(r#"dq "x" \t"#.to_string()))
        );
        assert_eq!(
            doc_attr("doc('''triple''')"),
            Some(DocAttr::Text("triple".to_string()))
        );
        assert_eq!(doc_attr("doc"), Some(DocAttr::Hidden));
        assert_eq!(doc_attr("docs('no')"), None);
        for (escape, text) in [
            (r"\u{1F680}", "🚀"),
            (r"\u{1f680}", "🚀"),
            (r"\u{000041}", "A"),
        ] {
            assert_eq!(
                doc_attr(&format!("doc(\"x {escape} y\")")),
                Some(DocAttr::Text(format!("x {text} y"))),
                "{escape}"
            );
        }
        for escape in [
            r"\u{}",
            r"\u{0000041}",
            r"\u{D800}",
            r"\u{110000}",
            r"\u{zz}",
            r"\u{+41}",
            r"\u41",
            r"\u{41",
        ] {
            assert_eq!(
                doc_attr(&format!("doc(\"x {escape} y\")")),
                None,
                "{escape}"
            );
        }
        assert_eq!(doc_attr("group('no')"), None);
    }

    #[test]
    fn fallback_parser_prefers_the_doc_attribute_over_the_comment() {
        let dir = TempDir::new("just-fallback-doc-attr");
        let path = dir.path().join("justfile");
        fs::write(
            &path,
            "# long comment\n# second line\n[group('g'), doc('Summary, with comma')]\nbuild:\n  \
             echo build\n\n# hidden by a bare doc\n[doc]\nquiet:\n  echo quiet\n\n# plain \
             comment\nlint:\n  echo lint\n",
        )
        .expect("justfile should be written");

        let tasks = extract_tasks_from_source(&path).expect("justfile source should parse");
        let docs: Vec<(&str, Option<&str>)> = tasks
            .iter()
            .map(|task| match task {
                ExtractedTask::Recipe { name, doc } => (name.as_str(), doc.as_deref()),
                ExtractedTask::Alias { name, .. } => (name.as_str(), None),
            })
            .collect();
        assert_eq!(
            docs,
            [
                ("build", Some("Summary, with comma")),
                ("lint", Some("plain comment")),
                ("quiet", None),
            ]
        );
    }

    #[test]
    fn fallback_parser_enforces_just_name_grammar() {
        // Just's grammar is `NAME = [a-zA-Z_][a-zA-Z0-9_-]*`. Names that
        // start with a digit or hyphen, or contain non-ASCII letters, are
        // rejected by `just` itself and must not be surfaced by the
        // fallback parser either.
        let dir = TempDir::new("just-fallback-ident-grammar");
        let path = dir.path().join("justfile");

        fs::write(
            &path,
            "1build:\n  echo nope\n\n-build:\n  echo nope\n\néclair:\n  echo nope\n\nβuild:\n  \
             echo nope\n\nbuild:\n  echo yes\n",
        )
        .expect("justfile should be written");

        let tasks = extract_tasks_from_source(&path).expect("justfile source should parse");
        let names: Vec<&str> = tasks.iter().map(ExtractedTask::name).collect();
        assert_eq!(names, ["build"]);
    }

    #[test]
    fn extract_tasks_uses_just_json_when_available() {
        if std::process::Command::new("just")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: just unavailable");
            return;
        }

        let dir = TempDir::new("just-json");
        fs::write(
            dir.path().join("justfile"),
            "build:\n  echo build\n\n_secret:\n  echo nope\n\n@quiet:\n  echo hi\n",
        )
        .expect("justfile should be written");

        let tasks = extract_tasks(dir.path()).expect("justfile tasks should parse");
        let names: Vec<&str> = tasks.iter().map(ExtractedTask::name).collect();
        assert_eq!(names, ["build", "quiet"]);
    }

    #[test]
    fn detect_supports_uppercase_justfile_name() {
        let dir = TempDir::new("just-uppercase");
        fs::write(dir.path().join("JUSTFILE"), "build:\n  echo build\n")
            .expect("JUSTFILE should be written");

        assert!(detect(dir.path()));
    }

    #[test]
    fn detect_supports_uppercase_hidden_justfile_name() {
        let dir = TempDir::new("just-hidden-uppercase");
        fs::write(dir.path().join(".JUSTFILE"), "build:\n  echo build\n")
            .expect(".JUSTFILE should be written");

        assert!(detect(dir.path()));
    }

    #[test]
    fn parse_alias_accepts_standard_forms() {
        assert_eq!(
            parse_alias("b := build"),
            Some(("b".to_string(), "build".to_string()))
        );
        assert_eq!(
            parse_alias("b:=build"),
            Some(("b".to_string(), "build".to_string()))
        );
        assert_eq!(
            parse_alias("b := build # trailing"),
            Some(("b".to_string(), "build".to_string()))
        );
        assert_eq!(parse_alias("b build"), None);
        assert_eq!(parse_alias("b := "), None);
    }

    #[test]
    fn parse_alias_accepts_submodule_target() {
        assert_eq!(
            parse_alias("b := foo::bar"),
            Some(("b".to_string(), "foo::bar".to_string()))
        );
        assert_eq!(
            parse_alias("q := a::b::c"),
            Some(("q".to_string(), "a::b::c".to_string()))
        );
        assert_eq!(parse_alias("b := foo::"), None);
        assert_eq!(parse_alias("b := ::bar"), None);
    }

    #[test]
    fn fallback_parser_emits_submodule_aliases_without_doc() {
        let dir = TempDir::new("just-alias-submodule");
        let path = dir.path().join("justfile");

        fs::write(&path, "mod foo\n\nalias b := foo::bar\n").expect("justfile should be written");

        let tasks = extract_tasks_from_source(&path).expect("justfile source should parse");
        assert_eq!(
            tasks,
            vec![ExtractedTask::Alias {
                name: "b".to_string(),
                target: "foo::bar".to_string(),
            }]
        );
    }

    #[test]
    fn fallback_parser_extracts_public_aliases() {
        let dir = TempDir::new("just-alias-public");
        let path = dir.path().join("justfile");

        fs::write(
            &path,
            "# Build the project\nbuild:\n  echo build\n\nalias b := build\n",
        )
        .expect("justfile should be written");

        let tasks = extract_tasks_from_source(&path).expect("justfile source should parse");
        assert_eq!(
            tasks,
            vec![
                ExtractedTask::Alias {
                    name: "b".to_string(),
                    target: "build".to_string(),
                },
                ExtractedTask::Recipe {
                    name: "build".to_string(),
                    doc: Some("Build the project".to_string()),
                },
            ]
        );
    }

    #[test]
    fn fallback_parser_hides_aliases_to_private_recipes() {
        let dir = TempDir::new("just-alias-private-target");
        let path = dir.path().join("justfile");

        fs::write(
            &path,
            "_secret:\n  echo nope\n\n[private]\nhush:\n  echo nope\n\nalias s := _secret\nalias \
             h := hush\n",
        )
        .expect("justfile should be written");

        let tasks = extract_tasks_from_source(&path).expect("justfile source should parse");
        let names: Vec<&str> = tasks.iter().map(ExtractedTask::name).collect();
        assert!(names.is_empty(), "expected no tasks, got {names:?}");
    }

    #[test]
    fn fallback_parser_hides_private_aliases() {
        let dir = TempDir::new("just-alias-private-alias");
        let path = dir.path().join("justfile");

        fs::write(
            &path,
            "build:\n  echo build\n\nalias _hidden := build\n[private]\nalias h := build\n",
        )
        .expect("justfile should be written");

        let tasks = extract_tasks_from_source(&path).expect("justfile source should parse");
        let names: Vec<&str> = tasks.iter().map(ExtractedTask::name).collect();
        assert_eq!(names, ["build"]);
    }

    #[test]
    fn extract_tasks_uses_just_json_when_available_with_aliases() {
        let dir = TempDir::new("just-json-aliases");
        let path = dir.path().join("justfile");
        fs::write(
            &path,
            "# Build the project\nbuild:\n  echo build\n\n_secret:\n  echo nope\n\nalias b := \
             build\nalias s := _secret\nalias _hidden := build\n",
        )
        .expect("justfile should be written");

        let Some(tasks) = extract_tasks_with_just(&path) else {
            eprintln!("skipping: just unavailable");
            return;
        };
        let names: Vec<&str> = tasks.iter().map(ExtractedTask::name).collect();
        assert_eq!(names, ["b", "build"]);
        let b = tasks
            .iter()
            .find(|t| t.name() == "b")
            .expect("alias b should surface");
        assert!(
            matches!(b, ExtractedTask::Alias { target, .. } if target == "build"),
            "expected alias b → build, got {b:?}"
        );
    }

    #[test]
    fn fallback_parser_hides_aliases_to_private_submodule_targets() {
        let dir = TempDir::new("just-alias-submodule-private");
        let path = dir.path().join("justfile");

        fs::write(
            &path,
            "mod foo\n\nbuild:\n  echo build\n\nalias s := foo::_secret\nalias b := build\n",
        )
        .expect("justfile should be written");

        let tasks = extract_tasks_from_source(&path).expect("justfile source should parse");
        let names: Vec<&str> = tasks.iter().map(ExtractedTask::name).collect();
        assert_eq!(names, ["b", "build"]);
    }

    #[test]
    fn json_alias_targeting_submodule_recipe_is_unresolved() {
        let dir = TempDir::new("just-json-alias-ambig");
        let root = dir.path();
        fs::create_dir_all(root.join("foo")).expect("foo dir");
        fs::write(
            root.join("foo/mod.just"),
            "# submodule bar\nbar:\n  echo sub\n",
        )
        .expect("module justfile should be written");
        let path = root.join("justfile");
        fs::write(
            &path,
            "mod foo\n\n# top bar\nbar:\n  echo top\n\nalias b := foo::bar\n",
        )
        .expect("justfile should be written");

        let Some(tasks) = extract_tasks_with_just(&path) else {
            eprintln!("skipping: just unavailable");
            return;
        };
        let b = tasks
            .iter()
            .find(|t| t.name() == "b")
            .expect("alias b should be surfaced");
        assert!(
            matches!(b, ExtractedTask::Alias { target, .. } if target == "bar"),
            "ambiguous submodule alias must surface as Alias with leaf target, got {b:?}"
        );
    }

    #[test]
    fn json_alias_surfaces_when_top_level_private_but_submodule_shares_leaf() {
        // `just --dump` normalizes `alias b := foo::bar` to `target: "bar"`, so
        // when a `[private]` top-level `bar` exists alongside `foo::bar`, the
        // JSON view can't prove which one the alias points to. The documented
        // trade-off (see the comment on the `ambiguous` branch) is to surface
        // the alias rather than hide it; hiding would drop a legitimate
        // public submodule alias whenever any same-leaf private top-level
        // recipe happens to exist. Locking the behavior in so future refactors
        // make this call-out explicit.
        let dir = TempDir::new("just-json-alias-private-ambig");
        let root = dir.path();
        fs::create_dir_all(root.join("foo")).expect("foo dir");
        fs::write(
            root.join("foo/mod.just"),
            "# submodule bar\nbar:\n  echo sub\n",
        )
        .expect("module justfile should be written");
        let path = root.join("justfile");
        fs::write(
            &path,
            "mod foo\n\n[private]\nbar:\n  echo top\n\nalias b := foo::bar\n",
        )
        .expect("justfile should be written");

        let Some(tasks) = extract_tasks_with_just(&path) else {
            eprintln!("skipping: just unavailable");
            return;
        };
        let b = tasks
            .iter()
            .find(|t| t.name() == "b")
            .expect("alias b should still be surfaced despite private top-level `bar`");
        assert!(
            matches!(b, ExtractedTask::Alias { target, .. } if target == "bar"),
            "ambiguous alias must surface as Alias with the normalized leaf target, got {b:?}"
        );
        assert!(
            tasks
                .iter()
                .all(|t| !matches!(t, ExtractedTask::Recipe { name, .. } if name == "bar")),
            "private top-level `bar` must still stay hidden as a recipe"
        );
    }

    #[test]
    fn json_alias_hides_private_submodule_target_without_top_level() {
        // `alias s := foo::hush` where `foo::hush` is the only candidate and
        // it is `[private]`. Since there is no same-leaf top-level recipe,
        // the privacy check is unambiguous: we know exactly which recipe the
        // alias points to, and it is private. The alias must be hidden.
        let dir = TempDir::new("just-json-alias-submodule-private");
        let root = dir.path();
        fs::create_dir_all(root.join("foo")).expect("foo dir");
        fs::write(root.join("foo/mod.just"), "[private]\nhush:\n  echo nope\n")
            .expect("module justfile should be written");
        let path = root.join("justfile");
        fs::write(
            &path,
            "mod foo\n\nbuild:\n  echo build\n\nalias s := foo::hush\nalias b := build\n",
        )
        .expect("justfile should be written");

        let Some(tasks) = extract_tasks_with_just(&path) else {
            eprintln!("skipping: just unavailable");
            return;
        };
        let names: Vec<&str> = tasks.iter().map(ExtractedTask::name).collect();
        assert_eq!(
            names,
            ["b", "build"],
            "alias `s` must be hidden when its only candidate submodule recipe is private"
        );
    }
}
