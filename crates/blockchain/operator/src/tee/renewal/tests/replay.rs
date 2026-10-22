use outbe_primitives::tee_attestation_v1::TeePolicyV1;
use std::sync::{Arc, Mutex};

use super::super::*;
use crate::rpc::{ChainRpc, FinalizedStateRpc, RegistryRpc};
use alloy_primitives::{Address, Bytes};
use ed25519_dalek::Signer as _;
use outbe_primitives::{
    chain::DEVNET_CHAIN_ID,
    tee_attestation_v1::{
        AttestationMode, DcapCollateralComponentV1, DcapCollateralKind,
        EnclaveInitializationManifestV1, NodeIdV1,
    },
    tee_genesis_v1::{initial_tee_policy_v1, InitialTeeProfileV1, ProductionSgxMeasurementV1},
    tee_operator_v1::TeeRenewalScheduleV1,
    tee_registry_abi_v1::NodeEnclaveBindingV1View,
    tee_test_utils::sign_node_host_hash_for_test,
};

fn renewal_services<'a, R, E, N>(
    rpc: &'a R,
    evm_signer: &'a RelaySignerV1,
    enclave: &'a mut E,
    node_signer: &'a N,
) -> RenewalServicesV1<'a, R, E, N> {
    RenewalServicesV1 {
        rpc,
        evm_signer,
        enclave,
        node_signer,
    }
}

struct ReplayRpc {
    policy: outbe_primitives::tee_attestation_v1::TeePolicyV1,
    binding: RenewalBindingV1,
    schedule: TeeRenewalScheduleV1,
    receipt: Arc<Mutex<Option<serde_json::Value>>>,
    send_error: Option<&'static str>,
    receipt_after_send_error: Option<serde_json::Value>,
    sent: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl TransactionReceiptRpc for ReplayRpc {
    async fn transaction_receipt(
        &self,
        _transaction_hash: &str,
    ) -> Result<Option<serde_json::Value>> {
        Ok(self.receipt.lock().unwrap().clone())
    }
}

impl FinalizedStateRpc for ReplayRpc {
    async fn finalized_block(&self) -> Result<serde_json::Value> {
        Ok(serde_json::json!({
            "number": format!("0x{:x}", self.schedule.finalized_height),
            "timestamp": format!("0x{:x}", self.schedule.finalized_timestamp),
            "hash": format!("{:#x}", self.schedule.finalized_hash),
            "stateRoot": format!("{:#x}", B256::repeat_byte(0x81)),
        }))
    }

    async fn call_at(&self, _to: Address, data: &[u8], _block_tag: &str) -> Result<Vec<u8>> {
        if data.starts_with(&ITeeRegistryV1::activePolicyV1Call::SELECTOR) {
            let policy = Bytes::from(self.policy.encode_canonical().unwrap());
            return Ok(ITeeRegistryV1::activePolicyV1Call::abi_encode_returns(
                &policy,
            ));
        }
        if data.starts_with(&ITeeRegistryV1::nodeHostEnclaveBindingCall::SELECTOR) {
            let binding = renewal_binding_view(&self.binding);
            return Ok(ITeeRegistryV1::nodeHostEnclaveBindingCall::abi_encode_returns(&binding));
        }
        if data.starts_with(&ITeeRegistryV1::tributeOfferPublicKeyCall::SELECTOR) {
            return Ok(
                ITeeRegistryV1::tributeOfferPublicKeyCall::abi_encode_returns(&U256::from(9)),
            );
        }
        Err(RenewalError::TestFault("unexpected Registry call").into())
    }
}

impl ChainRpc for ReplayRpc {
    async fn chain_id(&self) -> Result<u64> {
        Ok(DEVNET_CHAIN_ID)
    }
}

impl RelayPreparationRpc for ReplayRpc {
    async fn gas_price(&self) -> Result<U256> {
        Err(RenewalError::TestFault("restart replay regenerated gas price").into())
    }

    async fn transaction_count(&self, _address: Address) -> Result<u64> {
        Err(RenewalError::TestFault("restart replay regenerated account nonce").into())
    }

