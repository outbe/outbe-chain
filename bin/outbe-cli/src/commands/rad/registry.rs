//! Commands for the on-chain public Radicle repository registry (`outbe-cli rad ...`).

use alloy_primitives::{FixedBytes, U256};
use alloy_sol_types::SolCall;
use clap::Subcommand;
use eyre::{Result, WrapErr};
use outbe_primitives::addresses::CODE_REGISTRY_ADDRESS;

use crate::{abi::ICodeRegistry, commands::require_signer, errors::RadError, rpc::Rpc};

pub(super) type RepoId = FixedBytes<20>;

fn parse_repo_id(value: &str) -> std::result::Result<RepoId, RadError> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    let bytes = hex::decode(value).map_err(RadError::InvalidRepoIdHex)?;
    if bytes.len() != 20 {
        return Err(RadError::InvalidRepoIdLength(bytes.len()));
    }
    Ok(RepoId::from_slice(&bytes))
}

#[derive(Subcommand)]
pub enum RadRegistryCmd {
    /// Register a public Heartwood RepoId. Registration becomes active after finality.
    RegisterRepository {
        #[arg(long, value_parser = parse_repo_id)]
        repo_id: RepoId,
    },
    /// Enumerate every registered public RepoId.
    Repositories,
    /// Show the EVM account that registered a RepoId, or zero when absent.
    Repository {
        #[arg(long, value_parser = parse_repo_id)]
        repo_id: RepoId,
    },
}

impl RadRegistryCmd {
    pub async fn run(self, client: &(impl Rpc + Sync), private_key: Option<&str>) -> Result<()> {
        match self {
            Self::RegisterRepository { repo_id } => {
                let signer = require_signer(private_key)?;
                let call = ICodeRegistry::registerRepositoryCall { repoId: repo_id };
                let tx = signer
                    .send_tx(client, CODE_REGISTRY_ADDRESS, call.abi_encode(), U256::ZERO)
                    .await?;
                println!("Radicle repository registration transaction sent: {tx}");
                Ok(())
            }
            Self::Repositories => list(client).await,
            Self::Repository { repo_id } => show(client, repo_id).await,
        }
    }
}

async fn list(client: &(impl Rpc + Sync)) -> Result<()> {
    let output = client
        .eth_call(
            CODE_REGISTRY_ADDRESS,
            &ICodeRegistry::repositoryCountCall {}.abi_encode(),
        )
        .await
        .wrap_err("Radicle repositoryCount eth_call failed")?;
    let count = ICodeRegistry::repositoryCountCall::abi_decode_returns(&output)
        .wrap_err("decode Radicle repository count")?;
    for index in 0..count {
        let output = client
            .eth_call(
                CODE_REGISTRY_ADDRESS,
                &ICodeRegistry::repositoryAtCall { index }.abi_encode(),
            )
            .await
            .wrap_err_with(|| format!("Radicle repositoryAt({index}) eth_call failed"))?;
        let repo_id = ICodeRegistry::repositoryAtCall::abi_decode_returns(&output)
            .wrap_err("decode Radicle RepoId")?;
        println!("{index}: 0x{}", hex::encode(repo_id));
    }
    Ok(())
}

async fn show(client: &(impl Rpc + Sync), repo_id: RepoId) -> Result<()> {
    let output = client
        .eth_call(
            CODE_REGISTRY_ADDRESS,
            &ICodeRegistry::repositoryRegistrantCall { repoId: repo_id }.abi_encode(),
        )
        .await
        .wrap_err("Radicle repositoryRegistrant eth_call failed")?;
    let registrant = ICodeRegistry::repositoryRegistrantCall::abi_decode_returns(&output)
        .wrap_err("decode Radicle repository registrant")?;
    println!("RepoId: 0x{}", hex::encode(repo_id));
    println!("Registrant: {registrant}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{rpc::mock::recording_send_tx_rpc, Cli};
    use clap::Parser;

    const PRIVATE_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

    #[test]
    fn parses_registry_commands_under_rad_and_rejects_wrong_length() {
        let id = "11".repeat(20);
        assert!(Cli::try_parse_from(
            ["outbe-cli", "rad", "register-repository", "--repo-id", &id,]
        )
        .is_ok());
        assert!(Cli::try_parse_from(["outbe-cli", "rad", "repositories"]).is_ok());
        assert!(Cli::try_parse_from(["outbe-cli", "rad", "repository", "--repo-id", &id]).is_ok());
        assert!(
            Cli::try_parse_from(["outbe-cli", "rad", "repository", "--repo-id", "11"]).is_err()
        );
    }

    #[tokio::test]
    async fn register_sends_the_exact_precompile_call() {
        let repo_id = RepoId::repeat_byte(0x44);
        let call = ICodeRegistry::registerRepositoryCall { repoId: repo_id };
        let rpc = recording_send_tx_rpc(
            PRIVATE_KEY,
            CODE_REGISTRY_ADDRESS,
            call.abi_encode(),
            U256::ZERO,
        )
        .unwrap();
        RadRegistryCmd::RegisterRepository { repo_id }
            .run(&rpc, Some(PRIVATE_KEY))
            .await
            .unwrap();
    }
}
