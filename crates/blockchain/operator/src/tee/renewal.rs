//! One fail-closed DCAP renewal reducer shared by the node worker and CLI.

use std::path::PathBuf;

use alloy_primitives::{keccak256, B256, U256};
use alloy_sol_types::SolCall as _;
use eyre::{Result, WrapErr as _};
use outbe_primitives::{
    addresses::TEE_REGISTRY_ADDRESS,
    tee_attestation_v1::{
        AttestationEvidenceV1, AttestationOperationV1, DcapEvidenceV1,
        EnclaveInitializationManifestV1, RegistrationIntentV1, RegistryMutatorV1,
        TeeRegistryGasScheduleV1,
    },
    tee_registry_abi_v1::ITeeRegistryV1,
};
use outbe_tee::{
    acquire_dcap_collateral_v1, dcap_collateral_validity_window_v1,
    dcap_protocol::dcap_evidence_hash_v1, AuthorizedEnclaveClient, GeneratedDcapQuoteV1,
};

use crate::{
    rpc::RenewalRpc,
    tx::{buffered_gas_price, RelaySignerV1},
};

use super::{
    registry::{
        read_finalized_renewal_view_v1, FinalizedRenewalChainViewV1, NodeBindingSelectorV1,
        RenewalBindingV1,
    },
    renewal_journal::{
        PreparedRenewalV1, RenewalJournalGuard, RenewalJournalSnapshotV1, RenewalJournalStateV1,
    },
};

pub trait RenewalEnclaveV1 {
    fn generate_dcap_quote(
        &mut self,
        intent: &RegistrationIntentV1,
    ) -> Result<GeneratedDcapQuoteV1>;
}

impl RenewalEnclaveV1 for AuthorizedEnclaveClient {
    fn generate_dcap_quote(
        &mut self,
        intent: &RegistrationIntentV1,
    ) -> Result<GeneratedDcapQuoteV1> {
        AuthorizedEnclaveClient::generate_dcap_quote(self, intent)
            .map_err(|error| eyre::eyre!(error))
    }
}

pub trait RenewalNodeSignerV1 {
    fn sign_node_hash(&self, hash: B256) -> Result<[u8; 65]>;
}

impl<F> RenewalNodeSignerV1 for F
where
    F: Fn(B256) -> Result<[u8; 65]>,
{
    fn sign_node_hash(&self, hash: B256) -> Result<[u8; 65]> {
        self(hash)
    }
}