    async fn balance(&self, _address: Address) -> Result<U256> {
        Err(RenewalError::TestFault("restart replay rechecked preparation balance").into())
    }
}

impl RelayRpc for ReplayRpc {
    async fn send_raw_transaction(&self, raw_transaction: &[u8]) -> Result<String> {
        self.sent.lock().unwrap().push(raw_transaction.to_vec());
        if let Some(error) = self.send_error {
            if let Some(receipt) = &self.receipt_after_send_error {
                *self.receipt.lock().unwrap() = Some(receipt.clone());
            }
            return Err(RenewalError::TestFault(error).into());
        }
        Ok(format!("{:#x}", keccak256(raw_transaction)))
    }
}

impl RegistryRpc for ReplayRpc {
    async fn tee_renewal_schedule_v1(&self) -> Result<TeeRenewalScheduleV1> {
        Ok(self.schedule)
    }
}

struct ReplayMustNotPrepare;

impl RenewalEnclaveV1 for ReplayMustNotPrepare {
    fn generate_dcap_quote(
        &mut self,
        _intent: &RegistrationIntentV1,
    ) -> Result<GeneratedDcapQuoteV1> {
        panic!("restart replay regenerated a DCAP quote")
    }

    fn sign_registration_intent_dev_v1(
        &mut self,
        _intent: &RegistrationIntentV1,
    ) -> Result<[u8; 64]> {
        panic!("restart replay regenerated a DirectDev signature")
    }
}

fn renewal_binding_view(binding: &RenewalBindingV1) -> NodeEnclaveBindingV1View {
    binding.into()
}

struct ReplayIdentity {
    policy: TeePolicyV1,
    manifest: EnclaveInitializationManifestV1,
    source: RenewalBindingV1,
    node_signer: k256::ecdsa::SigningKey,
    enclave_signer: ed25519_dalek::SigningKey,
}

fn replay_policy(mode: AttestationMode) -> TeePolicyV1 {
    let genesis_hash = B256::repeat_byte(0x82);
    let profile = match mode {
        AttestationMode::DcapRequired => {
            InitialTeeProfileV1::DcapRequired(ProductionSgxMeasurementV1 {
                mrenclave: B256::repeat_byte(0x83),
                mrsigner: B256::repeat_byte(0x84),
                isv_prod_id: 1,
                minimum_isv_svn: 1,
                minimum_tcb_evaluation_data_number: 1,
            })
        }
        AttestationMode::GramineDirectDev => InitialTeeProfileV1::GramineDirectDev,
    };
    initial_tee_policy_v1(profile, DEVNET_CHAIN_ID, genesis_hash).unwrap()
}

fn replay_identity(mode: AttestationMode) -> ReplayIdentity {
    let policy = replay_policy(mode);
    let genesis_hash = policy.genesis_hash;
    let node_signer = k256::ecdsa::SigningKey::from_bytes((&[0x85; 32]).into()).unwrap();
    let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[0x86; 32]);
    let node_id = NodeIdV1 {
        reth_p2p_public: node_signer
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap(),
    };
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: policy.chain_id,
        genesis_hash,
        attestation_mode: policy.attestation_mode,
        node_id: node_id.clone(),
        initialization_challenge: [0x87; 32],
        node_host_noise_x25519: [0x88; 32],
        recipient_x25519: [0x89; 32],
        attestation_ed25519: enclave_signer.verifying_key().to_bytes(),
        noise_responder_x25519: [0x8a; 32],
    };
    let source = replay_source(&policy, &manifest);
    ReplayIdentity {
        policy,
        manifest,
        source,
        node_signer,
        enclave_signer,
    }
}

