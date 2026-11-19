use super::super::*;
use super::*;
use alloy_consensus::{transaction::RlpEcdsaDecodableTx, SignableTransaction};
use alloy_primitives::{keccak256, TxKind};
use alloy_sol_types::SolCall;
use outbe_primitives::{
    addresses::TEE_REGISTRY_ADDRESS, tee_attestation_v1::*, tee_registry_abi_v1::ITeeRegistryV1,
};

/// The runtime supplies its stable operator and NodeHost signers; secrets never cross this API.
pub trait EnrollmentSignerV1 {
    type Error: std::error::Error + Send + Sync + 'static;
    fn sign_operator_hash(&self, hash: B256) -> Result<Signature, Self::Error>;
    fn sign_node_hash(&self, hash: B256) -> Result<[u8; 65], Self::Error>;
}

pub(super) struct SubmissionContext<'a> {
    pub live: &'a InProcessTeeContext,
    pub config: EnrollmentConfigV1,
    pub view: &'a EnrollmentPreflightV1,
}

pub(super) async fn advance<P: LifecycleChainPort, S: EnrollmentSignerV1>(
    port: &P,
    signer: &S,
    journal: &mut ServiceTxJournalV1,
    context: SubmissionContext<'_>,
    reconciliation: ServiceTxReconciliationV1,
) -> Result<EnrollmentOutcome, EnrollmentError<P::Error, S::Error>> {
    let SubmissionContext { live, config, view } = context;
    if let Some(record) = journal.load()? {
        if record.sender != config.sender || blocks_fresh_register(&record) {
            return Err(EnrollmentError::WrongServiceTransaction);
        }
        // A terminal RebuildRequired Renew or UpgradeCancel is not a registration
        // predecessor. submit_fresh replaces it. A live Pending non-Register does not.
        if record.kind == ServiceTxKindV1::Register
            && record.public_intent.target.enclave_id == live.manifest().enclave_id()?
            && matches!(
                record.state,
                ServiceTxStateV1::Pending | ServiceTxStateV1::RebuildRequired
            )
        {
            let intent = pending_intent(&record, live, config.sender)?;
            check_predecessor(view, &intent)?;
        }
    }
    let reconciled_nonce = match reconciliation {
        ServiceTxReconciliationV1::WaitForFinality { .. } => {
            return Ok(EnrollmentOutcome::WaitForFinality)
        }
        ServiceTxReconciliationV1::ReplayExact => {
            let record = journal
                .load()?
                .ok_or(EnrollmentError::WrongServiceTransaction)?;
            if view.balance < record_budget(&record)? {
                return Ok(EnrollmentOutcome::WaitForGasBalance);
            }
            return broadcast(port, journal, live, record).await;
        }
        ServiceTxReconciliationV1::ReplaceLostIdentity { nonce }
        | ServiceTxReconciliationV1::RebuildAndResign { nonce } => Some(nonce),
        ServiceTxReconciliationV1::Empty
        | ServiceTxReconciliationV1::Completed
        | ServiceTxReconciliationV1::DiscardLostIdentity { .. } => None,
        ServiceTxReconciliationV1::IngestOnboarding => {
            let enclave_id = live.manifest().enclave_id()?;
            let own_expired = view.registry.binding.as_ref().is_some_and(|b| {
                b.enclave_id == enclave_id && view.registry.checkpoint.timestamp >= b.valid_until
            });
            if !own_expired {
                return Err(EnrollmentError::IdentityMismatch);
            }
            None
        }
    };
    submit_fresh(port, signer, journal, context, reconciled_nonce).await
}

async fn submit_fresh<P: LifecycleChainPort, S: EnrollmentSignerV1>(
    port: &P,
    signer: &S,
    journal: &mut ServiceTxJournalV1,
    context: SubmissionContext<'_>,
    reconciled_nonce: Option<u64>,
) -> Result<EnrollmentOutcome, EnrollmentError<P::Error, S::Error>> {
    let SubmissionContext { live, config, view } = context;
    let intent = build_intent::<P::Error, S::Error>(live, config, view)?;
    // Zero-length evidence/calldata bounds only the unavoidable fixed protocol charges.
    let min_gas = TeeRegistryGasScheduleV1::normative().maximum_transaction_gas(
        RegistryMutatorV1::RegisterEnclave,
        0,
        0,
        view.registry.policy.measurement_rules.len(),
        intent.attestation_mode,
    )?;
    if view.balance < U256::from(min_gas) * U256::from(config.max_fee_per_gas) {
        return Ok(EnrollmentOutcome::WaitForGasBalance);
    }
    let nonce = if let Some(nonce) = reconciled_nonce {
        nonce
    } else {
        port.account_nonce(config.sender, LifecycleNonceTagV1::Pending)
            .await?
    };
    let record = prepare(signer, &context, intent, nonce)?;
    if view.balance < record_budget(&record)? {
        return Ok(EnrollmentOutcome::WaitForGasBalance);
    }
    journal.begin(record.clone())?;
    broadcast(port, journal, live, record).await
}

