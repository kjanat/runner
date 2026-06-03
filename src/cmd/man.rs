//! `runner man` — render roff man pages for the `runner` and `run` binaries.

use std::io::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result};
use clap::{Command, CommandFactory as _};

use crate::cli::{Cli, RunAliasCli};

/// Build-time author, shared with the help byline. Populates the man page
/// `AUTHORS` section.
const AUTHOR: &str = env!("RUNNER_AUTHOR_NAME");

/// Footer source line (`runner X.Y.Z`) shown at the bottom of every page.
const SOURCE: &str = concat!("runner ", env!("CARGO_PKG_VERSION"));

/// Render man pages for both CLIs.
///
/// With `output`, writes one `.1` file per page into that directory
/// (created if absent): `runner.1`, one `runner-<sub>.1` per visible
/// subcommand, and `run.1`. A confirmation line goes to stderr.
///
/// Without `output`, the top-level `runner` page is written to stdout as
/// roff — the only single-stream shape that makes sense, since a directory
/// of pages can't share one pipe. Pipe it through a pager to preview:
/// `runner man | man -l -`.
pub(crate) fn man(output: Option<&Path>) -> Result<()> {
    let Some(dir) = output else {
        return write_runner_page_to_stdout();
    };
    write_pages_to_dir(dir)
}

/// Write only the top-level `runner` page to stdout as roff.
fn write_runner_page_to_stdout() -> Result<()> {
    let roff = render_command(Cli::command(), "runner")?;
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    handle
        .write_all(&roff)
        .context("failed to write man page to stdout")
}

/// Render every page and write each as `<name>.1` under `dir`.
fn write_pages_to_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;

    let pages = generate_pages()?;
    for (name, roff) in &pages {
        let path = dir.join(format!("{name}.1"));
        std::fs::write(&path, roff)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }

    eprintln!("wrote {} man pages to {}", pages.len(), dir.display());
    Ok(())
}

/// Assemble the full page set: the top-level `runner` page, a page per
/// visible `runner` subcommand (`runner-<sub>`), and the `run` alias page.
///
/// Hidden subcommands (e.g. the deprecated `info`) are skipped; the
/// external-subcommand catch-all has no name and never appears in
/// [`Command::get_subcommands`].
fn generate_pages() -> Result<Vec<(String, Vec<u8>)>> {
    let runner = Cli::command();

    let mut pages = Vec::new();
    pages.push((
        "runner".to_string(),
        render_command(runner.clone(), "runner")?,
    ));

    for sub in runner.get_subcommands() {
        if sub.is_hide_set() {
            continue;
        }
        let page = format!("runner-{}", sub.get_name());
        pages.push((page.clone(), render_command(sub.clone(), &page)?));
    }

    pages.push((
        "run".to_string(),
        render_command(RunAliasCli::command(), "run")?,
    ));

    Ok(pages)
}

/// Render one clap [`Command`] to roff, titled `page_name`, in man section 1.
///
/// The rendered bytes are run through [`strip_ansi`] because the CLI's help
/// strings embed raw ANSI escapes for terminal coloring (see the `cyan!`
/// macro in [`crate::cli`]); `clap_mangen` copies that help text verbatim, so
/// the escapes would otherwise corrupt the roff output.
fn render_command(cmd: Command, page_name: &str) -> Result<Vec<u8>> {
    let cmd = cmd.name(page_name.to_string()).author(AUTHOR);
    let man = clap_mangen::Man::new(cmd)
        .section("1")
        .manual("Runner Manual")
        .source(SOURCE);

    let mut raw = Vec::new();
    man.render(&mut raw)
        .with_context(|| format!("failed to render man page for {page_name}"))?;
    Ok(strip_ansi(&raw))
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
    use std::fs;

    use super::{generate_pages, man, strip_ansi};
    use crate::tool::test_support::TempDir;

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
    fn generate_pages_includes_both_binaries() {
        let pages = generate_pages().expect("pages should render");
        let names: Vec<&str> = pages.iter().map(|(n, _)| n.as_str()).collect();

        assert!(
            names.contains(&"runner"),
            "missing runner page; got {names:?}"
        );
        assert!(names.contains(&"run"), "missing run page; got {names:?}");
        // Subcommand pages are namespaced under the parent.
        assert!(
            names.contains(&"runner-run"),
            "missing runner-run subcommand page; got {names:?}"
        );
        assert!(
            names.contains(&"runner-completions"),
            "missing runner-completions subcommand page; got {names:?}"
        );
    }

    #[test]
    fn generate_pages_skips_hidden_subcommands() {
        let pages = generate_pages().expect("pages should render");
        let names: Vec<&str> = pages.iter().map(|(n, _)| n.as_str()).collect();

        // `info` is `#[command(hide = true)]` — it must not get its own page.
        assert!(
            !names.contains(&"runner-info"),
            "hidden `info` subcommand should not produce a page; got {names:?}"
        );
    }

    #[test]
    fn rendered_pages_are_valid_roff_without_escapes() {
        let pages = generate_pages().expect("pages should render");
        for (name, roff) in &pages {
            assert!(
                !roff.contains(&0x1b),
                "page {name} still contains an ANSI escape after stripping"
            );
            let text = String::from_utf8_lossy(roff);
            assert!(
                text.starts_with(".ie") || text.starts_with(".TH") || text.contains(".TH"),
                "page {name} should be roff with a .TH title header"
            );
        }
    }

    #[test]
    fn man_writes_all_pages_to_dir() {
        let dir = TempDir::new("runner-man-output");

        man(Some(dir.path())).expect("man should write to a directory");

        for file in ["runner.1", "run.1", "runner-run.1"] {
            let path = dir.path().join(file);
            let body = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{file} should be readable: {e}"));
            assert!(
                body.contains(".TH"),
                "{file} should contain a .TH roff header"
            );
        }
    }

    #[test]
    fn man_creates_missing_output_dir() {
        let dir = TempDir::new("runner-man-mkdir");
        let nested = dir.path().join("share").join("man").join("man1");

        man(Some(&nested)).expect("man should create the output directory");

        assert!(
            nested.join("runner.1").is_file(),
            "runner.1 should exist in created dir"
        );
    }
}