fn replay_source(
    policy: &TeePolicyV1,
    manifest: &EnclaveInitializationManifestV1,
) -> RenewalBindingV1 {
    let mode = policy.attestation_mode;
    let node_id = &manifest.node_id;
    let policy_hash = policy.policy_hash().unwrap();
    let enclave_id = manifest.enclave_id().unwrap();
    let node_host_authorization_hash = manifest.node_host_authorization_hash().unwrap();
    let valid_until = 2_000_000;
    let requested_valid_until = valid_until + policy.maximum_lease;
    let source_collateral_valid_until = match mode {
        AttestationMode::DcapRequired => {
            requested_valid_until
                .checked_add(policy.collateral_margin)
                .unwrap()
                + 1_000
        }
        AttestationMode::GramineDirectDev => u64::MAX,
    };
    RenewalBindingV1 {
        node_id_hash: node_id.node_id_hash().unwrap(),
        enclave_id,
        binding_id: B256::repeat_byte(0x8b),
        intent_hash: B256::repeat_byte(0x8c),
        evidence_hash: B256::repeat_byte(0x8d),
        policy_hash,
        binding_version: 1,
        registration_version: 1,
        renewal_nonce: 1,
        transition_nonce: 0,
        lease_started_at: valid_until - policy.maximum_lease,
        valid_until,
        collateral_valid_until: source_collateral_valid_until,
        recipient_x25519: B256::from(manifest.recipient_x25519),
        attestation_ed25519: B256::from(manifest.attestation_ed25519),
        noise_responder_x25519: B256::from(manifest.noise_responder_x25519),
        mrenclave: B256::repeat_byte(0x83),
        mrsigner: B256::repeat_byte(0x84),
        isv_prod_id: 1,
        isv_svn: 1,
        platform_tcb_status: 0,
        verdict_hash: B256::repeat_byte(0x8e),
        node_host_authorization_hash,
    }
}

fn replay_intent(identity: &ReplayIdentity) -> RegistrationIntentV1 {
    let ReplayIdentity {
        policy,
        manifest,
        source,
        ..
    } = identity;
    let mode = policy.attestation_mode;
    let genesis_hash = policy.genesis_hash;
    let policy_hash = source.policy_hash;
    let node_id = manifest.node_id.clone();
    let enclave_id = source.enclave_id;
    let node_host_authorization_hash = source.node_host_authorization_hash;
    let requested_valid_until = source.valid_until + policy.maximum_lease;
    RegistrationIntentV1 {
        chain_id: policy.chain_id,
        genesis_hash,
        operation: AttestationOperationV1::RenewEnclave,
        attestation_mode: mode,
        policy_hash,
        node_id,
        enclave_id,
        binding_id: source.binding_id,
        binding_version: source.binding_version,
        registration_version: source.registration_version + 1,
        renewal_nonce: source.renewal_nonce + 1,
        transition_nonce: source.transition_nonce,
        requested_valid_until,
        recipient_x25519: manifest.recipient_x25519,
        attestation_ed25519: manifest.attestation_ed25519,
        noise_responder_x25519: manifest.noise_responder_x25519,
        node_host_authorization_hash,
    }
}

fn replay_evidence(
    mode: AttestationMode,
    intent: &RegistrationIntentV1,
    enclave_signature: [u8; 64],
) -> (Vec<u8>, B256) {
    let evidence_value = match mode {
        AttestationMode::DcapRequired => AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
            intent: intent.clone(),
            quote: vec![1],
            components: (1_u8..=8)
                .map(|tag| DcapCollateralComponentV1 {
                    kind: DcapCollateralKind::try_from(tag).unwrap(),
                    bytes: vec![tag],
                })
                .collect(),
            transition_key_ready_proof: None,
        }),
        AttestationMode::GramineDirectDev => {
            AttestationEvidenceV1::GramineDirectDev(GramineDirectEvidenceV1 {
                transition_key_ready_proof: None,
                intent: intent.clone(),
                dev_attestation_public: intent.attestation_ed25519,
                dev_signature: enclave_signature,
            })
        }
    };
    let evidence = evidence_value.encode_canonical().unwrap();
    let evidence_hash = match mode {
        AttestationMode::DcapRequired => dcap_evidence_hash_v1(&evidence).unwrap(),
        AttestationMode::GramineDirectDev => evidence_value.evidence_hash().unwrap(),
    };
    (evidence, evidence_hash)
}

