//! Command-line front end for the generator.
#![forbid(unsafe_code)]

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::Parser;
use rift_gen::{Config, Kind, Profile};

/// Generate a pair of files with a known mutation manifest.
#[derive(Debug, Parser)]
#[command(name = "rift-gen")]
struct Args {
    /// Seed for every random decision.
    #[arg(long, default_value_t = 1)]
    seed: u64,
    /// Target size of A: bytes, or with a K, M or G suffix (binary units).
    #[arg(long, default_value = "1M")]
    size: String,
    /// Line content.
    #[arg(long, value_enum, default_value_t = Profile::Sql)]
    profile: Profile,
    /// Probability that a mutation starts at a given line.
    #[arg(long, default_value_t = 0.001)]
    change_ratio: f64,
    /// Restrict mutations to these kinds.
    #[arg(long, value_enum, value_delimiter = ',')]
    kinds: Option<Vec<Kind>>,
    /// Output directory; receives `a`, `b` and `manifest.tsv`.
    #[arg(long, default_value = ".")]
    out: PathBuf,
}

fn parse_size(s: &str) -> anyhow::Result<u64> {
    let (num, mult) = match s.as_bytes().last() {
        Some(b'K' | b'k') => (&s[..s.len() - 1], 1u64 << 10),
        Some(b'M' | b'm') => (&s[..s.len() - 1], 1 << 20),
        Some(b'G' | b'g') => (&s[..s.len() - 1], 1 << 30),
        _ => (s, 1),
    };
    let n: u64 = num.parse().with_context(|| format!("bad size {s:?}"))?;
    match n.checked_mul(mult) {
        Some(v) => Ok(v),
        None => bail!("size {s:?} overflows"),
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut config = Config::new(args.seed, parse_size(&args.size)?, args.profile);
    config.change_ratio = args.change_ratio;
    if let Some(kinds) = args.kinds {
        config.kinds = kinds;
    }
    std::fs::create_dir_all(&args.out)?;
    let open = |name: &str| -> anyhow::Result<BufWriter<File>> {
        let path = args.out.join(name);
        let file = File::create(&path).with_context(|| format!("create {}", path.display()))?;
        Ok(BufWriter::with_capacity(1 << 20, file))
    };
    rift_gen::generate(&config, open("a")?, open("b")?, open("manifest.tsv")?)?;
    Ok(())
}
