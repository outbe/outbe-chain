//! Host-local Outbe validator lifecycle orchestration.

use clap::Parser;
use eyre::Result;

mod cli;
mod disk;
mod host;
mod image;
mod network;
mod vm;

#[tokio::main]
async fn main() -> Result<()> {
    cli::Cli::parse().run().await
}