fn replay_relay(
    policy: &TeePolicyV1,
    calldata: &[u8],
    evidence_len: usize,
) -> (RelaySignerV1, crate::tx::RawRelayTransactionV1) {
    let mode = policy.attestation_mode;
    let gas_limit = TeeRegistryGasScheduleV1::normative()
        .maximum_transaction_gas(
            RegistryMutatorV1::RenewEnclave,
            calldata.len(),
            evidence_len,
            policy.measurement_rules.len(),
            mode,
        )
        .unwrap();
    let relay = RelaySignerV1::new(&hex::encode([0x8f; 32])).unwrap();
    let raw = relay
        .sign_renewal(UnsignedRelayTransactionV1 {
            chain_id: DEVNET_CHAIN_ID,
            account_nonce: 7,
            gas_price: U256::from(2_000_000_000_u64),
            gas_limit,
            to: TEE_REGISTRY_ADDRESS,
            calldata,
        })
        .unwrap();
    (relay, raw)
}

fn replay_attempt(identity: &ReplayIdentity) -> (RelaySignerV1, PreparedRenewalV1) {
    let ReplayIdentity {
        policy,
        source,
        node_signer,
        enclave_signer,
        ..
    } = identity;
    let mode = policy.attestation_mode;
    let source_collateral_valid_until = source.collateral_valid_until;
    let requested_valid_until = source.valid_until + policy.maximum_lease;
    let intent = replay_intent(identity);
    let intent_hash = intent.intent_hash().unwrap();
    let node_signature = sign_node_host_hash_for_test(node_signer, intent_hash);
    let enclave_signature = enclave_signer.sign(intent_hash.as_slice()).to_bytes();
    let (evidence, evidence_hash) = replay_evidence(mode, &intent, enclave_signature);
    let calldata = ITeeRegistryV1::renewEnclaveCall {
        evidence: evidence.clone().into(),
        nodeSignature: node_signature.to_vec().into(),
        enclaveSignature: enclave_signature.to_vec().into(),
    }
    .abi_encode();
    let (relay, raw) = replay_relay(policy, &calldata, evidence.len());
    let attempt = PreparedRenewalV1 {
        source: source.clone(),
        intent: intent.encode_canonical().unwrap(),
        intent_hash,
        evidence,
        evidence_hash,
        node_signature: node_signature.to_vec(),
        enclave_signature: enclave_signature.to_vec(),
        calldata_hash: keccak256(&calldata),
        calldata,
        requested_valid_until,
        collateral_valid_until: source_collateral_valid_until,
        collateral_margin: match mode {
            AttestationMode::DcapRequired => policy.collateral_margin,
            AttestationMode::GramineDirectDev => 0,
        },
        relay: relay.address(),
        relay_variants: vec![raw],
    };
    (relay, attempt)
}

fn replay_fixture(
    node_data_dir: &std::path::Path,
    mode: AttestationMode,
) -> (
    ReplayRpc,
    RelaySignerV1,
    RenewalServiceConfigV1,
    PreparedRenewalV1,
) {
    let identity = replay_identity(mode);
    let (relay, attempt) = replay_attempt(&identity);
    let ReplayIdentity {
        policy,
        manifest,
        source,
        ..
    } = identity;
    let valid_until = source.valid_until;
    let schedule = TeeRenewalScheduleV1 {
        finalized_height: 120,
        finalized_hash: B256::repeat_byte(0x90),
        finalized_timestamp: valid_until - policy.maximum_lease / 2 + 1,
        epoch_number: 2,
        epoch_start_height: 100,
        epoch_length_blocks: 100,
        next_freeze_height: 180,
        planned_activation_height: 200,
        dkg_prepare_window_blocks: 20,
        minimum_block_time_millis: 2_000,
    };
    let sent = Arc::new(Mutex::new(Vec::new()));
    let rpc = ReplayRpc {
        policy,
        binding: source,
        schedule,
        receipt: Arc::new(Mutex::new(None)),
        send_error: None,
        receipt_after_send_error: None,
        sent,
    };
    let config = RenewalServiceConfigV1 {
        node_data_dir: node_data_dir.to_path_buf(),
        selector: NodeBindingSelectorV1::NodeHost(manifest.node_id.reth_p2p_public),
        manifest,
    };
    (rpc, relay, config, attempt)
}

