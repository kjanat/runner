//! npm semver: `package.json` ranges against `<tool> --version` output.

use runner_core::{ParseError, Scheme};

/// The npm range grammar over the semver version grammar.
pub struct NodeSemver;

impl Scheme for NodeSemver {
    type Version = semver::Version;
    type Constraint = semver::VersionReq;

    fn version(s: &str) -> Result<Self::Version, ParseError> {
        let token = version_token(s).ok_or_else(|| ParseError {
            input: s.to_owned(),
            reason: "no semver token".to_owned(),
        })?;
        semver::Version::parse(&padded(token)).map_err(|err| ParseError {
            input: s.to_owned(),
            reason: err.to_string(),
        })
    }

    fn constraint(s: &str) -> Result<Self::Constraint, ParseError> {
        semver::VersionReq::parse(s.trim()).map_err(|err| ParseError {
            input: s.to_owned(),
            reason: err.to_string(),
        })
    }

    fn satisfies(v: &Self::Version, c: &Self::Constraint) -> bool {
        if c.matches(v) {
            return true;
        }
        !v.pre.is_empty()
            && c.comparators
                .iter()
                .all(|comparator| comparator.pre.is_empty())
            && c.matches(&semver::Version::new(v.major, v.minor, v.patch))
    }
}

/// The first word of the first nonempty line that reads as a version, without a `v` prefix.
fn version_token(text: &str) -> Option<&str> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    line.split_whitespace().find_map(|word| {
        let word = word.strip_prefix('v').unwrap_or(word);
        semver::Version::parse(&padded(word)).ok().map(|_| word)
    })
}

/// `major` and `major.minor` padded to a full triple.
fn padded(raw: &str) -> String {
    let segments: Vec<&str> = raw.split('.').collect();
    match segments.len() {
        1 => format!("{}.0.0", segments[0]),
        2 => format!("{}.{}.0", segments[0], segments[1]),
        _ => raw.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use runner_core::{Check, check};

    use super::NodeSemver;

    #[test]
    fn versions_are_read_from_version_output() {
        for (output, expected) in [
            ("10.9.2", "10.9.2"),
            ("v22.1.0", "22.1.0"),
            (
                "deno 2.7.12 (stable, release, x86_64-unknown-linux-gnu)",
                "2.7.12",
            ),
            ("9", "9.0.0"),
            ("4.1", "4.1.0"),
            ("\n1.2.3\n", "1.2.3"),
        ] {
            assert_eq!(
                <NodeSemver as runner_core::Scheme>::version(output).unwrap(),
                semver::Version::parse(expected).unwrap(),
                "{output}"
            );
        }
        assert!(<NodeSemver as runner_core::Scheme>::version("no version here").is_err());
    }

    #[test]
    fn ranges_check_installed_versions() {
        assert_eq!(check::<NodeSemver>(">=9.0.0", "9.1.0"), Check::Satisfied);
        assert_eq!(check::<NodeSemver>("^4.0.0", "4.1"), Check::Satisfied);
        assert_eq!(
            check::<NodeSemver>(">=9.0.0", "8.15.0"),
            Check::Violated {
                declared: ">=9.0.0".to_owned(),
                found: "8.15.0".to_owned(),
            }
        );
        assert!(matches!(
            check::<NodeSemver>("not-a-valid-range", "1.0.0"),
            Check::Unknown { .. }
        ));
    }

    #[test]
    fn a_prerelease_clears_the_range_its_release_clears() {
        assert_eq!(
            check::<NodeSemver>(">=1.0.0", "1.3.0-canary.1"),
            Check::Satisfied
        );
        assert!(matches!(
            check::<NodeSemver>(">=1.0.0-beta", "0.9.0-beta"),
            Check::Violated { .. }
        ));
    }
}
