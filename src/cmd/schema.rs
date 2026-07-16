//! `runner schema`, emit committed JSON Schemas (feature `schema`).

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use schemars::{JsonSchema, Schema};
use serde_json::{Map, Value, json};

use crate::schema::project::TaskListView;

const SCHEMA_DIR: &str = "schemas";

struct SchemaDocument {
    filename: &'static str,
    value: Value,
}

/// Write the config schema to stdout/a file, or every committed schema to a directory.
/// A trailing newline is appended so committed `schemas/*.json` ends cleanly.
pub(crate) fn write_schema(all: bool, output: Option<&Path>) -> Result<()> {
    if all {
        let dir = output.unwrap_or_else(|| Path::new(SCHEMA_DIR));
        write_all_schemas(dir)
    } else {
        write_json(output, &config_schema()?)
    }
}

/// The `runner.toml` config schema, tagged with its canonical `$id` so the
/// committed file self-identifies (matching the `#:schema` directive the
/// scaffold writes).
pub(crate) fn config_schema() -> Result<Value> {
    let mut schema = schema_value(schemars::schema_for!(crate::config::RunnerConfig))?;
    set_object_field(
        &mut schema,
        "$id",
        json!(crate::schema::config_schema_url()),
    );
    patch_tasks_label_vocab(&mut schema);
    Ok(schema)
}

/// Constrain `[tasks].prefer` and `[tasks.overrides]` values to the closed
/// label vocabulary the resolver actually accepts (task runners, package
/// managers, source names, see `resolver::policies::resolve_source_label`),
/// instead of leaving them as unconstrained strings. Derived from
/// [`crate::types::task_source_labels`] so the schema can't drift from the
/// resolver's own vocabulary.
fn patch_tasks_label_vocab(schema: &mut Value) {
    let Some(defs) = schema.get_mut("$defs").and_then(Value::as_object_mut) else {
        return;
    };
    let Some(tasks) = defs
        .get_mut("TasksSection")
        .and_then(|def| def.get_mut("properties"))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let labels = json!(crate::types::task_source_labels());
    if let Some(prefer_items) = tasks
        .get_mut("prefer")
        .and_then(|f| f.get_mut("items"))
        .and_then(Value::as_object_mut)
    {
        prefer_items.insert("enum".to_string(), labels.clone());
    }
    if let Some(overrides_values) = tasks
        .get_mut("overrides")
        .and_then(|f| f.get_mut("additionalProperties"))
        .and_then(Value::as_object_mut)
    {
        overrides_values.insert("enum".to_string(), labels);
    }
}

fn write_all_schemas(dir: &Path) -> Result<()> {
    if dir.exists() && !dir.is_dir() {
        bail!("--all output must be a directory: {}", dir.display());
    }
    std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;

    for document in schema_documents()? {
        write_json(Some(&dir.join(document.filename)), &document.value)?;
    }

    let init_template_path = dir.join("runner.init.toml");
    std::fs::write(&init_template_path, checked_init_template()?)
        .with_context(|| format!("failed to write {}", init_template_path.display()))?;

    Ok(())
}

/// [`render_init_template`], but converted into a clean [`anyhow::Error`]
/// instead of an unhandled panic reaching `runner schema --all`'s caller.
/// `render_init_template` panics on `FIELD_TEMPLATE`/`RunnerConfig` drift
/// by design (a hard, loud failure is exactly right for the drift-guard
/// test that normally catches this before merge); this is only the
/// production CLI path's translation of that same failure into a
/// `Result`, with the default panic hook suppressed so users see one
/// clean error instead of a raw backtrace followed by one.
fn checked_init_template() -> Result<String> {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(render_init_template);
    std::panic::set_hook(previous_hook);

    result.map_err(|payload| {
        let message = panic_message(&*payload);
        anyhow::anyhow!(
            "internal error generating the runner.toml scaffold (FIELD_TEMPLATE has drifted from \
             RunnerConfig): {message}"
        )
    })
}

/// Extracts a human-readable message from a caught panic payload, covering
/// the two payload shapes `panic!`/`assert!` actually produce (`&str` for
/// string literals, `String` for `format!`-built messages) and falling back
/// to a fixed message for anything else (e.g. a payload built from
/// `panic_any` with a non-string type).
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "render_init_template panicked with a non-string payload".to_string())
}

