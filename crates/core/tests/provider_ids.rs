//! Article 1 of `docs/architecture.md`: the core never names a provider.

use std::path::{Path, PathBuf};

fn sources(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).expect("source dir reads") {
        let path = entry.expect("entry reads").path();
        if path.is_dir() {
            found.extend(sources(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
    found
}

fn outside_test_modules(text: &str) -> Vec<(usize, &str)> {
    let mut kept = Vec::new();
    let mut lines = text.lines().enumerate().peekable();
    while let Some((index, line)) = lines.next() {
        if line == "#[cfg(test)]"
            && lines
                .peek()
                .is_some_and(|(_, next)| next.starts_with("mod ") && next.ends_with('{'))
        {
            for (_, inner) in lines.by_ref() {
                if inner == "}" {
                    break;
                }
            }
            continue;
        }
        kept.push((index + 1, line));
    }
    kept
}

fn aliases(lines: &[(usize, &str)]) -> Vec<String> {
    let ident = |text: &str| -> String {
        text.chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect()
    };
    let mut names = vec!["ProviderId".to_owned()];
    for (_, line) in lines {
        if let Some((_, rest)) = line.split_once("ProviderId as ") {
            names.push(ident(rest));
        }
        if let Some(rest) = line.trim_start().strip_prefix("type ")
            && line.trim_end().ends_with("ProviderId;")
        {
            names.push(ident(rest));
        }
    }
    names.retain(|name| !name.is_empty());
    names
}

fn names_a_provider(line: &str, names: &[String]) -> bool {
    names.iter().any(|name| {
        line.match_indices(&format!("{name}::")).any(|(at, _)| {
            !line[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        })
    })
}

fn provider_paths(text: &str) -> Vec<(usize, &str)> {
    let lines = outside_test_modules(text);
    let names = aliases(&lines);
    lines
        .into_iter()
        .filter(|(_, line)| names_a_provider(line, &names))
        .collect()
}

#[test]
fn the_core_never_names_a_provider_outside_its_tests() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    for path in sources(&src) {
        let text = std::fs::read_to_string(&path).expect("source reads");
        for (number, line) in provider_paths(&text) {
            hits.push(format!("{}:{number}: {}", path.display(), line.trim()));
        }
    }
    assert!(hits.is_empty(), "{}", hits.join("\n"));
}

#[test]
fn aliased_provider_paths_are_found() {
    let text = "use crate::provider::ProviderId as P;\ntype Id = ProviderId;\nfn a() { P::Npm; \
                }\nfn b() { Id::Bun; }\nfn c() { MaP::Npm; }\n";
    assert_eq!(
        provider_paths(text),
        [(3, "fn a() { P::Npm; }"), (4, "fn b() { Id::Bun; }")]
    );
}

#[test]
fn test_modules_are_skipped_and_the_rest_is_kept() {
    let text = "fn a() {}\n#[cfg(test)]\nmod tests {\n    ProviderId::Npm;\n}\nfn b() {}\n";
    assert_eq!(
        outside_test_modules(text),
        [(1, "fn a() {}"), (6, "fn b() {}")]
    );
}
