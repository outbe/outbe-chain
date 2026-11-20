//! Read-only TEE registry, policy, upgrade and local enclave health over RPC.

use clap::Subcommand;
use eyre::Result;

use crate::rpc::Rpc;

mod args;
mod rpc;
mod status_view;

use args::StatusArgs;

#[derive(Subcommand)]
pub enum TeeCmd {
    /// Read finalized registry, policy and upgrade state, plus node-reported enclave health.
    /// No enclave connection, signing key or lifecycle journal is required.
    #[command(visible_alias = "upgrade-status")]
    Status(StatusArgs),
}

impl TeeCmd {
    pub async fn run(self, client: &(impl Rpc + Sync)) -> Result<()> {
        match self {
            Self::Status(args) => {
                let status = status_view::read_status(client, &args).await?;
                println!("{}", serde_json::to_string_pretty(&status)?);
                Ok(())
            }
        }
    }
}