/// How a [`FIELD_TEMPLATE`] entry's inline hint is produced.
#[derive(Clone, Copy)]
enum FieldHint {
    /// Hand-written hint text, for booleans and fields whose accepted
    /// values aren't a small fixed set ([`broader_vocab`] validates
    /// their example value instead of enumerating every label inline).
    Static(&'static str),
    /// The field's real accepted-value set ([`accepted_labels`]),
    /// pipe-joined bare, with an optional trailing suffix note.
    ClosedSet { suffix: Option<&'static str> },
    /// The field's real accepted-value set, each with a short
    /// parenthetical note. Every label [`accepted_labels`] returns for
    /// this field must have exactly one entry here, enforced by
    /// `field_template_hints_cover_every_accepted_label`.
    Annotated(&'static [(&'static str, &'static str)]),
}

/// (section, field) -> (commented-out value, hint). Every field
/// [`crate::config::RunnerConfig`]'s schemars metadata declares must
/// have an entry here, and every entry must name a real field, both
/// enforced by [`render_init_template`]'s own assertions, which run
/// whenever `committed_init_template_matches_generator` exercises it,
/// so a new config field can't ship without scaffold coverage. Values
/// are either the field's real built-in default (`fallback`,
/// `on_mismatch`, the three booleans) or, where there's no single
/// sensible default to show (an unset PM override, an empty preference
/// list), a hand-picked illustrative example, validated against the
/// real accepted vocabulary (`accepted_labels`/`broader_vocab`) by
/// `field_template_values_use_real_accepted_labels`.
const FIELD_TEMPLATE: &[(&str, &str, &str, FieldHint)] = &[
    (
        "pm",
        "node",
        r#""pnpm""#,
        FieldHint::ClosedSet { suffix: None },
    ),
    (
        "pm",
        "python",
        r#""uv""#,
        FieldHint::ClosedSet { suffix: None },
    ),
    (
        "tasks",
        "prefer",
        r#"["turbo", "bun"]"#,
        FieldHint::Static("global order: turbo, then package.json (bun)"),
    ),
    (
        "tasks",
        "overrides",
        r#"{ dev = "bun", build = "turbo" }"#,
        FieldHint::Static("per-task pins beat the order"),
    ),
    (
        "task_runner",
        "prefer",
        r#"["just", "turbo"]"#,
        FieldHint::ClosedSet { suffix: None },
    ),
    (
        "install",
        "pms",
        r#"["bun"]"#,
        FieldHint::Static("only install with these; each must be detected"),
    ),
    (
        "install",
        "scripts",
        r#""deny""#,
        FieldHint::ClosedSet {
            suffix: Some("(absent = each PM's own default)"),
        },
    ),
    (
        "install",
        "on_collision",
        r#""resolve""#,
        FieldHint::Annotated(&[
            ("resolve", "one writer per install dir, rest shadowed"),
            ("error", "refuse to pick"),
        ]),
    ),
    (
        "resolution",
        "fallback",
        r#""probe""#,
        FieldHint::Annotated(&[("probe", "PATH probe"), ("npm", "legacy"), ("error", "")]),
    ),
    (
        "resolution",
        "on_mismatch",
        r#""warn""#,
        FieldHint::Annotated(&[("warn", ""), ("ignore", ""), ("error", "exit 2")]),
    ),
    (
        "chain",
        "keep_going",
        "false",
        FieldHint::Static("run every task despite failures (same as -k)"),
    ),
    (
        "chain",
        "kill_on_fail",
        "false",
        FieldHint::Static("parallel: kill siblings on first failure (same as -K)"),
    ),
    (
        "github",
        "group_output",
        "true",
        FieldHint::Static("wrap each task's output in a collapsible ::group::"),
    ),
    (
        "github",
        "group_parallel",
        "true",
        FieldHint::Static("buffer parallel tasks, print each as one block"),
    ),
    (
        "parallel",
        "grouped",
        "false",
        FieldHint::Static("buffer + print each task as one block on completion"),
    ),
];

/// Render a [`FieldHint`] into the trailing `# ...` comment text (without
/// the leading `#`), or `None` for no hint.
fn render_hint(section: &str, field: &str, hint: &FieldHint) -> String {
    match hint {
        FieldHint::Static(text) => (*text).to_string(),
        FieldHint::ClosedSet { suffix } => {
            let labels = accepted_labels(section, field).unwrap_or_else(|| {
                panic!("{section}.{field}: ClosedSet needs an accepted_labels entry")
            });
            let joined = labels.join(" | ");
            suffix.map_or_else(|| joined.clone(), |suffix| format!("{joined}  {suffix}"))
        }
        FieldHint::Annotated(notes) => {
            let labels = accepted_labels(section, field).unwrap_or_else(|| {
                panic!("{section}.{field}: Annotated needs an accepted_labels entry")
            });
            let annotated: Vec<&str> = notes.iter().map(|(label, _)| *label).collect();
            assert!(
                annotated == labels,
                "{section}.{field}: Annotated labels {annotated:?} don't match the real accepted \
                 set {labels:?} exactly (wrong order, or a variant was added/removed without \
                 updating the annotation table)"
            );
            notes
                .iter()
                .map(|(label, note)| {
                    if note.is_empty() {
                        (*label).to_string()
                    } else {
                        format!("{label} ({note})")
                    }
                })
                .collect::<Vec<_>>()
                .join(" | ")
        }
    }
}

/// The real, closed accepted-value set for a config field with a small
/// fixed vocabulary, derived from the same types/functions the resolver
/// uses to parse that field, so it cannot drift from what's actually
/// accepted. `None` for booleans and fields with no single fixed set
/// (see [`broader_vocab`] for those with a large-but-real vocabulary).
fn accepted_labels(section: &str, field: &str) -> Option<Vec<&'static str>> {
    use crate::resolver::{CollisionPolicy, FallbackPolicy, MismatchPolicy, ScriptPolicy};
    use crate::types::{Ecosystem, PackageManager, TaskRunner};

    match (section, field) {
        ("pm", "node") => Some(
            PackageManager::all()
                .iter()
                .filter(|pm| matches!(pm.ecosystem(), Ecosystem::Node | Ecosystem::Deno))
                .map(|pm| pm.label())
                .collect(),
        ),
        ("pm", "python") => Some(
            PackageManager::all()
                .iter()
                .filter(|pm| pm.ecosystem() == Ecosystem::Python)
                .map(|pm| pm.label())
                .collect(),
        ),
        ("task_runner", "prefer") => Some(TaskRunner::all().iter().map(|r| r.label()).collect()),
        ("install", "scripts") => Some(
            ScriptPolicy::SETTABLE
                .iter()
                .filter_map(|p| p.label())
                .collect(),
        ),
        ("install", "on_collision") => {
            Some(CollisionPolicy::ALL.iter().map(|p| p.label()).collect())
        }
        ("resolution", "fallback") => Some(FallbackPolicy::ALL.iter().map(|p| p.label()).collect()),
        ("resolution", "on_mismatch") => {
            Some(MismatchPolicy::ALL.iter().map(|p| p.label()).collect())
        }
        _ => None,
    }
}

/// The real accepted-value vocabulary for a config field whose set is
/// too large to enumerate as an inline hint (so [`FIELD_TEMPLATE`] keeps
/// a hand-written [`FieldHint::Static`] hint for it), but whose example
/// *value* should still be checked against something real rather than
/// trusted blind. `None` for fields with neither a closed nor a broader
/// vocabulary to check against (plain booleans).
fn broader_vocab(section: &str, field: &str) -> Option<Vec<&'static str>> {
    match (section, field) {
        ("install", "pms") => Some(
            crate::types::PackageManager::all()
                .iter()
                .map(|pm| pm.label())
                .collect(),
        ),
        ("tasks", "prefer" | "overrides") => Some(crate::types::task_source_labels()),
        _ => None,
    }
}

/// Assert every string leaf in a [`FIELD_TEMPLATE`] `value` literal is a
/// real accepted value for `section.field`, [`accepted_labels`] when the
/// field has a closed vocabulary, else [`broader_vocab`], else no check
/// (plain booleans have neither). `value` parses directly as a bare TOML
/// value expression (scalar, array, or inline table), the same syntax
/// it's spliced into after `field = ` in the real scaffold.
fn assert_value_uses_real_labels(section: &str, field: &str, value: &str) {
    let Some(vocab) = accepted_labels(section, field).or_else(|| broader_vocab(section, field))
    else {
        return;
    };

    let parsed: toml::Value = value.parse().unwrap_or_else(|err| {
        panic!("{section}.{field}: value {value:?} is not valid TOML: {err}")
    });
    let mut leaves = Vec::new();
    collect_string_leaves(&parsed, &mut leaves);

    for leaf in leaves {
        assert!(
            vocab.contains(&leaf.as_str()),
            "{section}.{field}: example value {value:?} uses {leaf:?}, which isn't in the real \
             accepted set {vocab:?}"
        );
    }
}

fn collect_string_leaves(value: &toml::Value, out: &mut Vec<String>) {
    match value {
        toml::Value::String(s) => out.push(s.clone()),
        toml::Value::Array(items) => items.iter().for_each(|v| collect_string_leaves(v, out)),
        toml::Value::Table(map) => map.values().for_each(|v| collect_string_leaves(v, out)),
        _ => {}
    }
}

const INIT_TEMPLATE_HEADER: &str = r"#:schema ./runner.toml.schema.json

# runner.toml, project task-runner configuration.
# Docs: https://runner.kjanat.dev
#
# Every key below is commented out, showing either its built-in default or an
# illustrative example value. Uncomment and edit the ones you want to pin.
# Precedence, highest first:
#   CLI flags  >  RUNNER_* env vars  >  this file  >  manifest declarations.
";

/// Render the `runner.toml` scaffold `runner config init` writes.
///
/// Walks [`crate::config::RunnerConfig`]'s schemars metadata. Section
/// order and doc-comment descriptions come straight from the struct, so
/// a field can't be silently forgotten or its prose silently drift from
/// the type. [`FIELD_TEMPLATE`] supplies the one thing schemars can't:
/// which value to show commented-out.
///
/// # Panics
///
/// Panics if `RunnerConfig`'s schema is malformed (a property without a
/// `$defs` `$ref`) or a schema field has no [`FIELD_TEMPLATE`] entry.
/// Both indicate a real bug the generator should surface loudly, not
/// paper over, since this only ever runs under `just gen-schema`.
pub(crate) fn render_init_template() -> String {
    let schema = serde_json::to_value(schemars::schema_for!(crate::config::RunnerConfig))
        .expect("RunnerConfig schema should serialize");
    let top_properties = schema["properties"]
        .as_object()
        .expect("RunnerConfig schema must have top-level properties");
    let defs = schema["$defs"]
        .as_object()
        .expect("RunnerConfig schema must have $defs");

    let mut out = INIT_TEMPLATE_HEADER.to_string();
    let mut used = std::collections::HashSet::with_capacity(FIELD_TEMPLATE.len());
    for (section, section_schema) in top_properties {
        let def_name = section_schema["$ref"]
            .as_str()
            .and_then(|r| r.strip_prefix("#/$defs/"))
            .unwrap_or_else(|| panic!("{section}: expected a $defs $ref in the schema"));
        let def = &defs[def_name];
        let properties = def["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{def_name}: expected a properties object"));

        if def["deprecated"].as_bool().unwrap_or(false) {
            // Deprecated sections (e.g. `task_runner`, superseded by `tasks`)
            // still need their FIELD_TEMPLATE entries validated so drift is
            // caught, but new users shouldn't be handed a deprecated section
            // in their starter file, so skip printing it entirely.
            for field in properties.keys() {
                let &(entry_section, entry_field, value, hint) = FIELD_TEMPLATE
                    .iter()
                    .find(|(s, f, ..)| s == section && f == field)
                    .unwrap_or_else(|| panic!("{section}.{field}: missing FIELD_TEMPLATE entry"));
                used.insert((entry_section, entry_field));
                assert_value_uses_real_labels(section, field, value);
                let _ = render_hint(section, field, &hint);
            }
            continue;
        }

        let description = def["description"].as_str().unwrap_or_default();

        out.push('\n');
        for line in description.lines() {
            if line.is_empty() {
                out.push_str("#\n");
            } else {
                out.push_str("# ");
                out.push_str(&strip_intra_doc_links(line));
                out.push('\n');
            }
        }
        let _ = writeln!(out, "[{section}]");

        for field in properties.keys() {
            let &(entry_section, entry_field, value, hint) = FIELD_TEMPLATE
                .iter()
                .find(|(s, f, ..)| s == section && f == field)
                .unwrap_or_else(|| panic!("{section}.{field}: missing FIELD_TEMPLATE entry"));
            used.insert((entry_section, entry_field));
            assert_value_uses_real_labels(section, field, value);
            let hint = render_hint(section, field, &hint);
            let _ = writeln!(out, "# {field} = {value}   # {hint}");
        }
    }

