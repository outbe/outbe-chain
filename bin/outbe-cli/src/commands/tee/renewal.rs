use super::ensure_signer_matches_node_id;
use super::load_secp256k1_key_file;
use super::sign_node_hash;
use super::CliFinalityRpc;

use crate::rpc::Rpc;

use alloy_primitives::U256;

use eyre::Result;

use outbe_operator::tee::read_renewal_status_v1;
use outbe_operator::tee::run_renewal_once_v1;

use outbe_operator::tee::NodeBindingSelectorV1;

use outbe_operator::tee::RenewalOutcomeV1;
use outbe_operator::tee::RenewalServiceConfigV1;

use outbe_operator::tx::RelaySignerV1;

use outbe_tee::load_committed_enclave_manifest_v1;

use outbe_tee::NodeHostIdentityV1;

use std::time::Duration;
use std::time::Instant;

const MANUAL_RENEWAL_FINALITY_TIMEOUT: Duration = Duration::from_secs(300);

const MANUAL_RENEWAL_RECONCILE_INTERVAL: Duration = Duration::from_secs(2);

pub(super) async fn renew(
    client: &(impl Rpc + Sync),
    private_key: Option<&str>,
    enclave_socket: &str,
    node_data_dir: &std::path::Path,
    reth_p2p_secret_key: Option<&std::path::Path>,
) -> Result<()> {
    let private_key = private_key
        .ok_or_else(|| eyre::eyre!("tee renew requires the global --private-key EVM signer"))?;
    let evm_signer = RelaySignerV1::new(private_key)?;
    let manifest = load_committed_enclave_manifest_v1(node_data_dir)
        .map_err(|error| eyre::eyre!("load committed NodeHost manifest: {error}"))?;
    let rpc_chain_id = client.eth_chain_id().await?;
    if manifest.chain_id != U256::from(rpc_chain_id).to_be_bytes() {
        eyre::bail!("committed NodeHost manifest chain id does not match eth_chainId");
    }
    let path = reth_p2p_secret_key
        .ok_or_else(|| eyre::eyre!("NodeHost renewal requires --reth-p2p-secret-key"))?;
    let node_signing_key = load_secp256k1_key_file(path)?;
    ensure_signer_matches_node_id(&node_signing_key, &manifest.node_id)?;
    let mut enclave = outbe_tee::connect_or_initialize_node_host_enclave(
        enclave_socket,
        node_data_dir,
        NodeHostIdentityV1 {
            network_binding: manifest.network_binding(),
            reth_p2p_public: manifest.node_id.reth_p2p_public,
        },
        |hash| sign_node_hash(&node_signing_key, hash),
    )
    .map_err(|error| eyre::eyre!("connect NodeHost enclave: {error}"))?;
    let selector = NodeBindingSelectorV1::NodeHost(manifest.node_id.reth_p2p_public);
    let config = RenewalServiceConfigV1 {
        node_data_dir: node_data_dir.to_path_buf(),
        selector,
        manifest,
    };
    let signer = |hash| {
        sign_node_hash(&node_signing_key, hash)
            .map_err(|error| eyre::eyre!("node authority signing failed: {error}"))
    };
    let started = Instant::now();
    loop {
        let outcome = run_renewal_once_v1(
            &CliFinalityRpc(client),
            &evm_signer,
            &mut enclave,
            &signer,
            &config,
        )
        .await?;
        match &outcome {
            RenewalOutcomeV1::Finalized { .. } | RenewalOutcomeV1::NotDue { .. } => {
                println!("{outcome:#?}");
                return Ok(());
            }
            RenewalOutcomeV1::Submitted { .. } | RenewalOutcomeV1::Abandoned { .. } => {
                if started.elapsed() >= MANUAL_RENEWAL_FINALITY_TIMEOUT {
                    return Err(eyre::eyre!(
                        "tee renew timed out waiting for canonical finality; last outcome: {outcome:#?}"
                    ));
                }
                tokio::time::sleep(MANUAL_RENEWAL_RECONCILE_INTERVAL).await;
            }
        }
    }
}

pub(super) async fn renewal_status(
    client: &(impl Rpc + Sync),
    node_data_dir: &std::path::Path,
    warning_blocks: u64,
    critical_blocks: u64,
) -> Result<()> {
    let manifest = load_committed_enclave_manifest_v1(node_data_dir)
        .map_err(|error| eyre::eyre!("load committed NodeHost manifest: {error}"))?;
    let selector = NodeBindingSelectorV1::NodeHost(manifest.node_id.reth_p2p_public);
    let status = read_renewal_status_v1(
        &CliFinalityRpc(client),
        node_data_dir,
        &selector,
        warning_blocks,
        critical_blocks,
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&status)?);
    Ok(())
}
