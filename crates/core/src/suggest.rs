//! Conservative spelling suggestions before speculative execution.

use crate::{Cascade, NameShape};

pub(crate) fn corrections(cascade: &Cascade<'_>, name: &str, hints: &[&str]) -> Vec<String> {
    if NameShape::of(name) != NameShape::BARE || name.len() > 128 {
        return Vec::new();
    }
    let mut candidates: Vec<String> = cascade
        .builtins
        .iter()
        .copied()
        .chain(hints.iter().copied())
        .chain(
            cascade
                .project
                .tasks
                .iter()
                .filter(|task| {
                    cascade
                        .policy
                        .source
                        .as_ref()
                        .is_none_or(|choice| choice.id == task.source)
                })
                .map(|task| task.name.as_str())
                .filter(|name| matches!(crate::select(cascade, name), Ok(Some(_)))),
        )
        .filter(|candidate| *candidate != name && close(name, candidate.trim_start_matches('-')))
        .map(str::to_owned)
        .collect();
    candidates.sort_unstable();
    candidates.dedup();
    candidates.truncate(3);
    candidates
}

/// At most one insertion, deletion, substitution, or adjacent transposition.
fn close(left: &str, right: &str) -> bool {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.len().min(right.len()) < 3 || left.len().abs_diff(right.len()) > 1 {
        return false;
    }
    let prefix = left.iter().zip(&right).take_while(|(a, b)| a == b).count();
    let a = &left[prefix..];
    let b = &right[prefix..];
    match a.len().cmp(&b.len()) {
        std::cmp::Ordering::Less => a == &b[1..],
        std::cmp::Ordering::Greater => &a[1..] == b,
        std::cmp::Ordering::Equal => {
            a.is_empty()
                || a[1..] == b[1..]
                || (a.len() >= 2 && a[0] == b[1] && a[1] == b[0] && a[2..] == b[2..])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::close;

    #[test]
    fn accepts_common_typos_without_broad_fuzzy_matches() {
        for (a, b) in [
            ("biuld", "build"),
            ("buld", "build"),
            ("buiild", "build"),
            ("builf", "build"),
            ("version", "version"),
            ("tést", "test"),
        ] {
            assert!(close(a, b), "{a} -> {b}");
        }
        for (a, b) in [
            ("x", "y"),
            ("bd", "build"),
            ("other", "build"),
            ("building", "build"),
        ] {
            assert!(!close(a, b), "{a} -> {b}");
        }
    }
}