    let orphaned: Vec<String> = FIELD_TEMPLATE
        .iter()
        .filter(|&&(s, f, ..)| !used.contains(&(s, f)))
        .map(|(s, f, ..)| format!("{s}.{f}"))
        .collect();
    assert!(
        orphaned.is_empty(),
        "FIELD_TEMPLATE has entries for fields RunnerConfig no longer declares: {orphaned:?}, \
         remove them"
    );

    out
}

/// Rewrite a rustdoc intra-doc link (`` [`Type::field`] ``) into plain
/// backticked text (`` `Type::field` ``). The square brackets signal a
/// hyperlink to rustdoc, but read as stray punctuation in the plain-text
/// scaffold comments this feeds.
fn strip_intra_doc_links(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find("[`") {
        out.push_str(&rest[..start]);
        let after_bracket = &rest[start + 1..];
        let Some(end) = after_bracket.find("`]") else {
            out.push_str(&rest[start..]);
            return out;
        };
        out.push_str(&after_bracket[..=end]);
        rest = &after_bracket[end + 2..];
    }
    out.push_str(rest);
    out
}

fn schema_documents() -> Result<Vec<SchemaDocument>> {
    Ok(vec![
        SchemaDocument {
            filename: "runner.toml.schema.json",
            value: config_schema()?,
        },
        SchemaDocument {
            filename: "doctor.schema.json",
            value: output_schema::<crate::schema::doctor::DoctorReport<'static>>("doctor")?,
        },
        SchemaDocument {
            filename: "list.schema.json",
            value: output_schema::<TaskListView<'static>>("list")?,
        },
        SchemaDocument {
            filename: "why.schema.json",
            value: output_schema::<super::why::WhyReport<'static>>("why")?,
        },
    ])
}

