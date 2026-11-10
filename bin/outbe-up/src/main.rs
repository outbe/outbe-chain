//! Host-local Outbe validator lifecycle orchestration.

use clap::Parser;
use eyre::Result;

mod cli;
mod support;
mod validator;

#[tokio::main]
async fn main() -> Result<()> {
    cli::Cli::parse().run().await
}