#[tokio::test]
async fn promoted_upgrade_allows_successor_renewal_and_exact_restart_replay() {
    for mode in [
        AttestationMode::DcapRequired,
        AttestationMode::GramineDirectDev,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (mut rpc, relay, config, attempt) = replay_fixture(dir.path(), mode);
        store_upgrade_checkpoint(&config, &attempt, true);
        rpc.schedule.finalized_timestamp =
            renewal_opens_at(&rpc.binding, rpc.policy.maximum_lease).unwrap() - 1;
        let mut enclave = ReplayMustNotPrepare;
        let signer = |_hash: B256| -> Result<[u8; 65]> { panic!("unexpected signing") };
        assert!(matches!(
            run_renewal_once_v1(
                renewal_services(&rpc, &relay, &mut enclave, &signer),
                &config
            )
            .await
            .unwrap(),
            RenewalOutcomeV1::NotDue { .. }
        ));
        RenewalJournalGuard::acquire(dir.path())
            .unwrap()
            .store(RenewalJournalSnapshotV1::new(
                RenewalJournalStateV1::Prepared {
                    attempt: attempt.clone(),
                },
            ))
            .unwrap();
        let outcome = run_renewal_once_v1(
            renewal_services(&rpc, &relay, &mut enclave, &signer),
            &config,
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            RenewalOutcomeV1::Submitted {
                transaction_hash: attempt.relay_variants[0].transaction_hash,
                replayed: true,
            }
        );
        assert_eq!(
            *rpc.sent.lock().unwrap(),
            vec![attempt.relay_variants[0].raw_transaction.clone()]
        );
        assert!(matches!(
            super::super::super::UpgradeJournalGuardV1::acquire(dir.path())
                .unwrap()
                .load()
                .unwrap()
                .unwrap()
                .lifecycle,
            super::super::super::UpgradeJournalStateV1::Promoted { .. }
        ));
    }
}

#[tokio::test]
async fn promoted_successor_supersedes_only_finished_predecessor_renewals() {
    for case in 0..4 {
        assert_predecessor_supersession_case(case).await;
    }
}

async fn assert_predecessor_supersession_case(case: u8) {
    let dir = tempfile::tempdir().unwrap();
    let (mut rpc, relay, mut config, attempt) =
        replay_fixture(dir.path(), AttestationMode::GramineDirectDev);
    let mut previous = rpc.binding.clone();
    previous.intent_hash = attempt.intent_hash;
    previous.evidence_hash = attempt.evidence_hash;
    previous.registration_version += 1;
    previous.renewal_nonce += 1;
    previous.valid_until = attempt.requested_valid_until;
    let lifecycle = predecessor_lifecycle(case, &attempt, previous);
    RenewalJournalGuard::acquire(dir.path())
        .unwrap()
        .store(RenewalJournalSnapshotV1::new(lifecycle.clone()))
        .unwrap();
    config.manifest.recipient_x25519[0] ^= 1;
    rpc.binding.enclave_id = config.manifest.enclave_id().unwrap();
    rpc.binding.recipient_x25519 = config.manifest.recipient_x25519.into();
    rpc.binding.node_host_authorization_hash =
        config.manifest.node_host_authorization_hash().unwrap();
    rpc.binding.binding_version += if case == 3 { 2 } else { 1 };
    rpc.binding.transition_nonce += 1;
    rpc.schedule.finalized_timestamp =
        renewal_opens_at(&rpc.binding, rpc.policy.maximum_lease).unwrap() - 1;
    store_upgrade_checkpoint(&config, &attempt, true);
    let mut enclave = ReplayMustNotPrepare;
    let signer = |_hash: B256| -> Result<[u8; 65]> { panic!("unexpected signing") };
    let result = run_renewal_once_v1(
        renewal_services(&rpc, &relay, &mut enclave, &signer),
        &config,
    )
    .await;
    if case < 2 {
        assert!(matches!(result.unwrap(), RenewalOutcomeV1::NotDue { .. }));
    } else {
        assert!(
            result.is_err(),
            "pending or non-successor journal must not be ignored"
        );
    }
    assert!(rpc.sent.lock().unwrap().is_empty());
    assert_eq!(
        RenewalJournalGuard::acquire(dir.path())
            .unwrap()
            .load()
            .unwrap()
            .unwrap()
            .lifecycle,
        lifecycle
    );
}

