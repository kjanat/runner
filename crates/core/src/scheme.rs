//! An ecosystem's version grammar.

use std::fmt;

/// A version or constraint that did not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// The input.
    pub input: String,
    /// What was wrong with it.
    pub reason: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.input, self.reason)
    }
}

impl std::error::Error for ParseError {}

/// One ecosystem's version grammar and comparison rules.
pub trait Scheme {
    /// A parsed version.
    type Version: Ord;
    /// A parsed constraint.
    type Constraint;

    /// Parse a version.
    ///
    /// # Errors
    ///
    /// When `s` is not a version in this grammar.
    fn version(s: &str) -> Result<Self::Version, ParseError>;

    /// Parse a constraint.
    ///
    /// # Errors
    ///
    /// When `s` is not a constraint in this grammar.
    fn constraint(s: &str) -> Result<Self::Constraint, ParseError>;

    /// Whether `v` satisfies `c`.
    fn satisfies(v: &Self::Version, c: &Self::Constraint) -> bool;
}

/// The outcome of checking a found version against a declared constraint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// The found version satisfies the constraint.
    Satisfied,
    /// It does not.
    Violated {
        /// The constraint as written.
        declared: String,
        /// The version as found.
        found: String,
    },
    /// The question could not be answered.
    Unknown {
        /// Why.
        reason: String,
    },
}

/// Check `found` against `declared` under scheme `S`.
#[must_use]
pub fn check<S: Scheme>(declared: &str, found: &str) -> Check {
    let constraint = match S::constraint(declared) {
        Ok(constraint) => constraint,
        Err(err) => {
            return Check::Unknown {
                reason: format!("constraint {err}"),
            };
        }
    };
    let version = match S::version(found) {
        Ok(version) => version,
        Err(err) => {
            return Check::Unknown {
                reason: format!("version {err}"),
            };
        }
    };
    if S::satisfies(&version, &constraint) {
        Check::Satisfied
    } else {
        Check::Violated {
            declared: declared.to_owned(),
            found: found.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Check, ParseError, Scheme, check};

    struct Major;

    impl Scheme for Major {
        type Version = u32;
        type Constraint = u32;

        fn version(s: &str) -> Result<u32, ParseError> {
            s.parse().map_err(|_| ParseError {
                input: s.to_owned(),
                reason: "not a major".to_owned(),
            })
        }

        fn constraint(s: &str) -> Result<u32, ParseError> {
            Self::version(s.trim_start_matches(">="))
        }

        fn satisfies(v: &u32, c: &u32) -> bool {
            v >= c
        }
    }

    #[test]
    fn check_reports_each_outcome() {
        assert_eq!(check::<Major>(">=18", "20"), Check::Satisfied);
        assert_eq!(
            check::<Major>(">=22", "20"),
            Check::Violated {
                declared: ">=22".to_owned(),
                found: "20".to_owned(),
            }
        );
        assert!(matches!(
            check::<Major>("lts/*", "20"),
            Check::Unknown { .. }
        ));
    }
}