fn record_budget<E: std::error::Error + 'static, S: std::error::Error + 'static>(
    record: &ServiceTxRecordV1,
) -> Result<U256, EnrollmentError<E, S>> {
    let mut bytes = &record.raw_signed_tx[1..];
    let signed = TxEip1559::rlp_decode_signed(&mut bytes)?;
    Ok(U256::from(signed.tx().gas_limit) * U256::from(signed.tx().max_fee_per_gas))
}

async fn broadcast<P: LifecycleChainPort, S: std::error::Error + 'static>(
    port: &P,
    journal: &mut ServiceTxJournalV1,
    live: &InProcessTeeContext,
    record: ServiceTxRecordV1,
) -> Result<EnrollmentOutcome, EnrollmentError<P::Error, S>> {
    match port.send_raw_transaction(&record.raw_signed_tx).await {
        Ok(hash) if hash == record.tx_hash => Ok(EnrollmentOutcome::Submitted { tx_hash: hash }),
        Ok(_) => Err(EnrollmentError::TransactionHashMismatch),
        Err(
            LifecyclePortError::NonceTooLow { .. }
            | LifecyclePortError::TransactionDisplaced { .. },
        ) => {
            // Do not loop or rebuild against moving latest state; the next tick preflights again.
            reconcile_service_tx_v1(port, journal, live).await?;
            Ok(EnrollmentOutcome::WaitForFinality)
        }
        Err(error) => Err(error.into()),
    }
}

pub(super) fn build_intent<E: std::error::Error + 'static, S: std::error::Error + 'static>(
    live: &InProcessTeeContext,
    config: EnrollmentConfigV1,
    view: &EnrollmentPreflightV1,
) -> Result<RegistrationIntentV1, EnrollmentError<E, S>> {
    if config.binding_id.is_zero()
        || config.valid_until <= view.registry.checkpoint.timestamp
        || config.max_priority_fee_per_gas > config.max_fee_per_gas
        || U256::from(config.max_fee_per_gas) < view.registry.checkpoint.base_fee_per_gas
    {
        return Err(EnrollmentError::InvalidConfiguration);
    }
    let m = live.manifest();
    let previous = view.registry.binding.as_ref();
    let increment = |v: u64| v.checked_add(1).ok_or(EnrollmentError::CounterExhausted);
    Ok(RegistrationIntentV1 {
        chain_id: m.chain_id,
        genesis_hash: m.genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: m.attestation_mode,
        policy_hash: view.registry.policy.policy_hash()?,
        node_id: m.node_id.clone(),
        enclave_id: m.enclave_id()?,
        binding_id: config.binding_id,
        binding_version: increment(previous.map_or(0, |b| b.binding_version))?,
        registration_version: previous.map_or(Ok(0), |b| increment(b.registration_version))?,
        renewal_nonce: previous.map_or(0, |b| b.renewal_nonce),
        transition_nonce: previous.map_or(0, |b| b.transition_nonce),
        requested_valid_until: config.valid_until,
        recipient_x25519: m.recipient_x25519,
        attestation_ed25519: m.attestation_ed25519,
        noise_responder_x25519: m.noise_responder_x25519,
        node_host_authorization_hash: m.node_host_authorization_hash()?,
    })
}

