use std::{fs, os::unix::fs::PermissionsExt as _};

use alloy_primitives::{Address, U256};
use outbe_tee::{
    registry_view::RenewalBindingV1,
    upgrade_journal::{UpgradeSecurityMaterialV1, UPGRADE_SEALED_ROOT_FILENAME},
};

use super::super::rpc::{
    ChainRpc, CliRpcReport, FinalizedStateRpc, RegistryRpc, RelayPreparationRpc, RelayRpc,
    TransactionReceiptRpc,
};
use super::*;

const DIRECTORY_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;

fn store_checkpoint(guard: &UpgradeJournalGuardV1, state: UpgradeJournalStateV1) {
    guard.store(UpgradeJournalSnapshotV1::new(state)).unwrap();
}

fn store_candidate_prepared(guard: &UpgradeJournalGuardV1, context: &UpgradeContextV1) {
    store_checkpoint(
        guard,
        UpgradeJournalStateV1::CandidatePrepared {
            context: context.clone(),
        },
    );
}

fn key_provisioned(context: UpgradeContextV1, sealed_root_hash: B256) -> UpgradeJournalStateV1 {
    UpgradeJournalStateV1::KeyProvisioned {
        context,
        sealed_root_hash,
    }
}

fn key_ready(context: UpgradeContextV1, sealed_root_hash: B256) -> UpgradeJournalStateV1 {
    UpgradeJournalStateV1::CandidateKeyReady {
        context,
        security: UpgradeSecurityMaterialV1 {
            sealed_root_hash,
            resident_offer_public: B256::repeat_byte(7),
            proof_hash: B256::repeat_byte(8),
        },
    }
}

fn private_dir(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(DIRECTORY_MODE)).unwrap();
}

fn context(root: &Path) -> UpgradeContextV1 {
    UpgradeContextV1 {
        predecessor_manifest_hash: B256::repeat_byte(1),
        candidate_manifest_hash: B256::repeat_byte(2),
        successor_policy_hash: B256::repeat_byte(3),
        activation_height: 100,
        active_tee_dir: root.join("active"),
        candidate_tee_dir: root.join("candidate"),
    }
}

#[test]
fn software_seal_checkpoint_is_explicitly_feature_and_network_gated() {
    use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    private_dir(&context.candidate_tee_dir);
    prepare_upgrade_journal_v1(root.path(), context.clone()).unwrap();
    let seal = context.candidate_tee_dir.join(UPGRADE_SEALED_ROOT_FILENAME);
    let bytes_for = |chain_id, mode| {
        let mut bytes = b"LE2E1TSEAL".to_vec();
        bytes.extend([0u8; 32]);
        bytes.extend(
            NetworkBindingV1 {
                chain_id,
                genesis_hash: B256::repeat_byte(1),
                attestation_mode: mode,
            }
            .encode_canonical()
            .unwrap(),
        );
        bytes
    };
    let local_chain = U256::from(outbe_primitives::chain::DEVNET_CHAIN_ID).to_be_bytes::<32>();
    for bytes in [
        b"LE2E1".to_vec(),
        bytes_for([1; 32], AttestationMode::GramineDirectDev),
        bytes_for(local_chain, AttestationMode::DcapRequired),
    ] {
        fs::write(&seal, bytes).unwrap();
        fs::set_permissions(&seal, fs::Permissions::from_mode(FILE_MODE)).unwrap();
        assert!(record_network_key_provisioned_v1(root.path()).is_err());
    }
    fs::write(
        &seal,
        bytes_for(local_chain, AttestationMode::GramineDirectDev),
    )
    .unwrap();
    assert_eq!(
        record_network_key_provisioned_v1(root.path()).is_ok(),
        cfg!(feature = "local-e2e")
    );
}

struct SubmissionRpc {
    policy: outbe_primitives::tee_attestation_v1::TeePolicyV1,
    binding: RenewalBindingV1,
    schedule: outbe_primitives::tee_operator_v1::TeeRenewalScheduleV1,
    send_error: Option<&'static str>,
    receipt: Option<serde_json::Value>,
}

impl ChainRpc for SubmissionRpc {
    type Error = CliRpcReport;

    async fn chain_id(&self) -> std::result::Result<u64, Self::Error> {
        Ok(outbe_primitives::chain::DEVNET_CHAIN_ID)
    }
}

