//! Command-line front end for the generator.
#![forbid(unsafe_code)]

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use rift_core::Bytes;
use rift_gen::{Config, Kind, Profile};

/// Generate a pair of files with a known mutation manifest.
#[derive(Debug, Parser)]
#[command(name = "rift-gen")]
struct Args {
    /// Seed for every random decision.
    #[arg(long, default_value_t = 1)]
    seed: u64,
    /// Target size of A: bytes, or with a K, M, G or T unit (binary, optional iB).
    #[arg(long, default_value = "1M")]
    size: Bytes,
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

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut config = Config::new(args.seed, args.size.0, args.profile);
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