fn output_schema<T: JsonSchema>(command: &'static str) -> Result<Value> {
    let mut schema = serialize_schema_value::<T>()?;
    set_object_field(&mut schema, "$id", json!(schema_id(command)));
    set_object_field(&mut schema, "title", json!(title(command)));
    set_object_field(&mut schema, "description", json!(description(command)));
    patch_schema_version_const(&mut schema);
    patch_source_schema(&mut schema, command);
    Ok(schema)
}

fn serialize_schema_value<T: JsonSchema>() -> Result<Value> {
    let generator = schemars::generate::SchemaSettings::default()
        .for_serialize()
        .into_generator();
    schema_value(generator.into_root_schema_for::<T>())
}

fn schema_value(schema: Schema) -> Result<Value> {
    serde_json::to_value(schema).context("failed to serialize schema")
}

fn write_json(output: Option<&Path>, value: &Value) -> Result<()> {
    let mut sorted = value.clone();
    json_schema_sort::sort_schema(&mut sorted);
    let json = serde_json::to_string_pretty(&sorted).context("failed to serialize schema")?;
    output.map_or_else(
        || writeln!(std::io::stdout(), "{json}").context("failed to write schema to stdout"),
        |path| {
            std::fs::write(path, format!("{json}\n"))
                .with_context(|| format!("failed to write {}", path.display()))
        },
    )
}

