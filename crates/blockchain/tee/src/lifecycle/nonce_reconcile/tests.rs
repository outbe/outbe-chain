use super::*;
use crate::registry_view::NodeBindingSelectorV1;
use crate::{
    dcap_protocol::DcapOnboardingContextV1, in_process::ProofSourceV1,
    registry_view::RenewalBindingV1, resident::ResidentTeeKeys,
    upgrade_transfer::UpgradeKeyProofV1, NodeHostIdentityV1,
};
use alloy_consensus::{transaction::RlpEcdsaEncodableTx, SignableTransaction, TxEip1559};
use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_primitives::{
    tee_attestation_v1::*, tee_operator_v1::TeeRenewalScheduleV1,
    tee_registry_abi_v1::NodeEnclaveBindingV1View, validator_dto::ValidatorRecord,
};
use std::sync::atomic::{AtomicUsize, Ordering};

type PortResult<T> = Result<T, LifecyclePortError<std::io::Error>>;
struct Chain {
    at: LifecycleCheckpointV1,
    binding: Option<RenewalBindingV1>,
    candidate: Option<LifecycleUpgradeCandidateV1>,
    receipt: Option<LifecycleReceiptV1>,
    nonces: [u64; 3],
    reads: AtomicUsize,
    receipt_reads: AtomicUsize,
    fail_receipt: bool,
}
impl LifecycleChainPort for Chain {
    type Error = std::io::Error;
    async fn finalized_checkpoint(&self) -> PortResult<LifecycleCheckpointV1> {
        Ok(self.at)
    }
    async fn registry_state(
        &self,
        at: LifecycleCheckpointV1,
        _: &NodeBindingSelectorV1,
    ) -> PortResult<LifecycleRegistryStateV1> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(LifecycleRegistryStateV1 {
            checkpoint: at,
            binding: self.binding.clone(),
            candidate: self.candidate,
            policy: outbe_primitives::tee_test_utils::no_attestation_dev_policy_v1(
                at.chain_id,
                at.genesis_hash,
            )
            .unwrap(),
            schedule: TeeRenewalScheduleV1 {
                finalized_height: at.height,
                finalized_hash: at.hash,
                finalized_timestamp: at.timestamp,
                epoch_number: 0,
                epoch_start_height: 0,
                epoch_length_blocks: 100,
                next_freeze_height: 90,
                planned_activation_height: 100,
                dkg_prepare_window_blocks: 10,
                minimum_block_time_millis: 1000,
            },
            active_upgrade: None,
            successor_policy: None,
            network_encryption_public_key: B256::repeat_byte(9),
            key_epoch: 0,
            network_encryption_epoch: 0,
        })
    }
    async fn validator_record(
        &self,
        _: LifecycleCheckpointV1,
        _: Address,
    ) -> PortResult<Option<ValidatorRecord>> {
        // Reconciliation must not gate completed registration on current jail status.
        Ok(Some(ValidatorRecord {
            validator_address: Address::ZERO,
            consensus_pubkey: [0; 48],
            stake: U256::ZERO,
            status: VALIDATOR_STATUS_JAILED,
            slash_count: 0,
            missed_blocks: 0,
            missed_votes: 0,
            blocks_proposed: 0,
            joined_at_height: 0,
            deactivated_at_height: 0,
            unbonding_end: 0,
            has_bls_share: false,
        }))
    }
    async fn balance(&self, _: LifecycleCheckpointV1, _: Address) -> PortResult<U256> {
        Ok(U256::ZERO)
    }
    async fn account_nonce(&self, _: Address, tag: LifecycleNonceTagV1) -> PortResult<u64> {
        Ok(self.nonces[match tag {
            LifecycleNonceTagV1::Finalized(_) => 0,
            LifecycleNonceTagV1::Latest => 1,
            LifecycleNonceTagV1::Pending => 2,
        }])
    }
    async fn send_raw_transaction(&self, _: &[u8]) -> PortResult<B256> {
        Err(LifecyclePortError::Source(std::io::Error::other(
            "reconciliation must not send",
        )))
    }
    async fn finalized_receipt(
        &self,
        _: LifecycleCheckpointV1,
        _: B256,
    ) -> PortResult<Option<LifecycleReceiptV1>> {
        self.receipt_reads.fetch_add(1, Ordering::SeqCst);
        if self.fail_receipt {
            return Err(LifecyclePortError::Source(std::io::Error::other(
                "unavailable",
            )));
        }
        Ok(self.receipt)
    }
    async fn onboarding_proof(
        &self,
        _: LifecycleCheckpointV1,
        _: B256,
    ) -> PortResult<LifecycleOnboardingProofV1> {
        Err(LifecyclePortError::Source(std::io::Error::other("unused")))
    }
    async fn upgrade_key(
        &self,
        _: LifecycleCheckpointV1,
        _: &DcapOnboardingContextV1,
        _: &UpgradeKeyProofV1,
    ) -> PortResult<LifecycleOnboardingProofV1> {
        Err(LifecyclePortError::Source(std::io::Error::other("unused")))
    }
}

