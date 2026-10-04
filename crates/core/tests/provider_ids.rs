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

#[test]
fn the_core_never_names_a_provider_outside_its_tests() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    for path in sources(&src) {
        let text = std::fs::read_to_string(&path).expect("source reads");
        for (number, line) in outside_test_modules(&text) {
            if line.contains("ProviderId::") {
                hits.push(format!("{}:{number}: {}", path.display(), line.trim()));
            }
        }
    }
    assert!(hits.is_empty(), "{}", hits.join("\n"));
}

#[test]
fn test_modules_are_skipped_and_the_rest_is_kept() {
    let text = "fn a() {}\n#[cfg(test)]\nmod tests {\n    ProviderId::Npm;\n}\nfn b() {}\n";
    assert_eq!(
        outside_test_modules(text),
        [(1, "fn a() {}"), (6, "fn b() {}")]
    );
}
