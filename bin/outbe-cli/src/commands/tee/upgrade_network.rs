//! Network provisioning of a staged enclave; never copies a predecessor seal.
use super::admission_history;
use super::args::{UpgradeCandidateArgs, UpgradeProvisionArgs};
use super::join::{
    classify_join_offer_key_state_transport, load_finalized_admission_anchor_v1, JoinOfferKeyState,
};
use super::rpc::{json_b256_field, json_hex_array, json_hex_u256_field, json_hex_u64_field};
use super::upgrade::connect_upgrade_candidate_v1;
use super::*;
use alloy_consensus::BlockHeader as _;
use alloy_primitives::{keccak256, B256, U256};
use alloy_sol_types::{SolCall, SolValue};
use eyre::WrapErr;
use outbe_operator::tee::{
    generate_transition_evidence_v1, read_finalized_bound_renewal_view_v1,
    record_network_key_provisioned_v1, transition_intent_v1, NetworkUpgradeSubmissionV1,
    UpgradeJournalGuardV1,
};
use outbe_operator::tee::{read_finalized_upgrade_policy_v1, UpgradeJournalStateV1};
use outbe_operator::tx::{buffered_gas_price, RelaySignerV1};
use outbe_primitives::addresses::TEE_REGISTRY_ADDRESS;
use outbe_primitives::reshare_artifact::{decode_outbe_block_artifacts, ConsensusHeaderArtifact};
use outbe_primitives::tee_attestation_v1::{
    AttestationEvidenceV1, AttestationOperationV1, RegistryMutatorV1, TeeRegistryGasScheduleV1,
};
use outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1;
use outbe_tee::dcap_protocol::{DcapOnboardingArtifactV1, DcapOnboardingContextV1};
use outbe_tee::finalized_admission::{
    FinalizedAdmissionWitnessV1, MptAccountProofV1, MptStorageProofV1,
};
use outbe_tee::load_committed_enclave_manifest_v1;
use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};
use outbe_tee::upgrade_transfer::UpgradeKeyProofV1;
use std::path::Path;
use std::time::{Duration, Instant};

mod finality;
mod preparation;

pub(super) async fn provision(
    rpc: &(impl Rpc + Sync),
    private_key: Option<&str>,
    args: &UpgradeProvisionArgs,
) -> Result<()> {
    let UpgradeProvisionArgs {
        candidate:
            UpgradeCandidateArgs {
                candidate_enclave_socket: endpoint,
                node_data_dir: node_dir,
            },
        reth_p2p_secret_key,
        genesis,
        binding_id: binding,
        valid_until,
        timeout_secs,
        legacy_direct_dev_source,
        new_attempt: _,
    } = args;
    let node_key = reth_p2p_secret_key.as_deref();
    let timeout = Duration::from_secs(*timeout_secs);
    let legacy_source = *legacy_direct_dev_source;
    let relay = RelaySignerV1::new(
        private_key.ok_or_else(|| eyre::eyre!("upgrade-provision requires --private-key"))?,
    )?;
    let committed = load_committed_enclave_manifest_v1(node_dir)?;
    let (mut candidate, signing, selector) = connect_upgrade_candidate_v1(
        endpoint,
        node_dir,
        &committed,
        rpc.eth_chain_id().await?,
        node_key,
    )?;
    let binding_id = parse_nonzero_b256(binding, "--binding-id")?;
    let manifest_hash = candidate
        .manifest()
        .authorization_hash()
        .map_err(|e| eyre::eyre!("manifest: {e}"))?;
    let durable = preparation::NetworkPreparation {
        rpc,
        candidate: &mut candidate,
        signing: &signing,
        relay: &relay,
        target: preparation::ProvisioningTarget {
            binding_id,
            valid_until: *valid_until,
            manifest_hash,
        },
    }
    .load_or_prepare(args, &selector)
    .await?;
    let (context, finalized_height) =
        finality::wait_for_finalized_prepare(rpc, &relay, &durable, timeout).await?;
    match classify_join_offer_key_state_transport(
        candidate.request(&EnclaveRequest::GetPublicKeys)?,
        context.tribute_offer_public,
    )? {
        JoinOfferKeyState::ReadyExact => {}
        JoinOfferKeyState::ReadyMismatch => {
            eyre::bail!("candidate already contains a different network key")
        }
        JoinOfferKeyState::Keyless => {
            let proof = collect_proof(rpc, genesis, finalized_height, &context).await?;
            super::rpc::refresh_call_context(rpc, None).await?;
            let artifact = rpc
                .upgrade_key_v1(&durable.context, &proof, legacy_source)
                .await?;
            let decoded = DcapOnboardingArtifactV1::decode_canonical(&artifact)
                .map_err(|e| eyre::eyre!("export artifact: {e:?}"))?;
            if decoded.context != context {
                eyre::bail!("source returned a different recipient");
            }
            let response = candidate.ingest_upgrade_key_v1(&proof, &artifact)?;
            if !matches!(response, EnclaveResponse::FinalizedAdmissionIngestedV1 { tribute_offer_public, .. } if tribute_offer_public == context.tribute_offer_public)
            {
                eyre::bail!("candidate installed an unexpected network key");
            }
        }
    }
    let snapshot = record_network_key_provisioned_v1(node_dir)?;
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    println!("network key received and sealed by candidate; next: upgrade-submit with the same binding ID");
    Ok(())
}