impl FinalizedStateRpc for SubmissionRpc {
    async fn finalized_block(&self) -> std::result::Result<serde_json::Value, Self::Error> {
        Ok(serde_json::json!({
            "number": format!("0x{:x}", self.schedule.finalized_height),
            "timestamp": format!("0x{:x}", self.schedule.finalized_timestamp),
            "hash": format!("{:#x}", self.schedule.finalized_hash),
            "stateRoot": format!("{:#x}", B256::repeat_byte(0x81)),
        }))
    }

    async fn call_at(
        &self,
        _to: Address,
        data: &[u8],
        _block_tag: &str,
    ) -> std::result::Result<Vec<u8>, Self::Error> {
        use alloy_primitives::Bytes;
        if data.starts_with(&ITeeRegistryV1::activePolicyV1Call::SELECTOR) {
            let policy = Bytes::from(self.policy.encode_canonical().unwrap());
            return Ok(ITeeRegistryV1::activePolicyV1Call::abi_encode_returns(
                &policy,
            ));
        }
        if data.starts_with(&ITeeRegistryV1::nodeHostEnclaveBindingCall::SELECTOR) {
            let binding = outbe_primitives::tee_registry_abi_v1::NodeEnclaveBindingV1View::from(
                &self.binding,
            );
            return Ok(ITeeRegistryV1::nodeHostEnclaveBindingCall::abi_encode_returns(&binding));
        }
        if data.starts_with(&ITeeRegistryV1::tributeOfferPublicKeyCall::SELECTOR) {
            return Ok(
                ITeeRegistryV1::tributeOfferPublicKeyCall::abi_encode_returns(&U256::from(9)),
            );
        }
        Err(CliRpcReport(eyre::eyre!("unexpected Registry call")))
    }
}

impl RegistryRpc for SubmissionRpc {
    async fn tee_renewal_schedule_v1(
        &self,
    ) -> std::result::Result<outbe_primitives::tee_operator_v1::TeeRenewalScheduleV1, Self::Error>
    {
        Ok(self.schedule)
    }
}

impl TransactionReceiptRpc for SubmissionRpc {
    async fn transaction_receipt(
        &self,
        _transaction_hash: &str,
    ) -> Result<Option<serde_json::Value>> {
        Ok(self.receipt.clone())
    }
}

impl RelayPreparationRpc for SubmissionRpc {
    async fn gas_price(&self) -> Result<U256> {
        Ok(U256::from(1))
    }

    async fn transaction_count(&self, _address: Address) -> Result<u64> {
        Ok(0)
    }

    async fn balance(&self, _address: Address) -> Result<U256> {
        Ok(U256::MAX)
    }
}

impl RelayRpc for SubmissionRpc {
    async fn send_raw_transaction(&self, raw_transaction: &[u8]) -> Result<String> {
        if let Some(error) = self.send_error {
            return Err(eyre::eyre!(error));
        }
        Ok(format!("{:#x}", keccak256(raw_transaction)))
    }
}

fn write_private(path: &Path, bytes: &[u8]) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

fn candidate_record(
    predecessor: B256,
    manifest: &outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1,
) -> Vec<u8> {
    let manifest_bytes = manifest.encode_canonical().unwrap();
    let len = u16::try_from(manifest_bytes.len()).unwrap().to_be_bytes();
    let mut out = Vec::with_capacity(35 + manifest_bytes.len());
    out.push(1);
    out.extend_from_slice(predecessor.as_slice());
    out.extend_from_slice(&len);
    out.extend_from_slice(&manifest_bytes);
    out
}

struct PreparedTransition {
    rpc: SubmissionRpc,
    selector: NodeBindingSelectorV1,
    submission: PreparedUpgradeSubmissionV1,
}