#[derive(Clone, Debug)]
pub struct RenewalServiceConfigV1 {
    pub node_data_dir: PathBuf,
    pub selector: NodeBindingSelectorV1,
    pub manifest: EnclaveInitializationManifestV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenewalOutcomeV1 {
    NotDue {
        finalized_height: u64,
        opens_at_timestamp: u64,
    },
    Submitted {
        transaction_hash: B256,
        replayed: bool,
    },
    Finalized {
        finalized_height: u64,
        valid_until: u64,
    },
    Abandoned {
        finalized_height: u64,
        reason: String,
    },
}

pub async fn run_renewal_once_v1(
    rpc: &(impl RenewalRpc + Sync),
    evm_signer: &RelaySignerV1,
    enclave: &mut impl RenewalEnclaveV1,
    node_signer: &impl RenewalNodeSignerV1,
    config: &RenewalServiceConfigV1,
) -> Result<RenewalOutcomeV1> {
    // Serialize renewal intent creation against upgrade preparation. Holding
    // this host-only lock across the reducer prevents the two exact-next
    // Registry counter flows from being prepared concurrently.
    let upgrade_guard = super::UpgradeJournalGuardV1::acquire(&config.node_data_dir)?;
    if let Some(upgrade) = upgrade_guard.load()? {
        if matches!(
            upgrade.lifecycle,
            super::UpgradeJournalStateV1::CandidatePrepared { .. }
                | super::UpgradeJournalStateV1::RootCopied { .. }
                | super::UpgradeJournalStateV1::CandidateKeyReady { .. }
                | super::UpgradeJournalStateV1::SubmissionPrepared { .. }
                | super::UpgradeJournalStateV1::Submitted { .. }
                | super::UpgradeJournalStateV1::Finalized { .. }
                | super::UpgradeJournalStateV1::Promoted { .. }
                | super::UpgradeJournalStateV1::TerminalMissedCutoff { .. }
        ) {
            eyre::bail!(
                "renewal is blocked while an enclave upgrade is journaled to preserve exact transition counters"
            );
        }
    }
    let _upgrade_guard = upgrade_guard;
    let journal = RenewalJournalGuard::acquire(&config.node_data_dir)?;
    let view = read_finalized_renewal_view_v1(rpc, &config.selector).await?;
    validate_identity(config, &view)?;

    if let Some(snapshot) = journal.load()? {
        match snapshot.lifecycle {
            RenewalJournalStateV1::Prepared { attempt }
            | RenewalJournalStateV1::Submitted { attempt, .. } => {
                if target_matches(&view.binding, &attempt)? {
                    return finalize(&journal, attempt, &view);
                }
                ensure_source_or_conflict(&view.binding, &attempt.source)?;
                if let Some(reason) =
                    permanent_staleness(&attempt, view.schedule.finalized_timestamp)
                {
                    journal.store(RenewalJournalSnapshotV1::new(
                        RenewalJournalStateV1::Abandoned {
                            attempt,
                            abandoned_at_finalized_height: view.schedule.finalized_height,
                            reason: reason.clone(),
                        },
                    ))?;
                    return Ok(RenewalOutcomeV1::Abandoned {
                        finalized_height: view.schedule.finalized_height,
                        reason,
                    });
                }
                return submit_attempt(
                    rpc,
                    &journal,
                    attempt,
                    view.schedule.finalized_height,
                    true,
                )
                .await;
            }
            RenewalJournalStateV1::Finalized {
                attempt,
                finalized_binding,
                ..
            } => {
                if view.binding == finalized_binding || target_matches(&view.binding, &attempt)? {
                    if !renewal_is_open(&view.binding, view.schedule.finalized_timestamp)? {
                        return Ok(RenewalOutcomeV1::NotDue {
                            finalized_height: view.schedule.finalized_height,
                            opens_at_timestamp: renewal_opens_at(&view.binding)?,
                        });
                    }
                } else {
                    eyre::bail!("finalized Registry binding diverged from the renewal journal");
                }
            }
            RenewalJournalStateV1::Abandoned { attempt, .. } => {
                ensure_source_or_conflict(&view.binding, &attempt.source)?;
            }
        }
    }

    if !renewal_is_open(&view.binding, view.schedule.finalized_timestamp)? {
        return Ok(RenewalOutcomeV1::NotDue {
            finalized_height: view.schedule.finalized_height,
            opens_at_timestamp: renewal_opens_at(&view.binding)?,
        });
    }
    let attempt = prepare_attempt(rpc, evm_signer, enclave, node_signer, config, &view).await?;
    journal.store(RenewalJournalSnapshotV1::new(
        RenewalJournalStateV1::Prepared {
            attempt: attempt.clone(),
        },
    ))?;
    submit_attempt(
        rpc,
        &journal,
        attempt,
        view.schedule.finalized_height,
        false,
    )
    .await
}

fn validate_identity(
    config: &RenewalServiceConfigV1,
    view: &FinalizedRenewalChainViewV1,
) -> Result<()> {
    let manifest = &config.manifest;
    let node_id_hash = manifest
        .node_id
        .node_id_hash()
        .map_err(|error| eyre::eyre!("hash manifest node identity: {error}"))?;
    let enclave_id = manifest
        .enclave_id()
        .map_err(|error| eyre::eyre!("derive manifest enclave identity: {error}"))?;
    let authorization = manifest
        .node_host_authorization_hash()
        .map_err(|error| eyre::eyre!("derive manifest NodeHost authorization: {error}"))?;
    let policy_hash = view
        .policy
        .policy_hash()
        .map_err(|error| eyre::eyre!("hash finalized policy: {error}"))?;
    if manifest.chain_id != view.policy.chain_id
        || manifest.genesis_hash != view.policy.genesis_hash
        || node_id_hash != view.binding.node_id_hash
        || enclave_id != view.binding.enclave_id
        || B256::from(manifest.recipient_x25519) != view.binding.recipient_x25519
        || B256::from(manifest.attestation_ed25519) != view.binding.attestation_ed25519
        || B256::from(manifest.noise_responder_x25519) != view.binding.noise_responder_x25519
        || authorization != view.binding.node_host_authorization_hash
        || policy_hash != view.binding.policy_hash
    {
        eyre::bail!("committed NodeHost manifest does not match the finalized Registry binding");
    }
    match &config.selector {
        NodeBindingSelectorV1::NodeHost(public) if public == &manifest.node_id.reth_p2p_public => {}
        _ => {
            eyre::bail!("renewal selector does not match the committed node identity");
        }
    }
    Ok(())
}

async fn prepare_attempt(
    rpc: &(impl RenewalRpc + Sync),
    evm_signer: &RelaySignerV1,
    enclave: &mut impl RenewalEnclaveV1,
    node_signer: &impl RenewalNodeSignerV1,
    config: &RenewalServiceConfigV1,
    view: &FinalizedRenewalChainViewV1,
) -> Result<PreparedRenewalV1> {
    let desired_valid_until = view
        .schedule
        .finalized_timestamp
        .checked_add(view.policy.maximum_lease)
        .ok_or_else(|| eyre::eyre!("maximum renewal lease overflows timestamp"))?;
    let mut intent = renewal_intent(config, view, desired_valid_until)?;
    let mut generated_evidence = generate_evidence(enclave, &intent, &view.policy)?;
    let mut window = dcap_collateral_validity_window_v1(&generated_evidence.0, &view.policy)
        .map_err(|error| eyre::eyre!("validate signed renewal collateral window: {error:?}"))?;
    let ceiling = window
        .expiration_ceiling
        .checked_sub(view.policy.collateral_margin)
        .ok_or_else(|| eyre::eyre!("renewal collateral cannot satisfy the active margin"))?;
    let requested_valid_until = desired_valid_until.min(ceiling);
    let minimum_valid_until = view
        .schedule
        .finalized_timestamp
        .checked_add(view.policy.minimum_lease)
        .ok_or_else(|| eyre::eyre!("minimum renewal lease overflows timestamp"))?;
    if window.issue_floor > view.schedule.finalized_timestamp
        || requested_valid_until < minimum_valid_until
    {
        eyre::bail!("fresh Intel collateral cannot satisfy the active renewal lease policy");
    }
    if requested_valid_until != desired_valid_until {
        intent.requested_valid_until = requested_valid_until;
        generated_evidence = generate_evidence(enclave, &intent, &view.policy)?;
        window = dcap_collateral_validity_window_v1(&generated_evidence.0, &view.policy)
            .map_err(|error| eyre::eyre!("validate final renewal collateral window: {error:?}"))?;
        let final_ceiling = window
            .expiration_ceiling
            .checked_sub(view.policy.collateral_margin)
            .ok_or_else(|| eyre::eyre!("final renewal collateral margin underflows"))?;
        if intent.requested_valid_until > final_ceiling
            || window.issue_floor > view.schedule.finalized_timestamp
        {
            eyre::bail!(
                "regenerated renewal evidence no longer satisfies its signed collateral ceiling"
            );
        }
    }
    let intent_hash = intent
        .intent_hash()
        .map_err(|error| eyre::eyre!("hash renewal intent: {error}"))?;
    let node_signature = node_signer
        .sign_node_hash(intent_hash)
        .wrap_err("sign renewal intent with node authority")?;
    let enclave_signature = generated_evidence.1;
    let evidence_value = AttestationEvidenceV1::Dcap(generated_evidence.0);
    let evidence = evidence_value
        .encode_canonical()
        .map_err(|error| eyre::eyre!("encode canonical renewal evidence: {error}"))?;
    let evidence_hash = dcap_evidence_hash_v1(&evidence)
        .map_err(|code| eyre::eyre!("hash canonical renewal DCAP evidence: {code:?}"))?;
    let calldata = ITeeRegistryV1::renewEnclaveCall {
        evidence: evidence.clone().into(),
        nodeSignature: node_signature.to_vec().into(),
        enclaveSignature: enclave_signature.to_vec().into(),
    }
    .abi_encode();
    let gas_limit = TeeRegistryGasScheduleV1::normative()
        .maximum_transaction_gas(
            RegistryMutatorV1::RenewEnclave,
            calldata.len(),
            evidence.len(),
            view.policy.measurement_rules.len(),
            view.policy.attestation_mode,
        )
        .map_err(|error| eyre::eyre!("calculate normative renewal gas: {error}"))?;
    let chain_id = rpc.chain_id().await?;
    let account_nonce = rpc.transaction_count(evm_signer.address()).await?;
    let gas_price = buffered_gas_price(rpc.gas_price().await?);
    let required_balance = gas_price.saturating_mul(U256::from(gas_limit));
    let balance = rpc.balance(evm_signer.address()).await?;
    if balance < required_balance {
        eyre::bail!(
            "renewal EVM signer {} has {balance} but needs at least {required_balance}",
            evm_signer.address()
        );
    }
    let raw = evm_signer.sign_renewal(
        chain_id,
        account_nonce,
        gas_price,
        gas_limit,
        TEE_REGISTRY_ADDRESS,
        &calldata,
    )?;
    let intent_bytes = intent
        .encode_canonical()
        .map_err(|error| eyre::eyre!("encode canonical renewal intent: {error}"))?;
    Ok(PreparedRenewalV1 {
        source: view.binding.clone(),
        intent: intent_bytes,
        intent_hash,
        evidence_hash,
        evidence,
        node_signature: node_signature.to_vec(),
        enclave_signature: enclave_signature.to_vec(),
        calldata_hash: keccak256(&calldata),
        calldata,
        requested_valid_until: intent.requested_valid_until,
        collateral_valid_until: window.expiration_ceiling,
        collateral_margin: view.policy.collateral_margin,
        // `relay` is retained in the V1 journal shape for restart compatibility;
        // manual renewal binds it to the caller's global EVM signer.
        relay: evm_signer.address(),
        relay_variants: vec![raw],
    })
}

fn renewal_intent(
    config: &RenewalServiceConfigV1,
    view: &FinalizedRenewalChainViewV1,
    requested_valid_until: u64,
) -> Result<RegistrationIntentV1> {
    Ok(RegistrationIntentV1 {
        chain_id: view.policy.chain_id,
        genesis_hash: view.policy.genesis_hash,
        operation: AttestationOperationV1::RenewEnclave,
        attestation_mode: view.policy.attestation_mode,
        policy_hash: view
            .policy
            .policy_hash()
            .map_err(|error| eyre::eyre!("hash active policy: {error}"))?,
        node_id: config.manifest.node_id.clone(),
        enclave_id: view.binding.enclave_id,
        binding_id: view.binding.binding_id,
        binding_version: view.binding.binding_version,
        registration_version: view
            .binding
            .registration_version
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("registration version exhausted"))?,
        renewal_nonce: view
            .binding
            .renewal_nonce
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("renewal nonce exhausted"))?,
        transition_nonce: view.binding.transition_nonce,
        requested_valid_until,
        recipient_x25519: config.manifest.recipient_x25519,
        attestation_ed25519: config.manifest.attestation_ed25519,
        noise_responder_x25519: config.manifest.noise_responder_x25519,
        node_host_authorization_hash: view.binding.node_host_authorization_hash,
    })
}

