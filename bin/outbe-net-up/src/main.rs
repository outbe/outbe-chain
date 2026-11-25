mod cli;
mod network;

use clap::Parser;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    cli::Cli::parse().run().await
}