fn prepared_transition(root: &Path) -> PreparedTransition {
    use ed25519_dalek::Signer as _;
    use outbe_primitives::tee_attestation_v1::{
        AttestationEvidenceV1, AttestationMode, AttestationOperationV1,
        EnclaveInitializationManifestV1, GramineDirectEvidenceV1, NodeIdV1, RegistrationIntentV1,
        TransitionKeyReadyProofV1,
    };
    use outbe_primitives::tee_genesis_v1::{initial_tee_policy_v1, InitialTeeProfileV1};
    use outbe_primitives::tee_test_utils::sign_node_host_hash_for_test;
    use outbe_tee::node_host::{
        NODE_HOST_DIRECTORY_V1, NODE_HOST_MANIFEST_V1, NODE_HOST_NOISE_KEY_V1,
        NODE_HOST_REPLACEMENT_CANDIDATE_V1,
    };
    use outbe_tee::{persist_replacement_candidate_submission, NodeHostNoiseKey};
    use std::os::unix::fs::DirBuilderExt as _;

    let chain_id = U256::from(outbe_primitives::chain::DEVNET_CHAIN_ID).to_be_bytes();
    let genesis_hash = B256::repeat_byte(0x14);
    let policy = initial_tee_policy_v1(
        InitialTeeProfileV1::GramineDirectDev,
        outbe_primitives::chain::DEVNET_CHAIN_ID,
        genesis_hash,
    )
    .unwrap();
    let node_signer = k256::ecdsa::SigningKey::from_bytes((&[0x61; 32]).into()).unwrap();
    let reth_p2p_public = node_signer
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .unwrap();
    let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[0x62; 32]);
    let node_id = NodeIdV1 { reth_p2p_public };
    let host = root.join(NODE_HOST_DIRECTORY_V1);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&host)
        .unwrap();
    let noise = NodeHostNoiseKey::create_new(&host.join(NODE_HOST_NOISE_KEY_V1)).unwrap();
    let active = EnclaveInitializationManifestV1 {
        chain_id,
        genesis_hash,
        attestation_mode: AttestationMode::GramineDirectDev,
        node_id: node_id.clone(),
        initialization_challenge: [0x41; 32],
        node_host_noise_x25519: noise.public(),
        recipient_x25519: [0x51; 32],
        attestation_ed25519: [0x52; 32],
        noise_responder_x25519: [0x53; 32],
    };
    let candidate = EnclaveInitializationManifestV1 {
        initialization_challenge: [0x42; 32],
        recipient_x25519: [0x61; 32],
        attestation_ed25519: enclave_signer.verifying_key().to_bytes(),
        noise_responder_x25519: [0x63; 32],
        ..active.clone()
    };
    write_private(
        &host.join(NODE_HOST_MANIFEST_V1),
        &active.encode_canonical().unwrap(),
    );
    let predecessor = active.authorization_hash().unwrap();
    write_private(
        &host.join(NODE_HOST_REPLACEMENT_CANDIDATE_V1),
        &candidate_record(predecessor, &candidate),
    );
    let intent = RegistrationIntentV1 {
        chain_id,
        genesis_hash,
        operation: AttestationOperationV1::TransitionEnclaveMeasurement,
        attestation_mode: AttestationMode::GramineDirectDev,
        policy_hash: policy.policy_hash().unwrap(),
        node_id,
        enclave_id: candidate.enclave_id().unwrap(),
        binding_id: B256::repeat_byte(0x45),
        binding_version: 2,
        registration_version: 1,
        renewal_nonce: 0,
        transition_nonce: 1,
        requested_valid_until: 20_000,
        recipient_x25519: candidate.recipient_x25519,
        attestation_ed25519: candidate.attestation_ed25519,
        noise_responder_x25519: candidate.noise_responder_x25519,
        node_host_authorization_hash: candidate.node_host_authorization_hash().unwrap(),
    };
    let intent_hash = intent.intent_hash().unwrap();
    let mut proof = TransitionKeyReadyProofV1 {
        chain_id,
        genesis_hash,
        transition_intent_hash: intent_hash,
        candidate_manifest_hash: candidate.authorization_hash().unwrap(),
        transition_nonce: intent.transition_nonce,
        resident_offer_public: [0x71; 32],
        candidate_attestation_signature: [0; 64],
    };
    proof.candidate_attestation_signature = enclave_signer
        .sign(proof.signing_hash().unwrap().as_slice())
        .to_bytes();
    let enclave_signature = enclave_signer.sign(intent_hash.as_slice()).to_bytes();
    let node_signature = sign_node_host_hash_for_test(&node_signer, intent_hash);
    let evidence = AttestationEvidenceV1::GramineDirectDev(GramineDirectEvidenceV1 {
        intent: intent.clone(),
        dev_attestation_public: candidate.attestation_ed25519,
        dev_signature: enclave_signature,
        transition_key_ready_proof: Some(proof),
    });
    let durable = persist_replacement_candidate_submission(
        root,
        &evidence,
        &node_signature,
        &enclave_signature,
    )
    .unwrap();
    let evidence_bytes = durable.evidence();
    let decoded = AttestationEvidenceV1::decode_canonical(evidence_bytes).unwrap();
    let calldata = ITeeRegistryV1::transitionEnclaveMeasurementCall {
        evidence: evidence_bytes.to_vec().into(),
        nodeSignature: durable.node_signature().to_vec().into(),
        enclaveSignature: durable.enclave_signature().to_vec().into(),
    }
    .abi_encode();
    let raw_transaction = vec![0x31, 0x32];
    let submission = PreparedUpgradeSubmissionV1 {
        intent_hash: decoded.intent().intent_hash().unwrap(),
        evidence_hash: decoded.evidence_hash().unwrap(),
        calldata_hash: keccak256(&calldata),
        relay: Address::repeat_byte(5),
        relay_variants: vec![RawRelayTransactionV1 {
            relay: Address::repeat_byte(5),
            chain_id: 1,
            account_nonce: 2,
            gas_price: U256::from(3),
            gas_limit: 4,
            calldata_hash: keccak256(&calldata),
            transaction_hash: keccak256(&raw_transaction),
            raw_transaction,
        }],
    };
    let schedule = outbe_primitives::tee_operator_v1::TeeRenewalScheduleV1 {
        finalized_height: 120,
        finalized_hash: B256::repeat_byte(0x90),
        finalized_timestamp: 1_000,
        epoch_number: 2,
        epoch_start_height: 100,
        epoch_length_blocks: 100,
        next_freeze_height: 180,
        planned_activation_height: 200,
        dkg_prepare_window_blocks: 20,
        minimum_block_time_millis: 2_000,
    };
    let binding = RenewalBindingV1 {
        node_id_hash: active.node_id.node_id_hash().unwrap(),
        enclave_id: active.enclave_id().unwrap(),
        binding_id: B256::repeat_byte(0x44),
        intent_hash: B256::repeat_byte(0x46),
        evidence_hash: B256::repeat_byte(0x47),
        policy_hash: policy.policy_hash().unwrap(),
        binding_version: intent.binding_version - 1,
        registration_version: intent.registration_version - 1,
        renewal_nonce: intent.renewal_nonce,
        transition_nonce: intent.transition_nonce - 1,
        lease_started_at: 1,
        valid_until: 2_000_000,
        collateral_valid_until: u64::MAX,
        recipient_x25519: B256::from(active.recipient_x25519),
        attestation_ed25519: B256::from(active.attestation_ed25519),
        noise_responder_x25519: B256::from(active.noise_responder_x25519),
        mrenclave: B256::repeat_byte(0x83),
        mrsigner: B256::repeat_byte(0x84),
        isv_prod_id: 1,
        isv_svn: 1,
        platform_tcb_status: 0,
        verdict_hash: B256::repeat_byte(0x8e),
        node_host_authorization_hash: intent.node_host_authorization_hash,
    };
    PreparedTransition {
        rpc: SubmissionRpc {
            policy,
            binding,
            schedule,
            send_error: None,
            receipt: None,
        },
        selector: NodeBindingSelectorV1::NodeHost(reth_p2p_public),
        submission,
    }
}