fn generate_evidence(
    enclave: &mut impl RenewalEnclaveV1,
    intent: &RegistrationIntentV1,
    policy: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
) -> Result<(DcapEvidenceV1, [u8; 64])> {
    let generated = enclave
        .generate_dcap_quote(intent)
        .wrap_err("generate intent-bound renewal quote")?;
    let components = acquire_dcap_collateral_v1(&generated.quote_body)
        .map_err(|error| eyre::eyre!("acquire renewal collateral: {error}"))?;
    let evidence = DcapEvidenceV1 {
        intent: intent.clone(),
        quote: generated.quote_body,
        components,
        transition_key_ready_proof: generated.transition_key_ready_proof,
    };
    dcap_collateral_validity_window_v1(&evidence, policy)
        .map_err(|error| eyre::eyre!("validate renewal collateral: {error:?}"))?;
    Ok((evidence, generated.enclave_signature))
}

async fn submit_attempt(
    rpc: &(impl RenewalRpc + Sync),
    journal: &RenewalJournalGuard,
    attempt: PreparedRenewalV1,
    finalized_height: u64,
    replayed: bool,
) -> Result<RenewalOutcomeV1> {
    let raw = attempt
        .relay_variants
        .last()
        .ok_or_else(|| eyre::eyre!("renewal attempt has no relay transaction"))?;
    let returned_hash = match rpc.send_raw_transaction(&raw.raw_transaction).await {
        Ok(returned) => returned
            .parse::<B256>()
            .wrap_err("parse eth_sendRawTransaction renewal hash")?,
        Err(error) if transaction_is_already_known(&error) => raw.transaction_hash,
        Err(error) => return Err(error).wrap_err("submit exact renewal transaction"),
    };
    if returned_hash != raw.transaction_hash {
        eyre::bail!("RPC returned a transaction hash different from the signed renewal bytes");
    }
    let hashes = attempt
        .relay_variants
        .iter()
        .map(|variant| variant.transaction_hash)
        .collect();
    journal.store(RenewalJournalSnapshotV1::new(
        RenewalJournalStateV1::Submitted {
            attempt,
            submitted_at_finalized_height: finalized_height,
            transaction_hashes: hashes,
        },
    ))?;
    Ok(RenewalOutcomeV1::Submitted {
        transaction_hash: returned_hash,
        replayed,
    })
}