fn set_object_field(schema: &mut Value, key: &'static str, value: Value) {
    if let Some(object) = schema.as_object_mut() {
        object.insert(key.to_string(), value);
    }
}

fn patch_schema_version_const(schema: &mut Value) {
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return;
    };
    let Some(version_schema) = properties
        .get_mut("schema_version")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    version_schema.insert("const".to_string(), json!(crate::schema::SCHEMA_VERSION));
}

fn patch_source_schema(schema: &mut Value, command: &str) {
    let Some(defs) = schema.get_mut("$defs").and_then(Value::as_object_mut) else {
        return;
    };

    defs.insert(
        "TaskSourceLabel".to_string(),
        task_source_label_schema(command),
    );
    patch_task_info_source(defs);
    patch_why_task(defs);
    patch_def_field(defs, "SourceEntry", "kind", "TaskSourceLabel");
    patch_overrides_source_labels(defs);
}

/// `Overrides.prefer_sources`/`task_source_pins` hold the same structured
/// source labels as `SourceEntry.kind`, command-dependent like it; reuse
/// `TaskSourceLabel` instead of leaving them generic strings. (Every other
/// `Overrides` label field is backed by a real enum and gets its schema
/// constraint straight from `#[derive(schemars::JsonSchema)]` on that enum,
/// no hand-built schema needed there.)
fn patch_overrides_source_labels(defs: &mut Map<String, Value>) {
    if !defs.contains_key("Overrides") {
        return;
    }
    patch_def_array_items(defs, "Overrides", "prefer_sources", "TaskSourceLabel");
    patch_def_map_array_items(defs, "Overrides", "task_source_pins", "TaskSourceLabel");
}

