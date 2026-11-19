//! Purpose-bound evidence and canonical zero-value EIP-1559 service envelopes.
use super::super::*;
use super::{renewal_schedule_v1, RenewalScheduleError, RenewalStep, RenewalUpgradePhaseV1};
use crate::{
    in_process::{InProcessError, InProcessTeeContext},
    registry_view::{NodeBindingSelectorV1, RenewalBindingV1},
};
use alloy_consensus::{SignableTransaction, TxEip1559};
use alloy_primitives::{keccak256, Address, Signature, TxKind, B256, U256};
use alloy_sol_types::SolCall as _;
use outbe_primitives::{tee_attestation_v1::*, tee_registry_abi_v1::ITeeRegistryV1};

/// Runtime owns both keys; reducers never receive or persist their secret bytes.
pub trait LifecycleSignerV1 {
    type Error: std::error::Error + Send + Sync + 'static;
    fn address(&self) -> Address;
    fn sign_node_hash(&self, hash: B256) -> Result<[u8; 65], Self::Error>;
    fn sign_transaction_hash(&self, hash: B256) -> Result<Signature, Self::Error>;
}

#[derive(Debug, thiserror::Error)]
pub enum RenewalError<E: std::error::Error + 'static, S: std::error::Error + 'static> {
    #[error(transparent)]
    Port(#[from] LifecyclePortError<E>),
    #[error(transparent)]
    Journal(#[from] ServiceTxJournalError),
    #[error(transparent)]
    Reconcile(#[from] ReconcileError<E>),
    #[error(transparent)]
    Schedule(#[from] RenewalScheduleError),
    #[error("finalized renewal schedule rejected")]
    FinalizedSchedule(#[source] outbe_primitives::errors::TeeRenewalScheduleError),
    #[error(transparent)]
    Resident(#[from] InProcessError),
    #[error("canonical lifecycle data rejected")]
    Codec(#[from] CodecError),
    #[error("attestation collateral acquisition failed")]
    Collateral(#[source] crate::TransportError),
    #[error("DCAP collateral rejected: {0:?}")]
    Dcap(crate::dcap_protocol::DcapRejectCodeV1),
    #[error("signature failed")]
    Signing(#[source] S),
    #[error("finalized view or resident identity mismatch")]
    BindingMismatch,
    #[error("upgrade proof checkpoint decode rejected")]
    UpgradeProofCheckpoint(#[source] UpgradeProofCheckpointError),
    #[error("service counter overflow")]
    CounterOverflow,
    #[error("collateral does not cover the requested lease")]
    CollateralWindow {
        issue_floor: u64,
        expiration_ceiling: u64,
    },
    #[error("fee does not fit EIP-1559")]
    FeeOverflow,
    #[error("another lifecycle operation owns the pending journal")]
    PendingOtherOperation,
    #[error("port returned another transaction hash")]
    SubmittedHashMismatch,
    #[error("pending transaction is no longer safe to replay")]
    StalePending,
    #[error("pending renewal calldata ABI rejected")]
    PendingCalldata(#[source] alloy_sol_types::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum UpgradeProofCheckpointError {
    #[error("upgrade proof admission witness rejected")]
    Witness(#[source] crate::finalized_admission::FinalizedAdmissionCodecError),
    #[error("upgrade proof header rejected")]
    Header(#[source] alloy_rlp::Error),
}

pub(crate) type StepResult<T, P, S> =
    Result<T, RenewalError<<P as LifecycleChainPort>::Error, <S as LifecycleSignerV1>::Error>>;

/// One bounded tick; never sleeps, changes role or discards the resident key.
pub async fn renew_once_v1<P: LifecycleChainPort, S: LifecycleSignerV1>(
    port: &P,
    signer: &S,
    live: &InProcessTeeContext,
    journal: &mut ServiceTxJournalV1,
    upgrade: RenewalUpgradePhaseV1,
) -> StepResult<RenewalStep, P, S> {
    let at = port.finalized_checkpoint().await?;
    let view = port
        .registry_state(at, &NodeBindingSelectorV1::Validator(signer.address()))
        .await?;
    validate_view::<P::Error, S::Error>(&view, at, live)?;
    let binding = view.binding.as_ref().ok_or(RenewalError::BindingMismatch)?;
    // This guard precedes even replay of a previously signed renewal.
    if at.timestamp >= binding.valid_until {
        return expired_renewal::<P, S>(port, signer, live, journal, &view).await;
    }
    let pending = journal.load()?;
    if pending.as_ref().is_some_and(|r| {
        r.state == ServiceTxStateV1::Pending
            && (r.kind != ServiceTxKindV1::Renew || r.sender != signer.address())
    }) {
        return Err(RenewalError::PendingOtherOperation);
    }
    let phase_allows = match upgrade {
        RenewalUpgradePhaseV1::Idle | RenewalUpgradePhaseV1::CandidatePrepared => true,
        RenewalUpgradePhaseV1::Promoted {
            candidate_manifest_hash,
            successor_policy_hash,
        } => {
            live.manifest().authorization_hash()? == candidate_manifest_hash
                && binding.policy_hash == successor_policy_hash
        }
        RenewalUpgradePhaseV1::KeyProvisioned
        | RenewalUpgradePhaseV1::CandidateKeyReady
        | RenewalUpgradePhaseV1::SubmissionPrepared
        | RenewalUpgradePhaseV1::Submitted
        | RenewalUpgradePhaseV1::Finalized
        | RenewalUpgradePhaseV1::TerminalMissedCutoff => false,
    };
    let retired = view.active_upgrade.is_some_and(|u| {
        at.height >= u.activation_height && u.successor_policy_hash != binding.policy_hash
    });
    if !phase_allows || retired {
        return Ok(RenewalStep::BlockedByActiveUpgrade);
    }
    validate_binding::<P::Error, S::Error>(binding, live, &view.policy)?;
    let nonce = match renewal_action::<P, S>(port, signer, live, journal, &view).await? {
        RenewalAction::Stop(step) => return Ok(step),
        RenewalAction::Replay(record) => return broadcast::<P, S>(port, &record, true).await,
        RenewalAction::Build(nonce) => nonce,
    };
    let intent = renewal_intent_v1::<P::Error, S::Error>(live, binding, &view.policy)?;
    let (evidence, enclave_signature) =
        evidence_v1::<P::Error, S::Error>(live, &intent, &view.policy, at.timestamp)?;
    let node_signature = signer
        .sign_node_hash(intent.intent_hash()?)
        .map_err(RenewalError::Signing)?;
    verify_node_signature::<P::Error, S::Error>(&intent, &node_signature)?;
    let calldata = ITeeRegistryV1::renewEnclaveCall {
        evidence: evidence.into(),
        nodeSignature: node_signature.to_vec().into(),
        enclaveSignature: enclave_signature.to_vec().into(),
    }
    .abi_encode();
    let record = signed_record::<P, S>(
        signer,
        at,
        nonce,
        ServiceCallV1 {
            gas_limit: renewal_gas_limit_v1()?,
            calldata,
            kind: ServiceTxKindV1::Renew,
            intent: ServiceTxIntentV1 {
                target: target_v1(&intent)?,
                candidate: None,
            },
        },
    )?;
    journal.begin(record.clone())?;
    broadcast::<P, S>(port, &record, false).await
}

pub(crate) fn validate_view<E: std::error::Error + 'static, S: std::error::Error + 'static>(
    view: &LifecycleRegistryStateV1,
    at: LifecycleCheckpointV1,
    live: &InProcessTeeContext,
) -> Result<(), RenewalError<E, S>> {
    let manifest = live.manifest();
    let pinned = (
        view.schedule.finalized_height,
        view.schedule.finalized_hash,
        view.schedule.finalized_timestamp,
    ) == (at.height, at.hash, at.timestamp);
    let chain = (U256::from(at.chain_id).to_be_bytes::<32>(), at.genesis_hash)
        == (manifest.chain_id, manifest.genesis_hash);
    let policy = (view.policy.chain_id, view.policy.genesis_hash)
        == (manifest.chain_id, manifest.genesis_hash);
    let valid = [
        view.checkpoint == at,
        pinned,
        !at.hash.is_zero(),
        !at.state_root.is_zero(),
        chain,
        policy,
    ]
    .into_iter()
    .all(|v| v);
    if !valid {
        return Err(RenewalError::BindingMismatch);
    }
    view.schedule
        .validate()
        .map_err(RenewalError::FinalizedSchedule)?;
    Ok(())
}

fn validate_binding<E: std::error::Error + 'static, S: std::error::Error + 'static>(
    binding: &RenewalBindingV1,
    live: &InProcessTeeContext,
    policy: &TeePolicyV1,
) -> Result<(), RenewalError<E, S>> {
    let m = live.manifest();
    let identity = (
        binding.node_id_hash,
        binding.enclave_id,
        binding.node_host_authorization_hash,
        binding.policy_hash,
    ) == (
        m.node_id.node_id_hash()?,
        m.enclave_id()?,
        m.node_host_authorization_hash()?,
        policy.policy_hash()?,
    );
    let keys = (
        binding.recipient_x25519,
        binding.attestation_ed25519,
        binding.noise_responder_x25519,
    ) == (
        B256::from(m.recipient_x25519),
        B256::from(m.attestation_ed25519),
        B256::from(m.noise_responder_x25519),
    );
    if !identity || !keys {
        return Err(RenewalError::BindingMismatch);
    }
    Ok(())
}

pub(crate) fn target_v1(
    intent: &RegistrationIntentV1,
) -> Result<ServiceBindingTargetV1, CodecError> {
    Ok(ServiceBindingTargetV1 {
        node_id_hash: intent.node_id.node_id_hash()?,
        enclave_id: intent.enclave_id,
        binding_id: intent.binding_id,
        binding_version: intent.binding_version,
        registration_version: intent.registration_version,
        renewal_nonce: intent.renewal_nonce,
        transition_nonce: intent.transition_nonce,
        valid_until: intent.requested_valid_until,
        policy_hash: intent.policy_hash,
        recipient_x25519: intent.recipient_x25519.into(),
    })
}

fn renewal_intent_v1<E: std::error::Error + 'static, S: std::error::Error + 'static>(
    live: &InProcessTeeContext,
    b: &RenewalBindingV1,
    p: &TeePolicyV1,
) -> Result<RegistrationIntentV1, RenewalError<E, S>> {
    let m = live.manifest();
    Ok(RegistrationIntentV1 {
        chain_id: m.chain_id,
        genesis_hash: m.genesis_hash,
        operation: AttestationOperationV1::RenewEnclave,
        attestation_mode: p.attestation_mode,
        policy_hash: p.policy_hash()?,
        node_id: m.node_id.clone(),
        enclave_id: b.enclave_id,
        binding_id: b.binding_id,
        binding_version: b.binding_version,
        registration_version: b
            .registration_version
            .checked_add(1)
            .ok_or(RenewalError::CounterOverflow)?,
        renewal_nonce: b
            .renewal_nonce
            .checked_add(1)
            .ok_or(RenewalError::CounterOverflow)?,
        transition_nonce: b.transition_nonce,
        requested_valid_until: b
            .valid_until
            .checked_add(p.maximum_lease)
            .ok_or(RenewalError::CounterOverflow)?,
        recipient_x25519: m.recipient_x25519,
        attestation_ed25519: m.attestation_ed25519,
        noise_responder_x25519: m.noise_responder_x25519,
        node_host_authorization_hash: b.node_host_authorization_hash,
    })
}

pub(crate) fn evidence_v1<E: std::error::Error + 'static, S: std::error::Error + 'static>(
    live: &InProcessTeeContext,
    intent: &RegistrationIntentV1,
    policy: &TeePolicyV1,
    now: u64,
) -> Result<(Vec<u8>, [u8; 64]), RenewalError<E, S>> {
    let (evidence, signature) = match policy.attestation_mode {
        AttestationMode::NoAttestationDev => {
            let signature = live.sign_registration_intent_dev_v1(intent)?;
            let proof = if intent.operation == AttestationOperationV1::TransitionEnclaveMeasurement
            {
                Some(live.transition_key_ready_proof_v1(intent)?)
            } else {
                None
            };
            (
                AttestationEvidenceV1::NoAttestationDev(NoAttestationDevEvidenceV1 {
                    intent: intent.clone(),
                    dev_attestation_public: intent.attestation_ed25519,
                    dev_signature: signature,
                    transition_key_ready_proof: proof,
                }),
                signature,
            )
        }
        AttestationMode::DcapRequired => {
            let generated = live.generate_dcap_quote(intent)?;
            let components = crate::acquire_dcap_collateral_v1(&generated.quote_body)
                .map_err(RenewalError::Collateral)?;
            let dcap = DcapEvidenceV1 {
                intent: intent.clone(),
                quote: generated.quote_body,
                components,
                transition_key_ready_proof: generated.transition_key_ready_proof,
            };
            let window = crate::dcap_collateral_validity_window_v1(&dcap, policy)
                .map_err(RenewalError::Dcap)?;
            let ceiling = window
                .expiration_ceiling
                .checked_sub(policy.collateral_margin)
                .ok_or(RenewalError::CollateralWindow {
                    issue_floor: window.issue_floor,
                    expiration_ceiling: window.expiration_ceiling,
                })?;
            if window.issue_floor > now || intent.requested_valid_until > ceiling {
                return Err(RenewalError::CollateralWindow {
                    issue_floor: window.issue_floor,
                    expiration_ceiling: window.expiration_ceiling,
                });
            }
            (
                AttestationEvidenceV1::Dcap(dcap),
                generated.enclave_signature,
            )
        }
    };
    if !intent.verify_enclave_signature(&signature) {
        return Err(RenewalError::BindingMismatch);
    }
    Ok((evidence.encode_canonical()?, signature))
}

/// Exact envelope ceiling used by RenewEnclaveHook, not the smaller per-call estimate.
pub fn renewal_gas_limit_v1() -> Result<u64, CodecError> {
    TeeRegistryGasScheduleV1::normative().maximum_transaction_gas(
        RegistryMutatorV1::RenewEnclave,
        MAX_ATTESTATION_EVIDENCE_BYTES + MAX_EVIDENCE_CALL_FRAMING_BYTES,
        MAX_ATTESTATION_EVIDENCE_BYTES,
        MAX_ACTIVE_MEASUREMENT_RULES,
        AttestationMode::DcapRequired,
    )
}

pub(crate) struct ServiceCallV1 {
    pub gas_limit: u64,
    pub calldata: Vec<u8>,
    pub kind: ServiceTxKindV1,
    pub intent: ServiceTxIntentV1,
}

pub(crate) fn signed_record<P: LifecycleChainPort, S: LifecycleSignerV1>(
    signer: &S,
    at: LifecycleCheckpointV1,
    nonce: u64,
    call: ServiceCallV1,
) -> StepResult<ServiceTxRecordV1, P, S> {
    let ServiceCallV1 {
        gas_limit: gas,
        calldata,
        kind,
        intent: public_intent,
    } = call;
    let min_fee = alloy_consensus::private::alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;
    let fee = u128::try_from(at.base_fee_per_gas.max(U256::from(min_fee)))
        .map_err(|_| RenewalError::FeeOverflow)?;
    let tx = TxEip1559 {
        chain_id: at.chain_id,
        nonce,
        gas_limit: gas,
        max_fee_per_gas: fee,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(outbe_primitives::addresses::TEE_REGISTRY_ADDRESS),
        value: U256::ZERO,
        input: calldata.into(),
        access_list: Default::default(),
    };
    let signature = signer
        .sign_transaction_hash(tx.signature_hash())
        .map_err(RenewalError::Signing)?;
    let mut raw = vec![2];
    tx.into_signed(signature).rlp_encode(&mut raw);
    let record = ServiceTxRecordV1 {
        tx_hash: keccak256(&raw),
        raw_signed_tx: raw.into(),
        sender: signer.address(),
        nonce,
        kind,
        public_intent,
        finalized_checkpoint: at,
        state: ServiceTxStateV1::Pending,
        reconciled_checkpoint: None,
    };
    record.validate()?;
    Ok(record)
}

fn validate_pending<E: std::error::Error + 'static, S: std::error::Error + 'static>(
    record: &ServiceTxRecordV1,
    view: &LifecycleRegistryStateV1,
) -> Result<(), RenewalError<E, S>> {
    let mut bytes = &record.raw_signed_tx[1..];
    use alloy_consensus::transaction::RlpEcdsaDecodableTx as _;
    let signed = TxEip1559::rlp_decode_signed(&mut bytes).map_err(ServiceTxJournalError::from)?;
    let call = ITeeRegistryV1::renewEnclaveCall::abi_decode(&signed.tx().input)
        .map_err(RenewalError::PendingCalldata)?;
    let evidence = AttestationEvidenceV1::decode_canonical(&call.evidence)?;
    if evidence.intent().operation != AttestationOperationV1::RenewEnclave
        || view.checkpoint.timestamp >= evidence.intent().requested_valid_until
    {
        return Err(RenewalError::StalePending);
    }
    if let AttestationEvidenceV1::Dcap(dcap) = evidence {
        let window = crate::dcap_collateral_validity_window_v1(&dcap, &view.policy)
            .map_err(RenewalError::Dcap)?;
        if view.checkpoint.timestamp >= window.expiration_ceiling {
            return Err(RenewalError::StalePending);
        }
    }
    Ok(())
}

pub(crate) async fn broadcast<P: LifecycleChainPort, S: LifecycleSignerV1>(
    port: &P,
    record: &ServiceTxRecordV1,
    replayed: bool,
) -> StepResult<RenewalStep, P, S> {
    match port.send_raw_transaction(&record.raw_signed_tx).await {
        Ok(hash) if hash == record.tx_hash => Ok(RenewalStep::Submitted {
            transaction_hash: hash,
            replayed,
        }),
        Ok(_) => Err(RenewalError::SubmittedHashMismatch),
        Err(
            LifecyclePortError::NonceTooLow { .. }
            | LifecyclePortError::TransactionDisplaced { .. },
        ) => Ok(RenewalStep::AwaitingFinality),
        Err(error) => Err(error.into()),
    }
}

enum RenewalAction {
    Stop(RenewalStep),
    Replay(Box<ServiceTxRecordV1>),
    Build(u64),
}
async fn renewal_action<P: LifecycleChainPort, S: LifecycleSignerV1>(
    port: &P,
    signer: &S,
    live: &InProcessTeeContext,
    journal: &mut ServiceTxJournalV1,
    view: &LifecycleRegistryStateV1,
) -> StepResult<RenewalAction, P, S> {
    let pending = journal.load()?;
    let reconciliation = reconcile_service_tx_v1(port, journal, live).await?;
    match reconciliation {
        ServiceTxReconciliationV1::WaitForFinality { .. } => {
            Ok(RenewalAction::Stop(RenewalStep::AwaitingFinality))
        }
        ServiceTxReconciliationV1::ReplayExact => {
            let record = pending.ok_or(RenewalError::StalePending)?;
            validate_pending::<P::Error, S::Error>(&record, view)?;
            Ok(RenewalAction::Replay(Box::new(record)))
        }
        ServiceTxReconciliationV1::ReplaceLostIdentity { nonce }
        | ServiceTxReconciliationV1::RebuildAndResign { nonce } => Ok(RenewalAction::Build(nonce)),
        ServiceTxReconciliationV1::IngestOnboarding => {
            if !live.offer_key_ready()? {
                return Err(RenewalError::PendingOtherOperation);
            }
            scheduled_action::<P, S>(port, signer, view).await
        }
        ServiceTxReconciliationV1::Empty
        | ServiceTxReconciliationV1::Completed
        | ServiceTxReconciliationV1::DiscardLostIdentity { .. } => {
            scheduled_action::<P, S>(port, signer, view).await
        }
    }
}

async fn scheduled_action<P: LifecycleChainPort, S: LifecycleSignerV1>(
    port: &P,
    signer: &S,
    view: &LifecycleRegistryStateV1,
) -> StepResult<RenewalAction, P, S> {
    let at = view.checkpoint;
    let binding = view.binding.as_ref().ok_or(RenewalError::BindingMismatch)?;
    let due = renewal_schedule_v1(at.timestamp, binding.valid_until, view.policy.maximum_lease)?;
    if let Some(step) = due {
        return Ok(RenewalAction::Stop(step));
    }
    Ok(RenewalAction::Build(
        port.account_nonce(signer.address(), LifecycleNonceTagV1::Pending)
            .await?,
    ))
}

fn verify_node_signature<E: std::error::Error + 'static, S: std::error::Error + 'static>(
    intent: &RegistrationIntentV1,
    signature: &[u8; 65],
) -> Result<(), RenewalError<E, S>> {
    if intent.verify_node_signature(signature) {
        Ok(())
    } else {
        Err(RenewalError::BindingMismatch)
    }
}

/// Release only an expired own renewal whose nonce is finalized or absent from both heads.
async fn expired_renewal<P: LifecycleChainPort, S: LifecycleSignerV1>(
    port: &P,
    signer: &S,
    live: &InProcessTeeContext,
    journal: &mut ServiceTxJournalV1,
    view: &LifecycleRegistryStateV1,
) -> StepResult<RenewalStep, P, S> {
    let record = journal.load()?;
    if let Some(record) = record.filter(|r| {
        r.state == ServiceTxStateV1::Pending
            && r.kind == ServiceTxKindV1::Renew
            && r.sender == signer.address()
    }) {
        let manifest = live.manifest();
        if record.process_identity()? != (manifest.enclave_id()?, manifest.recipient_x25519.into())
        {
            return Err(RenewalError::BindingMismatch);
        }
        let binding = view.binding.as_ref().ok_or(RenewalError::BindingMismatch)?;
        validate_binding::<P::Error, S::Error>(binding, live, &view.policy)?;
        let at = view.checkpoint;
        let finalized = port
            .account_nonce(signer.address(), LifecycleNonceTagV1::Finalized(at))
            .await?;
        let latest = port
            .account_nonce(signer.address(), LifecycleNonceTagV1::Latest)
            .await?;
        let pending = port
            .account_nonce(signer.address(), LifecycleNonceTagV1::Pending)
            .await?;
        if finalized <= record.nonce && (latest > record.nonce || pending > record.nonce) {
            return Ok(RenewalStep::AwaitingFinality);
        }
        let state = if record.public_intent.target.matches(binding) {
            ServiceTxStateV1::Completed
        } else {
            ServiceTxStateV1::RebuildRequired
        };
        journal.finish(record.tx_hash, state, at)?;
    }
    Ok(RenewalStep::LeaseAlreadyExpiredNeedsRejoin)
}