fn transaction_is_already_known(error: &eyre::Report) -> bool {
    let message = format!("{error:#}").to_ascii_lowercase();
    message.contains("already known") || message.contains("known transaction")
}

fn finalize(
    journal: &RenewalJournalGuard,
    attempt: PreparedRenewalV1,
    view: &FinalizedRenewalChainViewV1,
) -> Result<RenewalOutcomeV1> {
    journal.store(RenewalJournalSnapshotV1::new(
        RenewalJournalStateV1::Finalized {
            attempt: Box::new(attempt),
            finalized_binding: view.binding.clone(),
            finalized_height: view.schedule.finalized_height,
            finalized_hash: view.schedule.finalized_hash,
        },
    ))?;
    Ok(RenewalOutcomeV1::Finalized {
        finalized_height: view.schedule.finalized_height,
        valid_until: view.binding.valid_until,
    })
}

fn ensure_source_or_conflict(current: &RenewalBindingV1, source: &RenewalBindingV1) -> Result<()> {
    if current != source {
        eyre::bail!("finalized Registry binding matches neither renewal source nor target");
    }
    Ok(())
}

fn target_matches(current: &RenewalBindingV1, attempt: &PreparedRenewalV1) -> Result<bool> {
    let intent = RegistrationIntentV1::decode_canonical(&attempt.intent)
        .map_err(|error| eyre::eyre!("decode journal renewal intent: {error}"))?;
    Ok(current.node_id_hash == attempt.source.node_id_hash
        && current.enclave_id == intent.enclave_id
        && current.binding_id == intent.binding_id
        && current.intent_hash == attempt.intent_hash
        && current.evidence_hash == attempt.evidence_hash
        && current.policy_hash == intent.policy_hash
        && current.binding_version == intent.binding_version
        && current.registration_version == intent.registration_version
        && current.renewal_nonce == intent.renewal_nonce
        && current.transition_nonce == intent.transition_nonce
        && current.valid_until == intent.requested_valid_until
        && current.recipient_x25519 == B256::from(intent.recipient_x25519)
        && current.attestation_ed25519 == B256::from(intent.attestation_ed25519)
        && current.noise_responder_x25519 == B256::from(intent.noise_responder_x25519)
        && current.node_host_authorization_hash == intent.node_host_authorization_hash)
}