fn patch_task_info_source(defs: &mut Map<String, Value>) {
    patch_def_field(defs, "TaskInfo", "source", "TaskSourceLabel");
}

/// The `why` task object splits the old flat `source` label into `kind`
/// (mechanism label) and `provider` (executing tool family); constrain
/// both to their closed label sets.
fn patch_why_task(defs: &mut Map<String, Value>) {
    if !defs.contains_key("WhyTask") {
        return;
    }
    defs.insert(
        "ProviderLabel".to_string(),
        json!({ "type": "string", "enum": provider_labels() }),
    );
    patch_def_field(defs, "WhyTask", "kind", "TaskSourceLabel");
    patch_def_field(defs, "WhyTask", "provider", "ProviderLabel");
}

/// Mutable handle on `$defs.<def_name>.properties.<field>`, the shared
/// navigation prefix of every schema patch below.
fn field_schema_mut<'a>(
    defs: &'a mut Map<String, Value>,
    def_name: &str,
    field: &str,
) -> Option<&'a mut Value> {
    defs.get_mut(def_name)
        .and_then(|definition| definition.get_mut("properties"))
        .and_then(Value::as_object_mut)
        .and_then(|properties| properties.get_mut(field))
}

fn def_ref(target_def: &str) -> Value {
    json!({ "$ref": format!("#/$defs/{target_def}") })
}

fn patch_def_field(
    defs: &mut Map<String, Value>,
    def_name: &'static str,
    field: &'static str,
    target_def: &'static str,
) {
    if let Some(field_schema) = field_schema_mut(defs, def_name, field) {
        *field_schema = def_ref(target_def);
    }
}

/// Like [`patch_def_field`], but for an array-typed field, constrains its
/// `items` schema instead of the field itself.
fn patch_def_array_items(
    defs: &mut Map<String, Value>,
    def_name: &'static str,
    field: &'static str,
    target_def: &'static str,
) {
    if let Some(items) = field_schema_mut(defs, def_name, field)
        .and_then(|field_schema| field_schema.get_mut("items"))
    {
        *items = def_ref(target_def);
    }
}

/// Like [`patch_def_array_items`], but for a map-of-array field, constrains
/// the array items nested under `additionalProperties`.
fn patch_def_map_array_items(
    defs: &mut Map<String, Value>,
    def_name: &'static str,
    field: &'static str,
    target_def: &'static str,
) {
    if let Some(items) = field_schema_mut(defs, def_name, field)
        .and_then(|field_schema| field_schema.get_mut("additionalProperties"))
        .and_then(|additional| additional.get_mut("items"))
    {
        *items = def_ref(target_def);
    }
}

fn task_source_label_schema(command: &str) -> Value {
    json!({ "type": "string", "enum": source_labels(command) })
}

/// Closed set for the `why` `provider` field, the tool family that
/// executes the task. Derived from [`crate::types::TaskSource::all`]
/// through [`super::why::provider_label`], the same function `why` calls
/// at runtime, so the committed schema's enum can't drift from it.
fn provider_labels() -> Vec<&'static str> {
    crate::types::TaskSource::all()
        .iter()
        .map(|&source| super::why::provider_label(source))
        .collect()
}