async fn pending_at(
    rpc: &(impl Rpc + Sync),
    node: B256,
    tag: &str,
) -> Result<ITeeRegistryV1::pendingEnclaveUpgradeReturn> {
    let bytes = rpc
        .eth_call_at(
            TEE_REGISTRY_ADDRESS,
            &ITeeRegistryV1::pendingEnclaveUpgradeCall { nodeIdHash: node }.abi_encode(),
            tag,
        )
        .await?;
    Ok(ITeeRegistryV1::pendingEnclaveUpgradeCall::abi_decode_returns(&bytes)?)
}
async fn scalar_at(rpc: &(impl Rpc + Sync), call: Vec<u8>, tag: &str) -> Result<u64> {
    let bytes = rpc.eth_call_at(TEE_REGISTRY_ADDRESS, &call, tag).await?;
    let value = U256::abi_decode(&bytes)?;
    u64::try_from(value).map_err(|_| eyre::eyre!("registry epoch exceeds u64"))
}

async fn collect_proof(
    rpc: &(impl Rpc + Sync),
    genesis: &Path,
    finalized_height: u64,
    context: &DcapOnboardingContextV1,
) -> Result<UpgradeKeyProofV1> {
    let mut transitions = Vec::new();
    // Capture the exact state proof before scanning history while the head advances.
    let slots = outbe_tee::finalized_admission::upgrade_registry_slots_v1(context);
    let slot_params = slots
        .iter()
        .map(|slot| format!("{slot:#x}"))
        .collect::<Vec<_>>();
    let (finalized_height, opening) =
        admission_history::registry_opening(rpc, finalized_height, &slot_params).await?;
    let anchor =
        load_finalized_admission_anchor_v1(rpc, genesis, finalized_height, context).await?;

    let mut next_transition_epoch = 1_u64;
    let mut admission = None;
    for height in 1..=finalized_height {
        let Some(public) = admission_history::admission_public(
            rpc,
            height,
            finalized_height,
            next_transition_epoch,
        )
        .await?
        else {
            continue;
        };
        let (block, compact) = admission_history::compact_header(&public, height)?;
        let artifacts = decode_outbe_block_artifacts(block.header().extra_data().as_ref())
            .map_err(|error| eyre::eyre!("decode block {height} artifacts: {error:?}"))?;
        if let Some(ConsensusHeaderArtifact::CommitteePreAnnounce { epoch, .. }) =
            artifacts.consensus_header_artifact
        {
            if height < finalized_height && epoch == next_transition_epoch {
                let transition = compact
                    .clone()
                    .encode_canonical()
                    .map_err(|error| eyre::eyre!("encode committee transition: {error}"))?;
                transitions.push(transition.into());
                if transitions.len() > outbe_tee::upgrade_transfer::MAX_UPGRADE_COMMITTEES {
                    eyre::bail!("upgrade committee proof exceeds count limit");
                }
                next_transition_epoch = next_transition_epoch
                    .checked_add(1)
                    .ok_or_else(|| eyre::eyre!("committee transition epoch overflow"))?;
            }
        }
        if height == finalized_height {
            admission = Some(compact);
        }
    }

    let registry_account = MptAccountProofV1 {
        nonce: json_hex_u64_field(&opening, "nonce")?,
        balance: json_hex_u256_field(&opening, "balance")?,
        code_hash: json_b256_field(&opening, "codeHash")?,
        storage_root: json_b256_field(&opening, "storageHash")?,
        nodes: json_hex_array(&opening, "accountProof")?,
    };
    let storage = opening
        .get("storageProof")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| eyre::eyre!("eth_getProof has no storageProof array"))?;
    let mut registry_storage = Vec::with_capacity(storage.len());
    for item in storage {
        registry_storage.push(MptStorageProofV1 {
            key: json_b256_field(item, "key")?,
            value: json_hex_u256_field(item, "value")?,
            nodes: json_hex_array(item, "proof")?,
        });
    }
    if registry_storage.len() != slots.len() {
        return Err(eyre::eyre!(
            "eth_getProof omitted a required TeeRegistry slot"
        ));
    }
    let admission_witness = FinalizedAdmissionWitnessV1 {
        admission: admission.expect("positive finalized height sets admission proof"),
        registry_account,
        registry_storage,
    }
    .encode_canonical()
    .map_err(|error| eyre::eyre!("encode finalized onboarding admission witness: {error}"))?;

    let proof = UpgradeKeyProofV1 {
        anchor_outcome: anchor.into(),
        committee_transitions: transitions,
        admission: admission_witness.into(),
    };
    proof.validate()?;
    Ok(proof)
}

#[cfg(test)]
#[path = "tests/upgrade_network.rs"]
mod tests;