fn predecessor_lifecycle(
    case: u8,
    attempt: &PreparedRenewalV1,
    previous: RenewalBindingV1,
) -> RenewalJournalStateV1 {
    match case {
        1 => RenewalJournalStateV1::Abandoned {
            attempt: attempt.clone(),
            abandoned_at_finalized_height: 99,
            reason: "expired predecessor attempt".into(),
        },
        2 => RenewalJournalStateV1::Prepared {
            attempt: attempt.clone(),
        },
        _ => RenewalJournalStateV1::Finalized {
            attempt: Box::new(attempt.clone()),
            finalized_binding: previous,
            finalized_height: 99,
            finalized_hash: B256::repeat_byte(5),
        },
    }
}

#[tokio::test]
async fn finalized_upgrade_and_wrong_promoted_identity_still_block_renewal() {
    for case in 0..3 {
        let dir = tempfile::tempdir().unwrap();
        let (mut rpc, relay, mut config, attempt) =
            replay_fixture(dir.path(), AttestationMode::GramineDirectDev);
        store_upgrade_checkpoint(&config, &attempt, case != 0);
        if case == 1 {
            config.manifest.initialization_challenge[0] ^= 1;
        }
        if case == 2 {
            rpc.binding.enclave_id = B256::repeat_byte(0xee);
        }
        let mut enclave = ReplayMustNotPrepare;
        let signer = |_hash: B256| -> Result<[u8; 65]> { panic!("unexpected signing") };
        let error = run_renewal_once_v1(
            renewal_services(&rpc, &relay, &mut enclave, &signer),
            &config,
        )
        .await
        .unwrap_err()
        .to_string();
        let expected = [
            "renewal is blocked",
            "does not match the promoted enclave",
            "does not match the finalized Registry binding",
        ][case];
        assert!(error.contains(expected), "{error}");
        assert!(rpc.sent.lock().unwrap().is_empty());
    }
}

fn store_upgrade_checkpoint(
    config: &RenewalServiceConfigV1,
    attempt: &PreparedRenewalV1,
    promoted: bool,
) {
    use super::super::super::{
        PreparedUpgradeSubmissionV1, UpgradeContextV1, UpgradeJournalGuardV1,
        UpgradeJournalSnapshotV1, UpgradeJournalStateV1, UpgradeSecurityMaterialV1,
    };
    let context = UpgradeContextV1 {
        predecessor_manifest_hash: B256::repeat_byte(0xa1),
        candidate_manifest_hash: config.manifest.authorization_hash().unwrap(),
        successor_policy_hash: attempt.source.policy_hash,
        activation_height: 200,
        active_tee_dir: config.node_data_dir.join("old-tee"),
        candidate_tee_dir: config.node_data_dir.join("new-tee"),
    };
    let submission = PreparedUpgradeSubmissionV1 {
        intent_hash: attempt.intent_hash,
        evidence_hash: attempt.evidence_hash,
        calldata_hash: attempt.calldata_hash,
        relay: attempt.relay,
        relay_variants: attempt.relay_variants.clone(),
    };
    let lifecycle = if promoted {
        UpgradeJournalStateV1::Promoted {
            context,
            submission,
            security: UpgradeSecurityMaterialV1 {
                sealed_root_hash: B256::repeat_byte(1),
                resident_offer_public: B256::repeat_byte(2),
                proof_hash: B256::repeat_byte(3),
            },
            finalized_height: 100,
            finalized_hash: B256::repeat_byte(4),
        }
    } else {
        UpgradeJournalStateV1::Finalized {
            context,
            submission,
            security: UpgradeSecurityMaterialV1 {
                sealed_root_hash: B256::repeat_byte(1),
                resident_offer_public: B256::repeat_byte(2),
                proof_hash: B256::repeat_byte(3),
            },
            finalized_height: 100,
            finalized_hash: B256::repeat_byte(4),
        }
    };
    UpgradeJournalGuardV1::acquire(&config.node_data_dir)
        .unwrap()
        .store(UpgradeJournalSnapshotV1::new(lifecycle))
        .unwrap();
}