/// Closed label set for `command`'s source labels, derived from
/// [`crate::types::TaskSource::all`] through the same label functions
/// `list`/`doctor`/`why` use at runtime, so the committed schema's enum
/// can't drift from what the binary actually emits. `list` uses the flat
/// label convention; `doctor`/`why` use the structured one (only
/// `CargoAliases` differs, `"cargo-alias"` vs `"cargo"`).
fn source_labels(command: &str) -> Vec<&'static str> {
    use crate::schema::labels::{flat_source_label, structured_source_label};
    use crate::types::TaskSource;

    TaskSource::all()
        .iter()
        .map(|&source| {
            if command == "list" {
                flat_source_label(source)
            } else {
                structured_source_label(source)
            }
        })
        .collect()
}

fn schema_id(command: &str) -> String {
    crate::schema::schema_url(command)
}

fn title(command: &str) -> String {
    match command {
        "why" => "runner why <task> --json".to_string(),
        _ => format!("runner {command} --json"),
    }
}

fn description(command: &str) -> String {
    match command {
        "doctor" => "JSON schema for `runner doctor --json`: structured diagnostic inventory with \
                     invocation/environment provenance, per-ecosystem decisions, sources, \
                     fqn-keyed tasks, tools, conflicts, and diagnostics."
            .to_string(),
        "why" => "JSON schema for `runner why <task> --json`: candidate `{task, match}` pairs \
                  plus the selection decision."
            .to_string(),
        _ => format!("JSON schema for `{}`.", title(command)),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    #[test]
    fn doctor_conflict_schema_distinguishes_task_and_install_metadata() {
        let schema = super::output_schema::<crate::schema::doctor::DoctorReport<'static>>("doctor")
            .expect("doctor schema should generate");
        let variants = schema["$defs"]["Conflict"]["oneOf"]
            .as_array()
            .expect("Conflict should be split by kind");
        let variant = |kind: &str| {
            variants
                .iter()
                .find(|variant| variant["properties"]["kind"]["const"] == kind)
                .unwrap_or_else(|| panic!("missing {kind} conflict schema"))
        };

        let task = variant("duplicate-task-name");
        assert_eq!(
            task["properties"]["selected"]["description"],
            "FQN of the winning task."
        );
        assert_eq!(
            task["properties"]["shadowed"]["description"],
            "FQNs of the shadowed tasks."
        );

        let install = variant("install-dir-collision");
        assert_eq!(
            install["properties"]["selected"]["description"],
            "Label of the selected package manager."
        );
        assert_eq!(
            install["properties"]["selector"]["description"],
            "Path of the conflicting installation directory."
        );
        assert_eq!(
            install["properties"]["shadowed"]["description"],
            "Labels of the shadowed package managers."
        );
    }

    #[test]
    fn committed_doctor_example_includes_quiet_override() {
        let raw = std::fs::read_to_string("schemas/doctor.example.json")
            .expect("committed doctor example should be readable");
        let example: Value = serde_json::from_str(&raw).expect("example should parse as JSON");

        assert_eq!(example["overrides"]["quiet"], serde_json::json!(false));
    }

    /// Every committed schema file that carries a `TaskSourceLabel` def,
    /// paired with whether its source labels follow the structured
    /// (`doctor`/`why`) or flat (`list`) convention.
    const COMMITTED_SCHEMAS_WITH_TASK_SOURCE_LABEL: &[(&str, &str)] = &[
        ("schemas/doctor.schema.json", "doctor"),
        ("schemas/list.schema.json", "list"),
        ("schemas/why.schema.json", "why"),
    ];

    fn runtime_labels(command: &str) -> Vec<&'static str> {
        use crate::schema::labels::{flat_source_label, structured_source_label};
        use crate::types::TaskSource;

        TaskSource::all()
            .iter()
            .map(|&source| {
                if command == "list" {
                    flat_source_label(source)
                } else {
                    structured_source_label(source)
                }
            })
            .collect()
    }

    #[test]
    fn task_source_label_schema_matches_runtime_labels_per_command() {
        // source_labels(command) used to be three hand-maintained arrays,
        // free to drift from the label functions `list`/`why`/`doctor`
        // actually call at runtime. Now that it's derived, this test is a
        // tautology against today's implementation; its job is to catch a
        // future regression back to a hardcoded list.
        for command in ["list", "doctor", "why"] {
            let schema = super::task_source_label_schema(command);
            let enum_values: Vec<&str> = schema["enum"]
                .as_array()
                .unwrap_or_else(|| panic!("{command}: expected enum array"))
                .iter()
                .map(|v| v.as_str().expect("enum values should be strings"))
                .collect();
            assert_eq!(
                enum_values,
                runtime_labels(command),
                "{command}: schema TaskSourceLabel enum must match the runtime label function"
            );
        }
    }

    #[test]
    fn committed_schemas_task_source_label_matches_runtime_labels() {
        // The in-memory generator test above proves the generator is
        // correct; it says nothing about whether `just gen-schema` was
        // actually re-run before committing. Read every checked-in schema
        // that defines TaskSourceLabel directly off disk so a stale
        // artifact, the generator fixed, the commit forgotten, fails
        // here instead of shipping silently.
        for &(path, command) in COMMITTED_SCHEMAS_WITH_TASK_SOURCE_LABEL {
            let raw = std::fs::read_to_string(path)
                .unwrap_or_else(|err| panic!("{path}: should be readable: {err}"));
            let schema: Value = serde_json::from_str(&raw)
                .unwrap_or_else(|err| panic!("{path}: should parse as JSON: {err}"));
            let enum_values: Vec<&str> = schema["$defs"]["TaskSourceLabel"]["enum"]
                .as_array()
                .unwrap_or_else(|| panic!("{path}: expected $defs.TaskSourceLabel.enum array"))
                .iter()
                .map(|v| v.as_str().expect("enum values should be strings"))
                .collect();
            assert_eq!(
                enum_values,
                runtime_labels(command),
                "{path}: committed TaskSourceLabel enum has drifted from the runtime label \
                 function, run `just gen-schema` and commit the result"
            );
        }
    }

    #[test]
    fn provider_label_schema_matches_runtime_labels() {
        // provider_labels() used to be a hand-maintained PROVIDER_LABELS
        // array, free to drift from cmd::why::provider_label. Now that
        // it's derived, this test is a tautology against today's
        // implementation; its job is to catch a future regression back
        // to a hardcoded list.
        let enum_values = super::provider_labels();
        let runtime_values: Vec<&str> = crate::types::TaskSource::all()
            .iter()
            .map(|&source| super::super::why::provider_label(source))
            .collect();
        assert_eq!(
            enum_values, runtime_values,
            "ProviderLabel enum must match cmd::why::provider_label exactly"
        );
    }

    #[test]
    fn committed_why_schema_provider_label_matches_runtime_labels() {
        // Mirrors committed_schemas_task_source_label_matches_runtime_labels:
        // proves the committed schemas/why.schema.json wasn't left stale
        // after a generator fix.
        let raw = std::fs::read_to_string("schemas/why.schema.json")
            .expect("committed why schema should be readable");
        let schema: Value = serde_json::from_str(&raw).expect("schema should parse as JSON");
        let enum_values: Vec<&str> = schema["$defs"]["ProviderLabel"]["enum"]
            .as_array()
            .expect("expected $defs.ProviderLabel.enum array")
            .iter()
            .map(|v| v.as_str().expect("enum values should be strings"))
            .collect();
        assert_eq!(
            enum_values,
            super::provider_labels(),
            "schemas/why.schema.json: committed ProviderLabel enum has drifted from \
             cmd::why::provider_label, run `just gen-schema` and commit the result"
        );
    }

    #[test]
    fn committed_init_template_matches_generator() {
        let generated = super::render_init_template();
        let committed = std::fs::read_to_string("schemas/runner.init.toml")
            .expect("committed init template should be readable");
        assert_eq!(
            generated, committed,
            "schemas/runner.init.toml has drifted from render_init_template(), run `just \
             gen-schema` and commit the result"
        );
    }

    #[test]
    fn panic_message_extracts_str_payload() {
        let payload: Box<dyn std::any::Any + Send> = Box::new("boom");
        assert_eq!(super::panic_message(&*payload), "boom");
    }

    #[test]
    fn panic_message_extracts_string_payload() {
        let payload: Box<dyn std::any::Any + Send> = Box::new(String::from("boom"));
        assert_eq!(super::panic_message(&*payload), "boom");
    }

    #[test]
    fn panic_message_falls_back_for_non_string_payload() {
        let payload: Box<dyn std::any::Any + Send> = Box::new(42_i32);
        assert_eq!(
            super::panic_message(&*payload),
            "render_init_template panicked with a non-string payload"
        );
    }
}
