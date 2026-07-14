//! probatum — Don't trust the promise. Run the proof.
//!
//! v0: `probatum run <manifest.yaml> [--json] [--seed N]`

mod capture;
mod diagnose;
mod http;
mod manifest;
mod runner;
mod verdict;

use anyhow::{bail, Result};
use std::path::PathBuf;

fn main() {
    let code = match real_main() {
        Ok(violated) => {
            if violated {
                1
            } else {
                0
            }
        }
        Err(e) => {
            eprintln!("probatum: {e:#}");
            2
        }
    };
    std::process::exit(code);
}

fn real_main() -> Result<bool> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut json = false;
    let mut seed: Option<u32> = None;
    let mut positional: Vec<String> = Vec::new();

    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => json = true,
            "--seed" => {
                let v = it.next().unwrap_or_default();
                seed = Some(v.parse().map_err(|_| anyhow::anyhow!("--seed attend un entier"))?);
            }
            _ => positional.push(a),
        }
    }

    match positional.first().map(String::as_str) {
        Some("run") => {}
        _ => bail!("usage: probatum run <manifest.yaml> [--json] [--seed N]"),
    }
    let Some(path) = positional.get(1) else {
        bail!("usage: probatum run <manifest.yaml> [--json] [--seed N]");
    };
    let path = PathBuf::from(path);

    let m = manifest::load(&path)?;
    let seed = seed.unwrap_or_else(random_seed);
    let report = runner::run(&m, &path, seed)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        verdict::print(&report);
    }
    Ok(report.verdict == "violated")
}

/// Seed from /dev/urandom — recorded in the evidence so every run is replayable
/// by reference even before the seed drives any randomness (v0).
fn random_seed() -> u32 {
    use std::io::Read;
    let mut buf = [0u8; 4];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok()
    {
        u32::from_le_bytes(buf)
    } else {
        0xC0FFEE
    }
}
