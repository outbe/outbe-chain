use super::ensure_signer_matches_node_id;
use super::join::{finalized_binding_matches_intent, persist_authorized_join_admission_anchor_v1};
use super::load_secp256k1_key_file;
use super::parse_nonzero_b256;
use super::sign_node_hash;
use super::CliFinalityRpc;
const MANUAL_RENEWAL_RECONCILE_INTERVAL: Duration = Duration::from_secs(2);
use outbe_operator::tee::{
    read_finalized_registry_view_v1, record_upgrade_finalized_v1, record_upgrade_promoted_v1,
    UpgradeJournalStateV1,
};
use outbe_primitives::tee_attestation_v1::AttestationEvidenceV1;
use outbe_tee::{
    construct_finalized_replacement_authorization_v1, load_replacement_candidate_submission,
    promote_replacement_candidate, FinalizedReplacementBindingV1,
};
use std::time::{Duration, Instant};

use crate::rpc::Rpc;

use alloy_primitives::U256;

use eyre::Result;

use outbe_operator::tee::copy_same_platform_sealed_root_and_checkpoint_v1;
use outbe_operator::tee::inspect_upgrade_journal_v1;
use outbe_operator::tee::prepare_upgrade_journal_v1;

use outbe_operator::tee::read_finalized_upgrade_policy_v1;

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
    let staged = read_finalized_upgrade_policy_v1(&CliFinalityRpc(client))
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
        "next: run `outbe-cli tee upgrade-provision --node-data-dir {} ...` to obtain the network key",
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
pub(super) fn connect_upgrade_candidate_v1(
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

/// Like `tee join`, this records an RPC-finalized anchor for local certified
/// catch-up. It does not confer validator signing authority or unjail a node.
pub(super) async fn upgrade_finalize(
    client: &(impl Rpc + Sync),
    node_data_dir: &std::path::Path,
    timeout: Duration,
) -> Result<()> {
    let checkpoint = inspect_upgrade_journal_v1(node_data_dir)?
        .ok_or_else(|| eyre::eyre!("no upgrade is prepared"))?;
    let context = checkpoint.lifecycle.context().clone();
    let committed = load_committed_enclave_manifest_v1(node_data_dir)?;
    if committed
        .authorization_hash()
        .map_err(|e| eyre::eyre!("invalid manifest: {e}"))?
        == context.candidate_manifest_hash
    {
        match checkpoint.lifecycle {
            UpgradeJournalStateV1::Finalized { .. } => {
                record_upgrade_promoted_v1(node_data_dir)?;
            }
            UpgradeJournalStateV1::Promoted { .. } => {}
            _ => eyre::bail!("committed candidate has no finalized upgrade checkpoint"),
        }
        println!("upgrade is already promoted; start the successor enclave and complete certified catch-up");
        return Ok(());
    }
    if !matches!(
        checkpoint.lifecycle,
        UpgradeJournalStateV1::Submitted { .. } | UpgradeJournalStateV1::Finalized { .. }
    ) {
        eyre::bail!("upgrade-finalize requires a submitted transition");
    }
    let submission = load_replacement_candidate_submission(node_data_dir)?
        .ok_or_else(|| eyre::eyre!("durable transition evidence is missing"))?;
    let evidence = AttestationEvidenceV1::decode_canonical(submission.evidence())
        .map_err(|e| eyre::eyre!("invalid durable transition evidence: {e}"))?;
    let intent = evidence.intent();
    let selector = NodeBindingSelectorV1::NodeHost(committed.node_id.reth_p2p_public);
    let started = Instant::now();
    let exact = loop {
        let view = read_finalized_registry_view_v1(&CliFinalityRpc(client), &selector).await?;
        if let Some(binding) = &view.binding {
            if finalized_binding_matches_intent(binding, intent)? {
                break view;
            }
        }
        if started.elapsed() >= timeout {
            eyre::bail!("transition is not finalized; checkpoint retained for retry");
        }
        tokio::time::sleep(MANUAL_RENEWAL_RECONCILE_INTERVAL).await;
    };
    let binding = exact.binding.as_ref().expect("matching finalized binding");
    let finalized = FinalizedReplacementBindingV1 {
        view: exact.view.clone(),
        node_id_hash: binding.node_id_hash,
        enclave_id: binding.enclave_id,
        binding_id: binding.binding_id,
        intent_hash: binding.intent_hash,
        binding_version: binding.binding_version,
        registration_version: binding.registration_version,
        valid_until: binding.valid_until,
        recipient_x25519: binding.recipient_x25519.into(),
        attestation_ed25519: binding.attestation_ed25519.into(),
        noise_responder_x25519: binding.noise_responder_x25519.into(),
        node_host_authorization_hash: binding.node_host_authorization_hash,
    };
    let authorization =
        construct_finalized_replacement_authorization_v1(node_data_dir, &finalized)?;
    persist_authorized_join_admission_anchor_v1(
        node_data_dir,
        &exact,
        binding.node_id_hash,
        binding.enclave_id,
        binding.intent_hash,
    )?;
    if matches!(
        checkpoint.lifecycle,
        UpgradeJournalStateV1::Submitted { .. }
    ) {
        record_upgrade_finalized_v1(
            node_data_dir,
            exact.view.block_number,
            exact.view.block_hash,
        )?;
    }
    promote_replacement_candidate(node_data_dir, &authorization)?;
    record_upgrade_promoted_v1(node_data_dir)?;
    println!("successor enclave promoted at finalized height {}; complete certified follower catch-up before restarting validator authority", exact.view.block_number);
    println!("a jailed validator still requires normal unjail after its stake and cooldown requirements are met");
    Ok(())
}