#[tokio::test]
async fn prepared_and_submitted_restarts_replay_exact_transaction_in_both_modes() {
    for mode in [
        AttestationMode::DcapRequired,
        AttestationMode::GramineDirectDev,
    ] {
        for starts_submitted in [false, true] {
            let node_data_dir = tempfile::tempdir().unwrap();
            let (rpc, relay, config, attempt) = replay_fixture(node_data_dir.path(), mode);
            let expected_raw = attempt.relay_variants[0].raw_transaction.clone();
            let expected_hash = attempt.relay_variants[0].transaction_hash;
            let lifecycle = if starts_submitted {
                RenewalJournalStateV1::Submitted {
                    attempt: attempt.clone(),
                    submitted_at_finalized_height: 119,
                    transaction_hashes: vec![expected_hash],
                }
            } else {
                RenewalJournalStateV1::Prepared {
                    attempt: attempt.clone(),
                }
            };
            RenewalJournalGuard::acquire(node_data_dir.path())
                .unwrap()
                .store(RenewalJournalSnapshotV1::new(lifecycle))
                .unwrap();

            let mut enclave = ReplayMustNotPrepare;
            let node_signer = |_hash: B256| -> Result<[u8; 65]> {
                panic!("restart replay regenerated a NodeHost signature")
            };
            let services = renewal_services(&rpc, &relay, &mut enclave, &node_signer);
            let outcome = run_renewal_once_v1(services, &config).await.unwrap();
            assert_eq!(
                outcome,
                RenewalOutcomeV1::Submitted {
                    transaction_hash: expected_hash,
                    replayed: true,
                }
            );
            assert_eq!(rpc.sent.lock().unwrap().as_slice(), [expected_raw]);
            let replayed = RenewalJournalGuard::acquire(node_data_dir.path())
                .unwrap()
                .load()
                .unwrap()
                .unwrap();
            let RenewalJournalStateV1::Submitted {
                attempt: replayed_attempt,
                transaction_hashes,
                ..
            } = replayed.lifecycle
            else {
                panic!("replay did not persist Submitted state");
            };
            assert_eq!(replayed_attempt, attempt);
            assert_eq!(transaction_hashes, vec![expected_hash]);
        }
    }
}

#[tokio::test]
async fn submitted_exact_transaction_observed_before_finality_is_not_rebroadcast() {
    let node_data_dir = tempfile::tempdir().unwrap();
    let (rpc, relay, config, attempt) =
        replay_fixture(node_data_dir.path(), AttestationMode::GramineDirectDev);
    let expected_hash = attempt.relay_variants[0].transaction_hash;
    *rpc.receipt.lock().unwrap() = Some(serde_json::json!({
        "transactionHash": format!("{expected_hash:#x}"),
        "status": "0x1",
    }));
    RenewalJournalGuard::acquire(node_data_dir.path())
        .unwrap()
        .store(RenewalJournalSnapshotV1::new(
            RenewalJournalStateV1::Submitted {
                attempt,
                submitted_at_finalized_height: 119,
                transaction_hashes: vec![expected_hash],
            },
        ))
        .unwrap();

    let mut enclave = ReplayMustNotPrepare;
    let node_signer = |_hash: B256| -> Result<[u8; 65]> {
        panic!("receipt reconciliation regenerated a NodeHost signature")
    };
    let outcome = run_renewal_once_v1(
        renewal_services(&rpc, &relay, &mut enclave, &node_signer),
        &config,
    )
    .await
    .unwrap();

    assert_eq!(
        outcome,
        RenewalOutcomeV1::Submitted {
            transaction_hash: expected_hash,
            replayed: true,
        }
    );
    assert!(
        rpc.sent.lock().unwrap().is_empty(),
        "an exact transaction already observed by receipt must not be rebroadcast before finality"
    );
}