fn context(seed: u8) -> InProcessTeeContext {
    let keys = ResidentTeeKeys::from_seed([seed; 32]).unwrap();
    let signer = k256::ecdsa::SigningKey::from_slice(&[3; 32]).unwrap();
    let node_id = NodeIdV1 {
        reth_p2p_public: signer
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap(),
    };
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: U256::from(424242).to_be_bytes(),
        genesis_hash: B256::repeat_byte(2),
        attestation_mode: AttestationMode::NoAttestationDev,
        node_id,
        initialization_challenge: [3; 32],
        node_host_noise_x25519: [4; 32],
        recipient_x25519: keys.tribute_offer_public(),
        attestation_ed25519: keys.attestation_pub(),
        noise_responder_x25519: keys.noise_public(),
    };
    let identity = NodeHostIdentityV1 {
        network_binding: manifest.network_binding(),
        reth_p2p_public: manifest.node_id.reth_p2p_public,
    };
    let descriptor = TrustedNetworkDescriptorV1 {
        network_binding: manifest.network_binding(),
        genesis_consensus_keys: vec![],
    };
    InProcessTeeContext::new(
        keys,
        identity,
        manifest,
        descriptor,
        ProofSourceV1::NoHardware,
    )
    .unwrap()
}
fn fixture() -> (
    tempfile::TempDir,
    ServiceTxJournalV1,
    InProcessTeeContext,
    ServiceTxRecordV1,
    Chain,
) {
    let root = tempfile::tempdir().unwrap();
    let journal = ServiceTxJournalV1::open(root.path()).unwrap();
    let live = context(9);
    let at = LifecycleCheckpointV1 {
        height: 10,
        hash: B256::repeat_byte(10),
        state_root: B256::repeat_byte(11),
        timestamp: 100,
        chain_id: 424242,
        genesis_hash: live.manifest().genesis_hash,
        base_fee_per_gas: U256::from(1),
    };
    let tx = TxEip1559 {
        chain_id: at.chain_id,
        nonce: 4,
        gas_limit: 500000,
        to: alloy_primitives::TxKind::Call(outbe_primitives::addresses::TEE_REGISTRY_ADDRESS),
        ..Default::default()
    };
    let signer = k256::ecdsa::SigningKey::from_slice(&[3; 32]).unwrap();
    let (signature, recovery) = signer
        .sign_prehash_recoverable(tx.signature_hash().as_slice())
        .unwrap();
    let signature =
        alloy_primitives::Signature::from_signature_and_parity(signature, recovery.is_y_odd());
    let signed = tx.into_signed(signature);
    let sender = signed.recover_signer().unwrap();
    let mut raw = vec![2];
    signed.tx().rlp_encode_signed(signed.signature(), &mut raw);
    let record = ServiceTxRecordV1 {
        tx_hash: keccak256(&raw),
        raw_signed_tx: raw.into(),
        sender,
        nonce: 4,
        kind: ServiceTxKindV1::Register,
        public_intent: ServiceTxIntentV1 {
            target: ServiceBindingTargetV1 {
                node_id_hash: live.manifest().node_id.node_id_hash().unwrap(),
                enclave_id: live.manifest().enclave_id().unwrap(),
                binding_id: B256::repeat_byte(5),
                binding_version: 2,
                registration_version: 2,
                renewal_nonce: 0,
                transition_nonce: 0,
                valid_until: 2000,
                policy_hash: B256::repeat_byte(6),
                recipient_x25519: live.recipient_public().into(),
            },
            candidate: None,
        },
        finalized_checkpoint: at,
        state: ServiceTxStateV1::Pending,
        reconciled_checkpoint: None,
    };
    let chain = Chain {
        at,
        binding: None,
        candidate: None,
        receipt: None,
        nonces: [4; 3],
        reads: AtomicUsize::new(0),
        receipt_reads: AtomicUsize::new(0),
        fail_receipt: false,
    };
    (root, journal, live, record, chain)
}
fn applied(record: &ServiceTxRecordV1) -> RenewalBindingV1 {
    let t = &record.public_intent.target;
    RenewalBindingV1::try_from(NodeEnclaveBindingV1View {
        exists: true,
        nodeIdHash: t.node_id_hash,
        enclaveId: t.enclave_id,
        bindingId: t.binding_id,
        bindingVersion: t.binding_version,
        registrationVersion: t.registration_version,
        renewalNonce: t.renewal_nonce,
        transitionNonce: t.transition_nonce,
        validUntil: t.valid_until,
        policyHash: t.policy_hash,
        recipientX25519: t.recipient_x25519,
        ..Default::default()
    })
    .unwrap()
}

