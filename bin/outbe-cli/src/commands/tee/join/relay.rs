use super::super::relay_tx::{
    buffered_gas_price, RawRelayTransactionV1, RelaySignerV1, UnsignedRelayTransactionV1,
};
use super::super::CliFinalityRpc;
use super::finalized_binding_matches_intent;

use super::ExactJoinRelayV1;

use crate::errors::TeeJoinError;
use crate::rpc::Rpc;

use alloy_primitives::keccak256;

use alloy_primitives::B256;

use eyre::Result;
use eyre::WrapErr;

use outbe_primitives::tee_attestation_v1::RegistrationIntentV1;
use outbe_tee::registry_view::{
    read_finalized_registry_view_v1, FinalizedRegistryChainViewV1, NodeBindingSelectorV1,
};

use std::time::Duration;

pub(in super::super) async fn relay_exact_join_transaction(
    client: &(impl Rpc + Sync),
    relay: &ExactJoinRelayV1,
    resumes_finalized_target: bool,
) -> Result<String> {
    if keccak256(&relay.raw_transaction) != relay.transaction_hash {
        return Err(TeeJoinError::RelayTxHashMismatch.into());
    }
    let encoded_hash = format!("0x{}", hex::encode(relay.transaction_hash));
    if resumes_finalized_target {
        return Ok(encoded_hash);
    }
    let returned = match client
        .eth_send_raw_transaction(&relay.raw_transaction)
        .await
    {
        Ok(returned) => returned,
        Err(error) => {
            let message = format!("{error:#}").to_ascii_lowercase();
            if message.contains("already known") || message.contains("known transaction") {
                return Ok(encoded_hash);
            }
            if message.contains("nonce too low") {
                let exact_receipt_exists = client
                    .eth_get_transaction_receipt(&encoded_hash)
                    .await?
                    .and_then(|receipt| {
                        receipt
                            .get("transactionHash")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .is_some_and(|hash| hash.eq_ignore_ascii_case(&encoded_hash));
                if exact_receipt_exists {
                    return Ok(encoded_hash);
                }
            }
            return Err(error).wrap_err("relay exact durable registerEnclave transaction");
        }
    };
    let returned_hash = returned
        .parse::<B256>()
        .wrap_err("parse durable registerEnclave transaction hash")?;
    (returned_hash == relay.transaction_hash)
        .then_some(encoded_hash)
        .ok_or_else(|| TeeJoinError::ReturnedTxHashMismatch.into())
}

pub(in super::super) async fn await_finalized_join_target(
    client: &(impl Rpc + Sync),
    selector: &NodeBindingSelectorV1,
    intent: &RegistrationIntentV1,
    timeout: Duration,
) -> Result<FinalizedRegistryChainViewV1> {
    let started = tokio::time::Instant::now();
    loop {
        let view = read_finalized_registry_view_v1(&CliFinalityRpc(client), selector).await?;
        if finalized_join_target_ready(
            view.binding.as_ref(),
            view.schedule.finalized_timestamp,
            intent,
        )? {
            return Ok(view);
        }
        if started.elapsed() >= timeout {
            return Err(TeeJoinError::FinalizedJoinTimeout(timeout.as_secs()).into());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn finalized_join_target_ready(
    binding: Option<&outbe_tee::registry_view::RenewalBindingV1>,
    finalized_timestamp: u64,
    intent: &RegistrationIntentV1,
) -> Result<bool> {
    let Some(binding) = binding else {
        return Ok(false);
    };
    if finalized_binding_matches_intent(binding, intent)? {
        return Ok(true);
    }
    if finalized_timestamp >= binding.valid_until {
        return Ok(false);
    }
    if binding.binding_version >= intent.binding_version {
        return Err(TeeJoinError::CompetingLiveBinding.into());
    }
    let source = (
        intent.attestation_mode,
        intent.operation,
        binding.node_id_hash,
        super::registration_counters(Some(binding))?,
    );
    let expected = (
        outbe_primitives::tee_attestation_v1::AttestationMode::NoAttestationDev,
        outbe_primitives::tee_attestation_v1::AttestationOperationV1::RegisterEnclave,
        intent
            .node_id
            .node_id_hash()
            .map_err(TeeJoinError::HashRegistrationNodeIdentity)?,
        (
            intent.binding_version,
            intent.registration_version,
            intent.renewal_nonce,
            intent.transition_nonce,
        ),
    );
    // Only the exact source counters may remain live while this registration finalizes.
    if source != expected {
        return Err(TeeJoinError::CompetingLiveBinding.into());
    }
    Ok(false)
}

pub(super) struct JoinRelayContext<'a> {
    pub(super) private_key_hex: &'a str,
    pub(super) evm_signer: &'a crate::tx::TxSigner,
    pub(super) rpc_chain_id: u64,
    pub(super) node_data_dir: Option<&'a std::path::Path>,
    pub(super) enclave_socket: &'a str,
    pub(super) call: Vec<u8>,
    pub(super) gas_limit: u64,
    pub(super) resumes_finalized_target: bool,
    pub(super) offer_key_state: super::JoinOfferKeyState,
    pub(super) expected_offer_pub: [u8; 32],
    pub(super) finalized: &'a FinalizedRegistryChainViewV1,
    pub(super) node_id_hash: B256,
    pub(super) enclave_id: B256,
    pub(super) intent_hash: B256,
    pub(super) is_committed_durable: bool,
}

pub(super) async fn submit_or_resume_join_tx(
    client: &(impl Rpc + Sync),
    enclave: &mut Option<super::JoinEnclave>,
    ctx: JoinRelayContext<'_>,
) -> Result<Option<String>> {
    let calldata_hash = keccak256(&ctx.call);
    if enclave
        .as_ref()
        .is_some_and(super::JoinEnclave::is_candidate)
    {
        let tx_hash = submit_candidate_join_tx(client, &ctx, calldata_hash).await?;
        return Ok(Some(tx_hash));
    }
    if ctx.is_committed_durable {
        return submit_committed_join_tx(client, enclave, &ctx, calldata_hash).await;
    }
    let tx_hash = ctx
        .evm_signer
        .send_tx_with_gas(
            client,
            crate::abi::TEE_REGISTRY_ADDR,
            ctx.call,
            alloy_primitives::U256::ZERO,
            ctx.gas_limit,
        )
        .await
        .wrap_err("V1 registerEnclave submission failed")?;
    Ok(Some(tx_hash))
}

async fn sign_join_relay_tx(
    client: &(impl Rpc + Sync),
    ctx: &JoinRelayContext<'_>,
) -> Result<RawRelayTransactionV1> {
    use alloy_primitives::U256;

    let relay_signer = RelaySignerV1::new(ctx.private_key_hex)?;
    if relay_signer.address() != ctx.evm_signer.address() {
        return Err(TeeJoinError::InconsistentSignerAddress.into());
    }
    let account_nonce = client
        .eth_get_transaction_count(ctx.evm_signer.address())
        .await?;
    let gas_price = buffered_gas_price(client.eth_gas_price().await?);
    let required_balance = gas_price.saturating_mul(U256::from(ctx.gas_limit));
    let balance = client.eth_get_balance(ctx.evm_signer.address()).await?;
    if balance < required_balance {
        return Err(TeeJoinError::InsufficientJoinBalance {
            signer: ctx.evm_signer.address(),
            balance,
            required: required_balance,
        }
        .into());
    }
    relay_signer.sign_renewal(UnsignedRelayTransactionV1 {
        chain_id: ctx.rpc_chain_id,
        account_nonce,
        gas_price,
        gas_limit: ctx.gas_limit,
        to: crate::abi::TEE_REGISTRY_ADDR,
        calldata: &ctx.call,
    })
}

async fn submit_candidate_join_tx(
    client: &(impl Rpc + Sync),
    ctx: &JoinRelayContext<'_>,
    calldata_hash: B256,
) -> Result<String> {
    let node_data_dir = ctx
        .node_data_dir
        .expect("candidate join requires NodeHost state");
    let relay = if let Some(durable) = outbe_tee::load_replacement_candidate_relay(node_data_dir)
        .map_err(TeeJoinError::LoadCandidateRelay)?
    {
        if durable.calldata_hash() != calldata_hash {
            return Err(TeeJoinError::CandidateRelayCalldataMismatch.into());
        }
        durable
    } else {
        if ctx.resumes_finalized_target {
            return Err(TeeJoinError::MissingCandidateRelayCheckpoint.into());
        }
        let raw = sign_join_relay_tx(client, ctx).await?;
        outbe_tee::persist_replacement_candidate_relay(
            node_data_dir,
            calldata_hash,
            &raw.raw_transaction,
        )
        .map_err(TeeJoinError::PersistCandidateRelay)?
    };
    let exact = ExactJoinRelayV1 {
        transaction_hash: relay.transaction_hash(),
        raw_transaction: relay.raw_transaction().to_vec(),
    };
    relay_exact_join_transaction(client, &exact, ctx.resumes_finalized_target).await
}

async fn submit_committed_join_tx(
    client: &(impl Rpc + Sync),
    enclave: &mut Option<super::JoinEnclave>,
    ctx: &JoinRelayContext<'_>,
    calldata_hash: B256,
) -> Result<Option<String>> {
    let node_data_dir = ctx
        .node_data_dir
        .expect("committed join requires NodeHost state");
    let relay = if let Some(durable) = outbe_tee::load_committed_join_relay(node_data_dir)
        .map_err(TeeJoinError::LoadCommittedRelay)?
    {
        if durable.calldata_hash() != calldata_hash {
            return Err(TeeJoinError::CommittedRelayCalldataMismatch.into());
        }
        durable
    } else {
        match super::plan_missing_committed_relay(
            ctx.resumes_finalized_target,
            ctx.offer_key_state,
        )? {
            super::MissingCommittedRelayPlan::CleanupReadyExact => {
                let _ = enclave.take();
                cleanup_completed_committed_join(ctx, node_data_dir)?;
                return Ok(None);
            }
            super::MissingCommittedRelayPlan::ConstructAndPersist => {}
        }
        let raw = sign_join_relay_tx(client, ctx).await?;
        let from_block = client.eth_block_number().await?;
        outbe_tee::persist_committed_join_relay(
            node_data_dir,
            calldata_hash,
            from_block,
            &raw.raw_transaction,
        )
        .map_err(TeeJoinError::PersistCommittedRelay)?
    };
    let exact = ExactJoinRelayV1 {
        transaction_hash: relay.transaction_hash(),
        raw_transaction: relay.raw_transaction().to_vec(),
    };
    let tx_hash =
        relay_exact_join_transaction(client, &exact, ctx.resumes_finalized_target).await?;
    Ok(Some(tx_hash))
}

fn cleanup_completed_committed_join(
    ctx: &JoinRelayContext<'_>,
    node_data_dir: &std::path::Path,
) -> Result<()> {
    use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};

    let mut reopened =
        outbe_tee::connect_committed_node_host_enclave(ctx.enclave_socket, node_data_dir)
            .map_err(TeeJoinError::ReopenCompletedCommittedEnclave)?;
    match reopened.request(&EnclaveRequest::GetPublicKeys)? {
        EnclaveResponse::PublicKeys {
            offer_key_ready: true,
            recipient_x25519_pub,
            ..
        } if recipient_x25519_pub == ctx.expected_offer_pub => {}
        other => {
            return Err(TeeJoinError::CleanupOfferKeyNotReady(format!("{other:?}")).into());
        }
    }
    super::persist_authorized_join_admission_anchor_v1(
        node_data_dir,
        ctx.finalized,
        ctx.node_id_hash,
        ctx.enclave_id,
        ctx.intent_hash,
    )?;
    outbe_tee::clear_committed_join_checkpoint(node_data_dir, ctx.intent_hash)
        .map_err(TeeJoinError::ClearRecoveredCheckpoint)?;
    println!(
        "[ok] finalized tee join cleanup recovered without another transaction or onboarding ingest"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::policy::test_renewal_binding;
    use super::*;
    use outbe_primitives::tee_attestation_v1::{AttestationMode, AttestationOperationV1, NodeIdV1};

    fn fixture() -> (
        outbe_tee::registry_view::RenewalBindingV1,
        RegistrationIntentV1,
    ) {
        let public = k256::ecdsa::SigningKey::from_bytes((&[1; 32]).into())
            .unwrap()
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap();
        let node_id = NodeIdV1 {
            reth_p2p_public: public,
        };
        let mut binding = test_renewal_binding();
        binding.node_id_hash = node_id.node_id_hash().unwrap();
        let intent = RegistrationIntentV1 {
            chain_id: [1; 32],
            genesis_hash: B256::repeat_byte(20),
            operation: AttestationOperationV1::RegisterEnclave,
            attestation_mode: AttestationMode::NoAttestationDev,
            policy_hash: binding.policy_hash,
            node_id,
            enclave_id: B256::repeat_byte(21),
            binding_id: B256::repeat_byte(22),
            binding_version: 2,
            registration_version: 1,
            renewal_nonce: 0,
            transition_nonce: 0,
            requested_valid_until: 300,
            recipient_x25519: [7; 32],
            attestation_ed25519: [8; 32],
            noise_responder_x25519: [9; 32],
            node_host_authorization_hash: B256::repeat_byte(13),
        };
        (binding, intent)
    }

    #[test]
    fn relay_waits_for_own_live_predecessor_but_rejects_competitors() {
        let (binding, intent) = fixture();
        assert!(!finalized_join_target_ready(Some(&binding), 199, &intent).unwrap());
        assert!(!finalized_join_target_ready(Some(&binding), 200, &intent).unwrap());
        assert!(!finalized_join_target_ready(None, 199, &intent).unwrap());
        for version in [intent.binding_version, intent.binding_version + 1] {
            let mut competitor = binding.clone();
            competitor.binding_version = version;
            let error = finalized_join_target_ready(Some(&competitor), 199, &intent).unwrap_err();
            assert!(matches!(
                error.downcast_ref::<TeeJoinError>(),
                Some(TeeJoinError::CompetingLiveBinding)
            ));
        }
        let mut foreign = binding.clone();
        foreign.node_id_hash = B256::repeat_byte(31);
        assert!(finalized_join_target_ready(Some(&foreign), 199, &intent).is_err());
        for field in [0, 1, 2] {
            let mut changed = binding.clone();
            match field {
                0 => changed.registration_version += 1,
                1 => changed.renewal_nonce += 1,
                _ => changed.transition_nonce += 1,
            }
            assert!(finalized_join_target_ready(Some(&changed), 199, &intent).is_err());
        }
        let mut dcap = intent;
        dcap.attestation_mode = AttestationMode::DcapRequired;
        assert!(finalized_join_target_ready(Some(&binding), 199, &dcap).is_err());
    }

    #[test]
    fn relay_accepts_only_the_exact_finalized_target() {
        let (mut binding, intent) = fixture();
        binding.enclave_id = intent.enclave_id;
        binding.binding_id = intent.binding_id;
        binding.intent_hash = intent.intent_hash().unwrap();
        binding.binding_version = 2;
        binding.registration_version = 1;
        binding.valid_until = 300;
        assert!(finalized_join_target_ready(Some(&binding), 199, &intent).unwrap());
        binding.intent_hash = B256::repeat_byte(32);
        assert!(finalized_join_target_ready(Some(&binding), 199, &intent).is_err());
    }
}
