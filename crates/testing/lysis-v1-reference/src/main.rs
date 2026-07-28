use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use eyre::Result;
use outbe_lysis_v1_reference::{
    canonical_json, check, default_manifest_path, default_vectors_path, render, write,
};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Mode {
    Check,
    Render,
    Write,
}

#[derive(Debug, Parser)]
#[command(about = "Independent Rust reference for bounded Lysis Program V1")]
struct Cli {
    #[arg(long, value_enum)]
    mode: Mode,
    #[arg(long)]
    cases: Option<PathBuf>,
    #[arg(long)]
    manifest: Option<PathBuf>,
}

fn main() -> Result<()> {
    let arguments = Cli::parse();
    let cases = arguments.cases.unwrap_or_else(default_vectors_path);
    match arguments.mode {
        Mode::Check => {
            let manifest = arguments.manifest.unwrap_or_else(default_manifest_path);
            let report = check(&cases, &manifest)?;
            println!("{}", String::from_utf8(canonical_json(&report)?)?);
            if report.status != "PASS" {
                std::process::exit(1);
            }
        }
        Mode::Render => {
            for line in render(&cases)? {
                println!("{}", String::from_utf8(line)?);
            }
        }
        Mode::Write => write(&cases)?,
    }
    Ok(())
}
