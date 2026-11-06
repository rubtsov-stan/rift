//! Developer tasks. `cargo xtask man` regenerates `man/rift.1`.

use std::path::{Path, PathBuf};

use clap::CommandFactory;

/// The description, which clap reduces to one line.
const HEAD: &str = include_str!("man_head.roff");

/// The sections clap cannot derive: modes, exit status, the memory
/// contract and examples.
const TAIL: &str = include_str!("man_tail.roff");

/// The full text of `rift.1`.
fn render() -> anyhow::Result<Vec<u8>> {
    let man = clap_mangen::Man::new(rift_diff::Args::command());
    let mut out = Vec::new();
    man.render_title(&mut out)?;
    man.render_name_section(&mut out)?;
    man.render_synopsis_section(&mut out)?;
    out.extend_from_slice(HEAD.as_bytes());
    man.render_options_section(&mut out)?;
    out.extend_from_slice(TAIL.as_bytes());
    Ok(out)
}

/// Where the page is committed, so release archives can include it.
fn man_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../man/rift.1")
}

fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("man") => {
            std::fs::write(man_path(), render()?)?;
            Ok(())
        }
        _ => anyhow::bail!("usage: cargo xtask man"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A changed option or version fails here until `cargo xtask man` is
    /// run, so the page shipped in release archives matches the binary.
    #[test]
    fn committed_page_is_current() {
        let committed = std::fs::read(man_path()).expect("man/rift.1 exists");
        assert!(
            committed == render().unwrap(),
            "man/rift.1 is stale: run `cargo xtask man`"
        );
    }
}
