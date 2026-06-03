//! Man-page rendering for `runner man` (feature `man-gen`).

use std::io::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result};
use clap::{Command, CommandFactory as _};

use crate::cli::{Cli, RunAliasCli};

/// Write the top-level `runner` page to stdout as roff.
pub(crate) fn write_runner_page_to_stdout() -> Result<()> {
    let roff = render_command(Cli::command(), "runner");
    std::io::stdout()
        .write_all(&roff)
        .context("failed to write man page to stdout")
}

/// Render every page and write each as `<stem>.1` under `dir`.
pub(crate) fn write_man_pages(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    for (stem, roff) in man_pages() {
        let path = dir.join(format!("{stem}.1"));
        std::fs::write(&path, roff)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    Ok(())
}

const AUTHOR: &str = env!("RUNNER_AUTHOR_NAME");
const SOURCE: &str = concat!("runner ", env!("CARGO_PKG_VERSION"));

/// `(stem, roff)` for `runner`, `run`, and each visible subcommand
/// (`runner-<sub>`). Hidden subcommands are skipped.
#[must_use]
pub(crate) fn man_pages() -> Vec<(String, Vec<u8>)> {
    let runner = Cli::command();

    let mut pages = Vec::new();
    pages.push((
        "runner".to_string(),
        render_command(runner.clone(), "runner"),
    ));

    for sub in runner.get_subcommands() {
        // `man` itself only exists in featured builds; don't document it.
        if sub.is_hide_set() || sub.get_name() == "man" {
            continue;
        }
        let stem = format!("runner-{}", sub.get_name());
        pages.push((stem.clone(), render_command(sub.clone(), &stem)));
    }

    pages.push((
        "run".to_string(),
        render_command(RunAliasCli::command(), "run"),
    ));

    pages
}

fn render_command(cmd: Command, page_name: &str) -> Vec<u8> {
    let cmd = cmd.name(page_name.to_string()).author(AUTHOR);
    let man = clap_mangen::Man::new(cmd)
        .section("1")
        .manual("Runner Manual")
        .source(SOURCE);

    let mut raw = Vec::new();
    man.render(&mut raw)
        .expect("rendering to a Vec cannot fail");
    strip_ansi(&raw)
}

/// Drop ANSI escapes (CSI/OSC) that the CLI help strings bake in for color —
/// they have no place in roff and would corrupt the page otherwise.
fn strip_ansi(input: &[u8]) -> Vec<u8> {
    const ESC: u8 = 0x1b;
    const BEL: u8 = 0x07;

    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == ESC && i + 1 < input.len() {
            match input[i + 1] {
                // CSI: ESC [ … terminated by a byte in 0x40..=0x7e.
                b'[' => {
                    i += 2;
                    while i < input.len() && !(0x40..=0x7e).contains(&input[i]) {
                        i += 1;
                    }
                    i += 1; // consume the final byte
                    continue;
                }
                // OSC: ESC ] … terminated by BEL or ST (ESC \).
                b']' => {
                    i += 2;
                    while i < input.len() {
                        if input[i] == BEL {
                            i += 1;
                            break;
                        }
                        if input[i] == ESC && i + 1 < input.len() && input[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                    continue;
                }
                _ => {}
            }
        }
        out.push(input[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{man_pages, strip_ansi};

    #[test]
    fn strip_ansi_removes_csi_color_codes() {
        let input = b"\x1b[36mnpm\x1b[0m and \x1b[36mbun\x1b[0m";
        assert_eq!(strip_ansi(input), b"npm and bun");
    }

    #[test]
    fn strip_ansi_removes_osc8_hyperlinks() {
        // OSC-8 link: ESC ] 8 ; ; URL ST label ESC ] 8 ; ; ST
        let input = b"\x1b]8;;https://example.com\x1b\\label\x1b]8;;\x1b\\";
        assert_eq!(strip_ansi(input), b"label");
    }

    #[test]
    fn strip_ansi_leaves_plain_text_untouched() {
        let input = b".TH RUNNER 1\nplain roff\n";
        assert_eq!(strip_ansi(input), input);
    }

    #[test]
    fn man_pages_cover_both_binaries_and_skip_hidden() {
        let pages = man_pages();
        let names: Vec<&str> = pages.iter().map(|(n, _)| n.as_str()).collect();

        for expected in ["runner", "run", "runner-run", "runner-completions"] {
            assert!(
                names.contains(&expected),
                "missing {expected} page; got {names:?}"
            );
        }
        // `info` is `#[command(hide = true)]` — no page for it.
        assert!(
            !names.contains(&"runner-info"),
            "hidden `info` subcommand should not produce a page; got {names:?}"
        );
    }

    #[test]
    fn rendered_pages_are_clean_roff() {
        for (name, roff) in man_pages() {
            assert!(
                !roff.contains(&0x1b),
                "page {name} still contains an ANSI escape after stripping"
            );
            assert!(
                String::from_utf8_lossy(&roff).contains(".TH"),
                "page {name} should be roff with a .TH title header"
            );
        }
    }
}
