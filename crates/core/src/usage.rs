//! Per-task argument specs.

/// A task's arguments and flags, as its source declares them.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct UsageSpec {
    /// One-line signature the source renders for the task, e.g. `<--fn <name>> [dir]`.
    pub signature: String,
    /// Positional arguments in declaration order.
    pub args: Vec<UsageArg>,
    /// Accepted flags.
    pub flags: Vec<UsageFlag>,
}

/// One positional argument of a task.
#[derive(Debug, PartialEq, Eq)]
pub struct UsageArg {
    /// Argument name.
    pub name: String,
    /// Help from the usage declaration.
    pub help: Option<String>,
    /// Whether callers must supply a value.
    pub required: bool,
    /// Accepted values, when the spec closes the set with `choices`.
    pub choices: Vec<String>,
}

/// One flag of a task.
#[derive(Debug, PartialEq, Eq)]
pub struct UsageFlag {
    /// Long spellings without the `--`.
    pub long: Vec<String>,
    /// Short spellings without the `-`.
    pub short: Vec<String>,
    /// Help from the usage declaration.
    pub help: Option<String>,
    /// Whether callers must supply a value.
    pub required: bool,
    /// `true` when the flag consumes the next word.
    pub takes_value: bool,
}

impl UsageSpec {
    /// `true` when the spec declares nothing worth completing or checking.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.args.is_empty() && self.flags.is_empty()
    }

    /// `true` when `word` is a flag that swallows the word after it, so the
    /// next position holds that flag's value rather than another flag.
    #[must_use]
    pub fn consumes_value_after(&self, word: &str) -> bool {
        let Some((name, long)) = word
            .strip_prefix("--")
            .map(|name| (name, true))
            .or_else(|| word.strip_prefix('-').map(|name| (name, false)))
        else {
            return false;
        };
        // `--flag=value` and `-f=value` carry their value already.
        if name.is_empty() || name.contains('=') {
            return false;
        }
        self.flags
            .iter()
            .filter(|flag| flag.takes_value)
            .any(|flag| {
                let spellings = if long { &flag.long } else { &flag.short };
                spellings.iter().any(|spelling| spelling == name)
            })
    }

    /// Every flag the spec marks required that `provided` does not set,
    /// named by its first spelling.
    ///
    /// A flag may declare only a short form, so both forms count as
    /// provided and a short-only flag is still reported when absent.
    #[must_use]
    pub fn missing_required_flags(&self, provided: &[String]) -> Vec<String> {
        self.flags
            .iter()
            .filter(|flag| flag.required)
            .filter(|flag| {
                !flag.spellings().into_iter().any(|dashed| {
                    provided
                        .iter()
                        .any(|word| word == &dashed || word.starts_with(&format!("{dashed}=")))
                })
            })
            .filter_map(|flag| flag.spellings().into_iter().next())
            .collect()
    }
}

impl UsageFlag {
    /// Every spelling of this flag, long forms first.
    #[must_use]
    pub fn spellings(&self) -> Vec<String> {
        self.long
            .iter()
            .map(|long| format!("--{long}"))
            .chain(self.short.iter().map(|short| format!("-{short}")))
            .collect()
    }
}

/// The spec `task`'s source declares for it, when the source has one.
///
/// # Errors
/// Returns the source's failure to read the spec.
pub fn usage(
    tree: &crate::Tree,
    project: &crate::Project,
    task: &crate::Task,
    registry: &crate::Registry,
) -> Result<Option<UsageSpec>, crate::Warning> {
    let Some(present) = project.present_in(task.source, &task.scope) else {
        return Ok(None);
    };
    let provider = registry.by_id(task.source).for_present(present);
    provider
        .caps
        .usage
        .map_or(Ok(None), |cap| (cap.spec)(tree, present, task))
}

#[cfg(test)]
mod tests {
    use super::{UsageArg, UsageFlag, UsageSpec};

    fn leaf_spec() -> UsageSpec {
        UsageSpec {
            signature: "<--fn <name>> [dir]".to_string(),
            args: vec![UsageArg {
                name: "dir".to_string(),
                help: Some("Core dump directory".to_string()),
                required: false,
                choices: vec![],
            }],
            flags: vec![UsageFlag {
                long: vec!["fn".to_string()],
                short: vec![],
                help: Some("Stable function name".to_string()),
                required: true,
                takes_value: true,
            }],
        }
    }

    fn two_flag_spec() -> UsageSpec {
        UsageSpec {
            signature: "<-f <name>> [--dry-run]".to_string(),
            args: vec![],
            flags: vec![
                UsageFlag {
                    long: vec![],
                    short: vec!["f".to_string()],
                    help: None,
                    required: true,
                    takes_value: true,
                },
                UsageFlag {
                    long: vec!["dry-run".to_string()],
                    short: vec![],
                    help: None,
                    required: false,
                    takes_value: false,
                },
            ],
        }
    }

    #[test]
    fn a_short_only_required_flag_is_reported_when_absent() {
        let spec = two_flag_spec();
        assert_eq!(spec.missing_required_flags(&[]), ["-f"]);
        assert_eq!(
            spec.missing_required_flags(&["-f".to_string(), "x".to_string()]),
            Vec::<String>::new()
        );
        assert_eq!(
            spec.missing_required_flags(&["-f=x".to_string()]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_short_flag_consumes_the_word_after_it() {
        let spec = two_flag_spec();
        assert!(spec.consumes_value_after("-f"));
        // Carries its value already.
        assert!(!spec.consumes_value_after("-f=x"));
        // Takes no value.
        assert!(!spec.consumes_value_after("--dry-run"));
        assert!(!spec.consumes_value_after("-"));
        assert!(!spec.consumes_value_after("--"));
    }

    #[test]
    fn spellings_lists_long_then_short() {
        let spec = two_flag_spec();
        assert_eq!(spec.flags[0].spellings(), ["-f"]);
        assert_eq!(spec.flags[1].spellings(), ["--dry-run"]);
        assert_eq!(leaf_spec().flags[0].spellings(), ["--fn"]);
    }

    #[test]
    fn missing_required_flags_reports_an_absent_flag() {
        let spec = leaf_spec();
        assert_eq!(
            spec.missing_required_flags(&["compiler/core-json".to_string()]),
            ["--fn"],
        );
        assert_eq!(
            spec.missing_required_flags(&["--fn".to_string(), "foo".to_string()]),
            Vec::<String>::new()
        );
        // The `--flag=value` spelling counts as provided.
        assert_eq!(
            spec.missing_required_flags(&["--fn=foo".to_string()]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn consumes_value_after_only_for_value_taking_flags() {
        let spec = leaf_spec();
        assert!(spec.consumes_value_after("--fn"));
        // Already carries its value, so the next word is not it.
        assert!(!spec.consumes_value_after("--fn=foo"));
        assert!(!spec.consumes_value_after("--unknown"));
        assert!(!spec.consumes_value_after("dir"));
    }

    #[test]
    fn usage_spec_is_empty_without_args_or_flags() {
        assert!(UsageSpec::default().is_empty());
        assert!(!leaf_spec().is_empty());
    }
}