fn store_upgrade_submission(root: &Path, prepared: &PreparedUpgradeSubmissionV1, submitted: bool) {
    let context = context(root);
    let security = UpgradeSecurityMaterialV1 {
        sealed_root_hash: B256::repeat_byte(6),
        resident_offer_public: B256::repeat_byte(7),
        proof_hash: B256::repeat_byte(8),
    };
    let guard = UpgradeJournalGuardV1::acquire(root).unwrap();
    store_candidate_prepared(&guard, &context);
    store_checkpoint(
        &guard,
        key_provisioned(context.clone(), security.sealed_root_hash),
    );
    store_checkpoint(
        &guard,
        key_ready(context.clone(), security.sealed_root_hash),
    );
    store_checkpoint(
        &guard,
        UpgradeJournalStateV1::SubmissionPrepared {
            context: context.clone(),
            security,
            submission: prepared.clone(),
        },
    );
    if submitted {
        store_checkpoint(
            &guard,
            UpgradeJournalStateV1::Submitted {
                context,
                security,
                submission: prepared.clone(),
                submitted_at_finalized_height: 90,
                transaction_hashes: vec![prepared.relay_variants[0].transaction_hash],
            },
        );
    }
}

fn upgrade_label(root: &Path) -> &'static str {
    UpgradeJournalGuardV1::acquire(root)
        .unwrap()
        .load()
        .unwrap()
        .unwrap()
        .lifecycle
        .label()
}