fn prepare<E: std::error::Error + 'static, S: EnrollmentSignerV1>(
    signer: &S,
    context: &SubmissionContext<'_>,
    intent: RegistrationIntentV1,
    nonce: u64,
) -> Result<ServiceTxRecordV1, EnrollmentError<E, S::Error>> {
    let SubmissionContext { live, config, view } = *context;
    let hash = intent.intent_hash()?;
    let node_signature = signer
        .sign_node_hash(hash)
        .map_err(EnrollmentError::Signer)?;
    if !intent.verify_node_signature(&node_signature) {
        return Err(EnrollmentError::InvalidSignature);
    }
    let (evidence, enclave_signature) = match intent.attestation_mode {
        AttestationMode::NoAttestationDev => {
            let signature = live.sign_registration_intent_dev_v1(&intent)?;
            (
                AttestationEvidenceV1::NoAttestationDev(NoAttestationDevEvidenceV1 {
                    intent: intent.clone(),
                    dev_attestation_public: live.attestation_public(),
                    dev_signature: signature,
                    transition_key_ready_proof: None,
                }),
                signature,
            )
        }
        AttestationMode::DcapRequired => {
            let quote = live.generate_dcap_quote(&intent)?;
            let components = crate::acquire_dcap_collateral_v1(&quote.quote_body)?;
            (
                AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
                    intent: intent.clone(),
                    quote: quote.quote_body,
                    components,
                    transition_key_ready_proof: quote.transition_key_ready_proof,
                }),
                quote.enclave_signature,
            )
        }
    };
    let evidence = evidence.encode_canonical()?;
    let binding = ValidatorNodeBindingV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        validator: config.sender.into(),
        node_id_hash: intent.node_id.node_id_hash()?,
    };
    // Rejoin re-proves the same association; the registry checks that it is idempotent.
    let binding_hash = binding.binding_hash()?;
    let validator_signature = signer
        .sign_operator_hash(binding_hash)
        .map_err(EnrollmentError::Signer)?
        .as_bytes();
    let node_binding_signature = signer
        .sign_node_hash(binding_hash)
        .map_err(EnrollmentError::Signer)?;
    if !binding.verify_validator_signature(&validator_signature)
        || !binding.verify_node_signature(&node_binding_signature)
    {
        return Err(EnrollmentError::InvalidSignature);
    }
    let call = ITeeRegistryV1::registerEnclaveCall {
        evidence: evidence.clone().into(),
        nodeSignature: node_signature.to_vec().into(),
        enclaveSignature: enclave_signature.to_vec().into(),
        validatorNodeBinding: binding.encode_canonical()?.into(),
        validatorSignature: validator_signature.to_vec().into(),
        nodeBindingSignature: node_binding_signature.to_vec().into(),
    }
    .abi_encode();
    let gas_limit = TeeRegistryGasScheduleV1::normative().maximum_transaction_gas(
        RegistryMutatorV1::RegisterEnclave,
        call.len(),
        evidence.len(),
        view.registry.policy.measurement_rules.len(),
        intent.attestation_mode,
    )?;
    let tx = TxEip1559 {
        chain_id: view.registry.checkpoint.chain_id,
        nonce,
        gas_limit,
        max_fee_per_gas: config.max_fee_per_gas,
        max_priority_fee_per_gas: config.max_priority_fee_per_gas,
        to: TxKind::Call(TEE_REGISTRY_ADDRESS),
        input: call.into(),
        ..Default::default()
    };
    let signature = signer
        .sign_operator_hash(tx.signature_hash())
        .map_err(EnrollmentError::Signer)?;
    let raw_signed_tx = signed_bytes(tx, signature);
    let record = ServiceTxRecordV1 {
        tx_hash: keccak256(&raw_signed_tx),
        raw_signed_tx,
        sender: config.sender,
        nonce,
        kind: ServiceTxKindV1::Register,
        public_intent: ServiceTxIntentV1 {
            target: ServiceBindingTargetV1 {
                node_id_hash: binding.node_id_hash,
                enclave_id: intent.enclave_id,
                binding_id: intent.binding_id,
                binding_version: intent.binding_version,
                registration_version: intent.registration_version,
                renewal_nonce: intent.renewal_nonce,
                transition_nonce: intent.transition_nonce,
                valid_until: intent.requested_valid_until,
                policy_hash: intent.policy_hash,
                recipient_x25519: intent.recipient_x25519.into(),
            },
            candidate: None,
        },
        finalized_checkpoint: view.registry.checkpoint,
        state: ServiceTxStateV1::Pending,
        reconciled_checkpoint: None,
    };
    record.validate()?;
    Ok(record)
}

/// A Pending Renew, UpgradeCancel, or any other non-Register still owns the journal.
/// Terminal RebuildRequired of those kinds is replaced by `submit_fresh`, not decoded here.
fn blocks_fresh_register(record: &ServiceTxRecordV1) -> bool {
    record.state == ServiceTxStateV1::Pending && record.kind != ServiceTxKindV1::Register
}

fn pending_intent<E: std::error::Error + 'static, S: std::error::Error + 'static>(
    record: &ServiceTxRecordV1,
    live: &InProcessTeeContext,
    sender: Address,
) -> Result<RegistrationIntentV1, EnrollmentError<E, S>> {
    if blocks_fresh_register(record)
        || record.kind != ServiceTxKindV1::Register
        || record.sender != sender
    {
        return Err(EnrollmentError::WrongServiceTransaction);
    }
    let mut bytes = &record.raw_signed_tx[1..];
    let tx = TxEip1559::rlp_decode_signed(&mut bytes)?;
    let call = ITeeRegistryV1::registerEnclaveCall::abi_decode(&tx.tx().input)?;
    let evidence = AttestationEvidenceV1::decode_canonical(&call.evidence)?;
    let intent = match evidence {
        AttestationEvidenceV1::NoAttestationDev(v) => v.intent,
        AttestationEvidenceV1::Dcap(v) => v.intent,
    };
    live.manifest().validate_intent_binding(&intent)?;
    Ok(intent)
}

fn check_predecessor<E: std::error::Error + 'static, S: std::error::Error + 'static>(
    view: &EnrollmentPreflightV1,
    intent: &RegistrationIntentV1,
) -> Result<(), EnrollmentError<E, S>> {
    if let Some(b) = view.registry.binding.as_ref() {
        let predecessor = (
            b.node_id_hash,
            b.binding_version.checked_add(1),
            b.registration_version.checked_add(1),
            b.renewal_nonce,
            b.transition_nonce,
        ) == (
            intent.node_id.node_id_hash()?,
            Some(intent.binding_version),
            Some(intent.registration_version),
            intent.renewal_nonce,
            intent.transition_nonce,
        );
        if !predecessor {
            return Err(EnrollmentError::CompetingLiveBinding);
        }
    }
    Ok(())
}