fn permanent_staleness(attempt: &PreparedRenewalV1, finalized_timestamp: u64) -> Option<String> {
    if finalized_timestamp >= attempt.collateral_valid_until {
        return Some("finalized consensus time reached the signed collateral expiration".into());
    }
    if finalized_timestamp >= attempt.requested_valid_until {
        return Some("finalized consensus time reached the requested lease expiration".into());
    }
    None
}

fn renewal_opens_at(binding: &RenewalBindingV1) -> Result<u64> {
    let duration = binding
        .valid_until
        .checked_sub(binding.lease_started_at)
        .ok_or_else(|| eyre::eyre!("finalized Registry lease interval is inconsistent"))?;
    Ok(binding
        .lease_started_at
        .saturating_add(duration.saturating_mul(2).div_ceil(3)))
}

fn renewal_is_open(binding: &RenewalBindingV1, finalized_timestamp: u64) -> Result<bool> {
    if finalized_timestamp >= binding.valid_until {
        return Ok(true);
    }
    Ok(finalized_timestamp >= renewal_opens_at(binding)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::RawRelayTransactionV1;
    use alloy_primitives::Address;
    use outbe_primitives::tee_attestation_v1::{AttestationMode, NodeIdV1};

    fn binding() -> RenewalBindingV1 {
        RenewalBindingV1 {
            node_id_hash: B256::repeat_byte(1),
            enclave_id: B256::repeat_byte(2),
            binding_id: B256::repeat_byte(3),
            intent_hash: B256::repeat_byte(4),
            evidence_hash: B256::repeat_byte(5),
            policy_hash: B256::repeat_byte(6),
            binding_version: 1,
            registration_version: 1,
            renewal_nonce: 1,
            transition_nonce: 0,
            lease_started_at: 100,
            valid_until: 400,
            collateral_valid_until: 500,
            recipient_x25519: B256::repeat_byte(7),
            attestation_ed25519: B256::repeat_byte(8),
            noise_responder_x25519: B256::repeat_byte(9),
            mrenclave: B256::repeat_byte(10),
            mrsigner: B256::repeat_byte(11),
            isv_prod_id: 1,
            isv_svn: 1,
            platform_tcb_status: 0,
            verdict_hash: B256::repeat_byte(12),
            node_host_authorization_hash: B256::repeat_byte(13),
        }
    }

    fn target_attempt() -> (PreparedRenewalV1, RenewalBindingV1) {
        let public: [u8; 33] = k256::ecdsa::SigningKey::from_bytes((&[1; 32]).into())
            .unwrap()
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap();
        let intent = RegistrationIntentV1 {
            chain_id: [1; 32],
            genesis_hash: B256::repeat_byte(20),
            operation: AttestationOperationV1::RenewEnclave,
            attestation_mode: AttestationMode::DcapRequired,
            policy_hash: B256::repeat_byte(6),
            node_id: NodeIdV1 {
                reth_p2p_public: public,
            },
            enclave_id: B256::repeat_byte(2),
            binding_id: B256::repeat_byte(3),
            binding_version: 1,
            registration_version: 2,
            renewal_nonce: 2,
            transition_nonce: 0,
            requested_valid_until: 700,
            recipient_x25519: [7; 32],
            attestation_ed25519: [8; 32],
            noise_responder_x25519: [9; 32],
            node_host_authorization_hash: B256::repeat_byte(13),
        };
        let intent_hash = intent.intent_hash().unwrap();
        let evidence_hash = B256::repeat_byte(30);
        let attempt = PreparedRenewalV1 {
            source: binding(),
            intent: intent.encode_canonical().unwrap(),
            intent_hash,
            evidence: vec![1],
            evidence_hash,
            node_signature: vec![1; 65],
            enclave_signature: vec![2; 64],
            calldata: vec![3],
            calldata_hash: keccak256([3]),
            requested_valid_until: 700,
            collateral_valid_until: 800,
            collateral_margin: 10,
            relay: Address::repeat_byte(40),
            relay_variants: vec![RawRelayTransactionV1 {
                relay: Address::repeat_byte(40),
                chain_id: 1,
                account_nonce: 1,
                gas_price: U256::from(1),
                gas_limit: 1,
                calldata_hash: keccak256([3]),
                raw_transaction: vec![4],
                transaction_hash: keccak256([4]),
            }],
        };
        let mut target = binding();
        target.intent_hash = intent_hash;
        target.evidence_hash = evidence_hash;
        target.registration_version = 2;
        target.renewal_nonce = 2;
        target.valid_until = 700;
        (attempt, target)
    }

    #[test]
    fn renewal_opens_at_the_exact_registry_final_third_boundary() {
        let binding = binding();
        assert_eq!(renewal_opens_at(&binding).unwrap(), 300);
        assert!(!renewal_is_open(&binding, 299).unwrap());
        assert!(renewal_is_open(&binding, 300).unwrap());
        assert!(renewal_is_open(&binding, 400).unwrap());
    }

    #[test]
    fn abandon_requires_finalized_time_to_reach_an_irrecoverable_ceiling() {
        let (attempt, _) = target_attempt();
        assert_eq!(permanent_staleness(&attempt, 699), None);
        assert!(permanent_staleness(&attempt, 700)
            .unwrap()
            .contains("lease expiration"));
        let mut collateral_first = attempt;
        collateral_first.requested_valid_until = 900;
        assert!(permanent_staleness(&collateral_first, 800)
            .unwrap()
            .contains("collateral expiration"));
    }

    #[test]
    fn finalization_requires_the_exact_intent_evidence_and_next_counters() {
        let (attempt, target) = target_attempt();
        assert!(target_matches(&target, &attempt).unwrap());
        let mut wrong_nonce = target.clone();
        wrong_nonce.renewal_nonce += 1;
        assert!(!target_matches(&wrong_nonce, &attempt).unwrap());
        let mut wrong_evidence = target;
        wrong_evidence.evidence_hash = B256::repeat_byte(31);
        assert!(!target_matches(&wrong_evidence, &attempt).unwrap());
    }

    #[test]
    fn a_third_registry_state_is_a_conflict_not_a_rebuild_authority() {
        let source = binding();
        assert!(ensure_source_or_conflict(&source, &source).is_ok());
        let mut third = source.clone();
        third.binding_id = B256::repeat_byte(99);
        assert!(ensure_source_or_conflict(&third, &source).is_err());
    }

    #[test]
    fn already_known_is_the_only_replay_error_treated_as_delivery() {
        assert!(transaction_is_already_known(&eyre::eyre!("already known")));
        assert!(transaction_is_already_known(&eyre::eyre!(
            "known transaction"
        )));
        assert!(!transaction_is_already_known(&eyre::eyre!("nonce too low")));
    }
}