#[tokio::test]
async fn already_known_text_does_not_submit_without_exact_receipt() {
    let root = tempfile::tempdir().unwrap();
    let mut prepared = prepared_transition(root.path());
    prepared.rpc.send_error = Some("already known");
    store_upgrade_submission(root.path(), &prepared.submission, false);
    let error = super::submission::submit_prepared(
        &prepared.rpc,
        root.path(),
        &prepared.selector,
        &prepared.submission,
        false,
    )
    .await
    .expect_err("endpoint text alone must not submit the transition");
    assert!(
        error
            .to_string()
            .contains("already known without the exact transaction receipt"),
        "{error:#}"
    );
    assert_eq!(upgrade_label(root.path()), "submissionPrepared");
}

#[tokio::test]
async fn already_known_exact_success_receipt_submits_the_transition() {
    let root = tempfile::tempdir().unwrap();
    let mut prepared = prepared_transition(root.path());
    let hash = prepared.submission.relay_variants[0].transaction_hash;
    prepared.rpc.send_error = Some("already known");
    prepared.rpc.receipt = Some(serde_json::json!({
        "transactionHash": format!("{hash:#x}"),
        "status": "0x1",
    }));
    store_upgrade_submission(root.path(), &prepared.submission, false);
    let outcome = super::submission::submit_prepared(
        &prepared.rpc,
        root.path(),
        &prepared.selector,
        &prepared.submission,
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        outcome,
        UpgradeSubmissionOutcomeV1::Submitted {
            transaction_hash: hash,
            replayed: false,
        }
    );
    assert_eq!(upgrade_label(root.path()), "submitted");
}

#[tokio::test]
async fn ordinary_send_acknowledgement_submits_without_a_mined_receipt() {
    let root = tempfile::tempdir().unwrap();
    let prepared = prepared_transition(root.path());
    let hash = prepared.submission.relay_variants[0].transaction_hash;
    store_upgrade_submission(root.path(), &prepared.submission, false);
    let outcome = super::submission::submit_prepared(
        &prepared.rpc,
        root.path(),
        &prepared.selector,
        &prepared.submission,
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        outcome,
        UpgradeSubmissionOutcomeV1::Submitted {
            transaction_hash: hash,
            replayed: false,
        }
    );
    assert_eq!(upgrade_label(root.path()), "submitted");
}

#[tokio::test]
async fn failed_replay_receipt_does_not_report_already_submitted() {
    let root = tempfile::tempdir().unwrap();
    let mut prepared = prepared_transition(root.path());
    let hash = prepared.submission.relay_variants[0].transaction_hash;
    prepared.rpc.receipt = Some(serde_json::json!({
        "transactionHash": format!("{hash:#x}"),
        "status": "0x0",
    }));
    store_upgrade_submission(root.path(), &prepared.submission, true);
    let error = super::submission::replay_submitted(
        &prepared.rpc,
        root.path(),
        &prepared.selector,
        &prepared.submission,
    )
    .await
    .expect_err("a failed receipt must not count as an existing submission");
    assert!(
        error.to_string().contains("non-success status"),
        "{error:#}"
    );
    assert_eq!(upgrade_label(root.path()), "submitted");
}

#[tokio::test]
async fn missing_replay_receipt_stays_pending_after_already_known() {
    let root = tempfile::tempdir().unwrap();
    let mut prepared = prepared_transition(root.path());
    let hash = prepared.submission.relay_variants[0].transaction_hash;
    prepared.rpc.send_error = Some("already known");
    store_upgrade_submission(root.path(), &prepared.submission, true);
    let outcome = super::submission::replay_submitted(
        &prepared.rpc,
        root.path(),
        &prepared.selector,
        &prepared.submission,
    )
    .await
    .unwrap();
    assert_eq!(
        outcome,
        UpgradeSubmissionOutcomeV1::AlreadySubmitted {
            transaction_hash: hash
        }
    );
    assert_eq!(upgrade_label(root.path()), "submitted");
}

fn replay_flag(root: &Path) -> bool {
    inspect_upgrade_journal_v1(root).unwrap().is_some()
}

fn test_relay() -> RelaySignerV1 {
    RelaySignerV1::new(&"61".repeat(32)).unwrap()
}

