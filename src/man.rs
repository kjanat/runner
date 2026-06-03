//! Man-page rendering (feature `man-gen`).
//!
//! Renders roff man pages from the same clap [`Command`] tree the CLI is
//! built from. Gated behind the `man-gen` feature and consumed only by the
//! `gen-man` example (`cargo run --example gen-man --features man-gen`),
//! which writes the committed pages under `man/`. Kept out of the default
//! build so `clap_mangen` never lands in the shipped binary.

use clap::{Command, CommandFactory as _};

use crate::cli::{Cli, RunAliasCli};

/// Build-time author, shared with the help byline. Populates each page's
/// `AUTHORS` section.
const AUTHOR: &str = env!("RUNNER_AUTHOR_NAME");

/// Footer source line (`runner X.Y.Z`) shown at the bottom of every page.
const SOURCE: &str = concat!("runner ", env!("CARGO_PKG_VERSION"));

/// Render the full page set: the top-level `runner` page, a page per visible
/// `runner` subcommand (`runner-<sub>`), and the `run` alias page.
///
/// Each entry is `(stem, roff)` — the example writes it as `<stem>.1`.
/// Hidden subcommands (e.g. the deprecated `info`) are skipped; the
/// external-subcommand catch-all has no name and never appears in
/// [`Command::get_subcommands`].
#[must_use]
pub(crate) fn man_pages() -> Vec<(String, Vec<u8>)> {
    let runner = Cli::command();

    let mut pages = Vec::new();
    pages.push((
        "runner".to_string(),
        render_command(runner.clone(), "runner"),
    ));

    for sub in runner.get_subcommands() {
        if sub.is_hide_set() {
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

/// Render one clap [`Command`] to roff, titled `page_name`, in man section 1.
///
/// The rendered bytes are run through [`strip_ansi`] because the CLI's help
/// strings embed raw ANSI escapes for terminal coloring (see the `cyan!`
/// macro in [`crate::cli`]); `clap_mangen` copies that help text verbatim,
/// so the escapes would otherwise corrupt the roff output.
fn render_command(cmd: Command, page_name: &str) -> Vec<u8> {
    let cmd = cmd.name(page_name.to_string()).author(AUTHOR);
    let man = clap_mangen::Man::new(cmd)
        .section("1")
        .manual("Runner Manual")
        .source(SOURCE);

    let mut raw = Vec::new();
    // Writing to a `Vec` is infallible — the only error path in `render` is
    // the underlying `io::Write`, which never fails for an in-memory buffer.
    man.render(&mut raw)
        .expect("rendering man page to a Vec cannot fail");
    strip_ansi(&raw)
}

/// Strip ANSI control sequences (CSI `ESC [ … <final>` and OSC
/// `ESC ] … BEL|ST`) from a byte stream, leaving all other bytes intact.
///
/// roff is plain text — a CSI/OSC sequence never legitimately appears in it
/// — so removing them is lossless for our purpose and rescues man output
/// from the color escapes baked into the CLI help strings.
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
