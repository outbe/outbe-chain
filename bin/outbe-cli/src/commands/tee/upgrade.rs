use super::ensure_signer_matches_node_id;
use super::load_secp256k1_key_file;
use super::parse_nonzero_b256;
use super::sign_node_hash;
use super::CliFinalityRpc;

use crate::rpc::Rpc;

use alloy_primitives::U256;

use eyre::Result;

use outbe_operator::tee::copy_same_platform_sealed_root_and_checkpoint_v1;
use outbe_operator::tee::inspect_upgrade_journal_v1;
use outbe_operator::tee::prepare_upgrade_journal_v1;

use outbe_operator::tee::read_finalized_staged_successor_policy_v1;

use outbe_operator::tee::run_upgrade_submission_v1;

use outbe_operator::tee::NodeBindingSelectorV1;

use outbe_operator::tee::UpgradeContextV1;

use outbe_operator::tx::RelaySignerV1;

use outbe_tee::load_committed_enclave_manifest_v1;

use outbe_tee::NodeHostIdentityV1;
use outbe_tee::ReplacementCandidateEnclaveV1;

#[allow(clippy::too_many_arguments)]
pub(super) async fn upgrade_prepare(
    client: &(impl Rpc + Sync),
    candidate_enclave_socket: &str,
    node_data_dir: &std::path::Path,
    active_tee_dir: &std::path::Path,
    candidate_tee_dir: &std::path::Path,
    reth_p2p_secret_key: Option<&std::path::Path>,
) -> Result<()> {
    let active = load_committed_enclave_manifest_v1(node_data_dir)
        .map_err(|error| eyre::eyre!("load committed NodeHost manifest: {error}"))?;
    let rpc_chain_id = client.eth_chain_id().await?;
    let (candidate, _, _) = connect_upgrade_candidate_v1(
        candidate_enclave_socket,
        node_data_dir,
        &active,
        rpc_chain_id,
        reth_p2p_secret_key,
    )?;
    let staged = read_finalized_staged_successor_policy_v1(&CliFinalityRpc(client))
        .await?
        .ok_or_else(|| eyre::eyre!("no successor TEE policy is staged at finalized state"))?;
    let successor_policy_hash = staged
        .policy
        .policy_hash()
        .map_err(|error| eyre::eyre!("hash finalized staged successor policy: {error}"))?;
    let predecessor_manifest_hash = active
        .authorization_hash()
        .map_err(|error| eyre::eyre!("hash active NodeHost manifest: {error}"))?;
    let candidate_manifest_hash = candidate
        .manifest()
        .authorization_hash()
        .map_err(|error| eyre::eyre!("hash candidate NodeHost manifest: {error}"))?;
    let snapshot = prepare_upgrade_journal_v1(
        node_data_dir,
        UpgradeContextV1 {
            predecessor_manifest_hash,
            candidate_manifest_hash,
            successor_policy_hash,
            activation_height: staged.policy.activation_height,
            active_tee_dir: active_tee_dir.to_path_buf(),
            candidate_tee_dir: candidate_tee_dir.to_path_buf(),
        },
    )?;
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    println!(
        "next: stop candidate B, run `outbe-cli tee upgrade-copy-root --node-data-dir {}`, then restart B",
        node_data_dir.display()
    );
    Ok(())
}

pub(super) fn upgrade_copy_root(node_data_dir: &std::path::Path) -> Result<()> {
    let snapshot = copy_same_platform_sealed_root_and_checkpoint_v1(node_data_dir)?;
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    println!("next: restart candidate B, then run `outbe-cli tee upgrade-submit ...`");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn upgrade_submit(
    client: &(impl Rpc + Sync),
    private_key: Option<&str>,
    candidate_enclave_socket: &str,
    node_data_dir: &std::path::Path,
    reth_p2p_secret_key: Option<&std::path::Path>,
    binding_id: &str,
    valid_until: u64,
) -> Result<()> {
    let private_key = private_key.ok_or_else(|| {
        eyre::eyre!("tee upgrade-submit requires the global --private-key EVM signer")
    })?;
    let evm_signer = RelaySignerV1::new(private_key)?;
    let active = load_committed_enclave_manifest_v1(node_data_dir)
        .map_err(|error| eyre::eyre!("load committed NodeHost manifest: {error}"))?;
    let rpc_chain_id = client.eth_chain_id().await?;
    let (mut candidate, node_signing_key, selector) = connect_upgrade_candidate_v1(
        candidate_enclave_socket,
        node_data_dir,
        &active,
        rpc_chain_id,
        reth_p2p_secret_key,
    )?;
    let signer = |hash| {
        sign_node_hash(&node_signing_key, hash)
            .map_err(|error| eyre::eyre!("node authority signing failed: {error}"))
    };
    let outcome = run_upgrade_submission_v1(
        &CliFinalityRpc(client),
        &evm_signer,
        &mut candidate,
        &signer,
        node_data_dir,
        &selector,
        parse_nonzero_b256(binding_id, "--binding-id")?,
        valid_until,
    )
    .await?;
    println!("{outcome:#?}");
    println!(
        "the running node will promote B only after the exact transition binding is locally finalized"
    );
    Ok(())
}

pub(super) fn upgrade_status(node_data_dir: &std::path::Path) -> Result<()> {
    match inspect_upgrade_journal_v1(node_data_dir)? {
        Some(snapshot) => println!("{}", serde_json::to_string_pretty(&snapshot)?),
        None => println!("no same-platform enclave upgrade is journaled"),
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn connect_upgrade_candidate_v1(
    candidate_enclave_socket: &str,
    node_data_dir: &std::path::Path,
    active: &outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1,
    rpc_chain_id: u64,
    reth_p2p_secret_key: Option<&std::path::Path>,
) -> Result<(
    ReplacementCandidateEnclaveV1,
    k256::ecdsa::SigningKey,
    NodeBindingSelectorV1,
)> {
    if active.chain_id != U256::from(rpc_chain_id).to_be_bytes() {
        eyre::bail!("committed NodeHost manifest chain id does not match eth_chainId");
    }
    let path = reth_p2p_secret_key
        .ok_or_else(|| eyre::eyre!("NodeHost upgrade requires --reth-p2p-secret-key"))?;
    let signing = load_secp256k1_key_file(path)?;
    ensure_signer_matches_node_id(&signing, &active.node_id)?;
    let candidate = outbe_tee::prepare_node_host_enclave_replacement_candidate(
        candidate_enclave_socket,
        node_data_dir,
        NodeHostIdentityV1 {
            network_binding: active.network_binding(),
            reth_p2p_public: active.node_id.reth_p2p_public,
        },
        |hash| sign_node_hash(&signing, hash),
    )
    .map_err(|error| eyre::eyre!("prepare NodeHost candidate B: {error}"))?;
    Ok((
        candidate,
        signing,
        NodeBindingSelectorV1::NodeHost(active.node_id.reth_p2p_public),
    ))
}