#[tokio::test]
async fn pending_then_finalized_registration_ingests_even_when_jailed() {
    let (_root, mut journal, live, record, mut chain) = fixture();
    journal.begin(record.clone()).unwrap();
    assert_eq!(
        reconcile_service_tx_v1(&chain, &mut journal, &live)
            .await
            .unwrap(),
        ServiceTxReconciliationV1::ReplayExact
    );
    chain.binding = Some(applied(&record));
    chain.nonces = [5; 3];
    assert_eq!(
        reconcile_service_tx_v1(&chain, &mut journal, &live)
            .await
            .unwrap(),
        ServiceTxReconciliationV1::IngestOnboarding
    );
    assert_eq!(
        journal.load().unwrap().unwrap().state,
        ServiceTxStateV1::Completed
    );
    assert_eq!(chain.receipt_reads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn external_nonce_waits_for_finality_then_rebuilds_without_replacing_live_context() {
    let (_root, mut journal, live, record, mut chain) = fixture();
    journal.begin(record.clone()).unwrap();
    chain.nonces = [4, 5, 6];
    assert_eq!(
        reconcile_service_tx_v1(&chain, &mut journal, &live)
            .await
            .unwrap(),
        ServiceTxReconciliationV1::WaitForFinality {
            latest_nonce: 5,
            pending_nonce: 6
        }
    );
    chain.nonces = [5, 5, 6];
    assert_eq!(
        reconcile_service_tx_v1(&chain, &mut journal, &live)
            .await
            .unwrap(),
        ServiceTxReconciliationV1::RebuildAndResign { nonce: 6 }
    );
    assert_eq!(
        live.manifest().enclave_id().unwrap(),
        record.public_intent.target.enclave_id
    );
    assert_eq!(
        live.recipient_public(),
        record.public_intent.target.recipient_x25519.0
    );
}

#[tokio::test]
async fn restarted_process_discards_old_recipient_only_after_chain_reconciliation() {
    for finalized in [false, true] {
        let (_root, mut journal, _old, record, mut chain) = fixture();
        journal.begin(record.clone()).unwrap();
        let fresh = context(10);
        let path = _root.path().join("tee-service-tx-v1/journal.json");
        let before = std::fs::read(&path).unwrap();
        for nonces in [[3, 4, 4], [4, 4, 5]] {
            chain.nonces = nonces;
            assert_eq!(
                reconcile_service_tx_v1(&chain, &mut journal, &fresh)
                    .await
                    .unwrap(),
                ServiceTxReconciliationV1::WaitForFinality {
                    latest_nonce: nonces[1],
                    pending_nonce: nonces[2],
                }
            );
            assert_eq!(journal.load().unwrap().unwrap(), record);
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        chain.nonces = [5; 3];
        if finalized {
            chain.binding = Some(applied(&record));
        }
        assert_eq!(
            reconcile_service_tx_v1(&chain, &mut journal, &fresh)
                .await
                .unwrap(),
            ServiceTxReconciliationV1::DiscardLostIdentity {
                mutation_finalized: finalized
            }
        );
        assert_eq!(chain.reads.load(Ordering::SeqCst), 3);
        assert_eq!(chain.receipt_reads.load(Ordering::SeqCst), 3);
        use std::os::unix::fs::MetadataExt as _;
        let inode = std::fs::metadata(&path).unwrap().ino();
        let terminal = std::fs::read(&path).unwrap();
        chain.at.height += 1;
        chain.at.hash = B256::repeat_byte(20);
        assert_eq!(
            reconcile_service_tx_v1(&chain, &mut journal, &fresh)
                .await
                .unwrap(),
            ServiceTxReconciliationV1::DiscardLostIdentity {
                mutation_finalized: finalized
            }
        );
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
        assert_eq!(std::fs::read(&path).unwrap(), terminal);
        assert_eq!(
            journal.load().unwrap().unwrap().state,
            ServiceTxStateV1::DiscardedLostIdentity
        );
    }
}

#[test]
fn journal_roundtrip_is_private_locked_and_preserves_unresolved_bytes() {
    use std::os::unix::fs::PermissionsExt as _;
    let (root, mut journal, _live, record, _) = fixture();
    journal.begin(record.clone()).unwrap();
    assert!(ServiceTxJournalV1::open(root.path()).is_err());
    let path = root.path().join("tee-service-tx-v1/journal.json");
    let original = std::fs::read(&path).unwrap();
    let mut other = record.clone();
    other.public_intent.target.valid_until += 1;
    assert!(matches!(
        journal.begin(other),
        Err(ServiceTxJournalError::UnresolvedTransaction)
    ));
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(journal);
    let journal = ServiceTxJournalV1::open(root.path()).unwrap();
    assert_eq!(journal.load().unwrap(), Some(record));
}

mod recovery;
mod storage;