#[tokio::test]
async fn candidate_free_reducer_submits_prepared_journal() {
    let root = tempfile::tempdir().unwrap();
    let prepared = prepared_transition(root.path());
    store_upgrade_submission(root.path(), &prepared.submission, false);
    let replayed = replay_flag(root.path());
    let step = super::submission::advance_candidate_free(
        &prepared.rpc,
        &test_relay(),
        root.path(),
        &prepared.selector,
        replayed,
    )
    .await
    .unwrap();
    let hash = prepared.submission.relay_variants[0].transaction_hash;
    assert_eq!(
        step,
        super::submission::CandidateFreeProgress::Finished(UpgradeSubmissionOutcomeV1::Submitted {
            transaction_hash: hash,
            replayed,
        })
    );
    assert!(replayed);
    assert_eq!(upgrade_label(root.path()), "submitted");
}

#[tokio::test]
async fn candidate_free_reducer_resets_expired_submission_before_provision() {
    let root = tempfile::tempdir().unwrap();
    let mut prepared = prepared_transition(root.path());
    prepared.rpc.schedule.finalized_timestamp = 30_000;
    store_upgrade_submission(root.path(), &prepared.submission, false);
    let step = super::submission::advance_candidate_free(
        &prepared.rpc,
        &test_relay(),
        root.path(),
        &prepared.selector,
        replay_flag(root.path()),
    )
    .await
    .unwrap();
    assert_eq!(step, super::submission::CandidateFreeProgress::ProvisionKey);
    assert_eq!(upgrade_label(root.path()), "keyProvisioned");
}

#[tokio::test]
async fn candidate_free_reducer_dispatches_missed_cutoff() {
    let root = tempfile::tempdir().unwrap();
    let prepared = prepared_transition(root.path());
    let upgrade_context = context(root.path());
    let guard = UpgradeJournalGuardV1::acquire(root.path()).unwrap();
    store_candidate_prepared(&guard, &upgrade_context);
    store_checkpoint(
        &guard,
        UpgradeJournalStateV1::TerminalMissedCutoff {
            context: upgrade_context,
            finalized_height: 50,
            activation_height: 40,
        },
    );
    drop(guard);
    let error = super::submission::advance_candidate_free(
        &prepared.rpc,
        &test_relay(),
        root.path(),
        &prepared.selector,
        false,
    )
    .await
    .expect_err("a missed cutoff stays terminal");
    let upgrade_error = error
        .downcast_ref::<UpgradeError>()
        .expect("cutoff stays a typed upgrade error");
    assert!(matches!(
        upgrade_error,
        UpgradeError::MissedActivationCutoff {
            activation_height: 40,
            finalized_height: 50,
        }
    ));
    assert_eq!(upgrade_label(root.path()), "terminalMissedCutoff");
}

#[test]
fn pending_renewal_journal_blocks_upgrade_prepare_and_provision_except_idempotent_prepare() {
    use super::super::renewal_journal::{
        tests::attempt as test_renewal_attempt, RenewalJournalGuard, RenewalJournalSnapshotV1,
        RenewalJournalStateV1,
    };

    let root = tempfile::tempdir().unwrap();
    let upgrade_context = context(root.path());
    private_dir(&upgrade_context.candidate_tee_dir);

    let store_pending_renewal = |dir: &Path| {
        RenewalJournalGuard::acquire(dir)
            .unwrap()
            .store(RenewalJournalSnapshotV1::new(
                RenewalJournalStateV1::Prepared {
                    attempt: test_renewal_attempt(),
                },
            ))
            .unwrap();
    };

    store_pending_renewal(root.path());
    let error = prepare_upgrade_journal_v1(root.path(), upgrade_context.clone())
        .expect_err("pending renewal must block fresh upgrade preparation");
    assert!(matches!(
        error.downcast_ref::<UpgradeError>(),
        Some(UpgradeError::PendingRenewalBlocksPrepare)
    ));

    let root_idempotent = tempfile::tempdir().unwrap();
    let idempotent_context = context(root_idempotent.path());
    private_dir(&idempotent_context.candidate_tee_dir);
    prepare_upgrade_journal_v1(root_idempotent.path(), idempotent_context.clone()).unwrap();
    store_pending_renewal(root_idempotent.path());
    prepare_upgrade_journal_v1(root_idempotent.path(), idempotent_context)
        .expect("identical already-prepared upgrade context remains idempotent");
    let error = record_network_key_provisioned_v1(root_idempotent.path())
        .expect_err("pending renewal must block network-key provisioning checkpoint");
    assert!(matches!(
        error.downcast_ref::<UpgradeError>(),
        Some(UpgradeError::PendingRenewalBlocksProvision)
    ));
}