#[tokio::test]
async fn nonce_too_low_without_exact_receipt_fails_closed() {
    let node_data_dir = tempfile::tempdir().unwrap();
    let (mut rpc, relay, config, attempt) =
        replay_fixture(node_data_dir.path(), AttestationMode::GramineDirectDev);
    let expected_hash = attempt.relay_variants[0].transaction_hash;
    rpc.send_error = Some("nonce too low: next nonce 1, tx nonce 0");
    RenewalJournalGuard::acquire(node_data_dir.path())
        .unwrap()
        .store(RenewalJournalSnapshotV1::new(
            RenewalJournalStateV1::Submitted {
                attempt,
                submitted_at_finalized_height: 119,
                transaction_hashes: vec![expected_hash],
            },
        ))
        .unwrap();

    let mut enclave = ReplayMustNotPrepare;
    let node_signer = |_hash: B256| -> Result<[u8; 65]> {
        panic!("nonce conflict handling regenerated a NodeHost signature")
    };
    let error = run_renewal_once_v1(
        renewal_services(&rpc, &relay, &mut enclave, &node_signer),
        &config,
    )
    .await
    .expect_err("a consumed nonce without the exact receipt must fail closed");

    assert!(
        error
            .to_string()
            .contains("consumed without the exact transaction receipt"),
        "unexpected error: {error:#}"
    );
}

#[tokio::test]
async fn nonce_too_low_race_accepts_only_the_exact_success_receipt() {
    let node_data_dir = tempfile::tempdir().unwrap();
    let (mut rpc, relay, config, attempt) =
        replay_fixture(node_data_dir.path(), AttestationMode::GramineDirectDev);
    let expected_raw = attempt.relay_variants[0].raw_transaction.clone();
    let expected_hash = attempt.relay_variants[0].transaction_hash;
    rpc.send_error = Some("nonce too low: next nonce 1, tx nonce 0");
    rpc.receipt_after_send_error = Some(serde_json::json!({
        "transactionHash": format!("{expected_hash:#x}"),
        "status": "0x1",
    }));
    RenewalJournalGuard::acquire(node_data_dir.path())
        .unwrap()
        .store(RenewalJournalSnapshotV1::new(
            RenewalJournalStateV1::Submitted {
                attempt,
                submitted_at_finalized_height: 119,
                transaction_hashes: vec![expected_hash],
            },
        ))
        .unwrap();

    let mut enclave = ReplayMustNotPrepare;
    let node_signer = |_hash: B256| -> Result<[u8; 65]> {
        panic!("nonce race handling regenerated a NodeHost signature")
    };
    let outcome = run_renewal_once_v1(
        renewal_services(&rpc, &relay, &mut enclave, &node_signer),
        &config,
    )
    .await
    .unwrap();

    assert_eq!(
        outcome,
        RenewalOutcomeV1::Submitted {
            transaction_hash: expected_hash,
            replayed: true,
        }
    );
    assert_eq!(rpc.sent.lock().unwrap().as_slice(), [expected_raw]);
}

#[tokio::test]
async fn exact_receipt_requires_canonical_hash_and_success_status() {
    let node_data_dir = tempfile::tempdir().unwrap();
    let (rpc, _relay, _config, attempt) =
        replay_fixture(node_data_dir.path(), AttestationMode::GramineDirectDev);
    let raw = &attempt.relay_variants[0];
    let expected_hash = raw.transaction_hash;

    for status in ["0x0", "0x00", "0", "0x2", "malformed"] {
        *rpc.receipt.lock().unwrap() = Some(serde_json::json!({
            "transactionHash": format!("{expected_hash:#x}"),
            "status": status,
        }));
        let error = exact_transaction_receipt_exists(&rpc, raw)
            .await
            .expect_err("non-success receipt status must fail closed");
        assert!(error.to_string().contains("non-success status"));
    }

    *rpc.receipt.lock().unwrap() = Some(serde_json::json!({
        "transactionHash": format!("{:#x}", B256::repeat_byte(0x33)),
        "status": "0x1",
    }));
    assert!(exact_transaction_receipt_exists(&rpc, raw).await.is_err());

    *rpc.receipt.lock().unwrap() = Some(serde_json::json!({
        "transactionHash": format!("{expected_hash:#x}"),
    }));
    assert!(exact_transaction_receipt_exists(&rpc, raw).await.is_err());
}
