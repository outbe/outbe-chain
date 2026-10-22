use super::super::*;
use crate::rpc::ChainRpc;
use crate::tx::RawRelayTransactionV1;
use alloy_consensus::{
    transaction::SignerRecoverable as _, EthereumTxEnvelope, Transaction as _, TxEip4844,
};
use alloy_eips::eip2718::Decodable2718 as _;
use alloy_primitives::{Address, TxKind};
use ed25519_dalek::Signer as _;
use outbe_primitives::{
    chain::DEVNET_CHAIN_ID,
    tee_attestation_v1::{AttestationMode, EnclaveInitializationManifestV1, NodeIdV1},
    tee_genesis_v1::{initial_tee_policy_v1, InitialTeeProfileV1},
    tee_operator_v1::TeeRenewalScheduleV1,
};

struct DirectOnlyEnclave {
    signer: ed25519_dalek::SigningKey,
    dcap_calls: usize,
    direct_calls: usize,
}

impl RenewalEnclaveV1 for DirectOnlyEnclave {
    fn generate_dcap_quote(
        &mut self,
        _intent: &RegistrationIntentV1,
    ) -> Result<GeneratedDcapQuoteV1> {
        self.dcap_calls += 1;
        Err(RenewalError::TestFault("DCAP must not be invoked for GramineDirectDev renewal").into())
    }

    fn sign_registration_intent_dev_v1(
        &mut self,
        intent: &RegistrationIntentV1,
    ) -> Result<[u8; 64]> {
        self.direct_calls += 1;
        Ok(self
            .signer
            .sign(intent.intent_hash().unwrap().as_slice())
            .to_bytes())
    }
}

struct PreparationRpc;

impl ChainRpc for PreparationRpc {
    async fn chain_id(&self) -> Result<u64> {
        Ok(DEVNET_CHAIN_ID)
    }
}

impl RelayPreparationRpc for PreparationRpc {
    async fn gas_price(&self) -> Result<U256> {
        Ok(U256::from(1_000_000_000_u64))
    }

    async fn transaction_count(&self, _address: Address) -> Result<u64> {
        Ok(7)
    }

    async fn balance(&self, _address: Address) -> Result<U256> {
        Ok(U256::ZERO)
    }
}

fn binding() -> RenewalBindingV1 {
    crate::test_support::RenewalBindingFixtureV1::new(1)
        .versions(1, 1)
        .nonces(1, 0)
        .lease(100, 400, 500)
        .claims(1, 1, 0)
        .build()
}

#[test]
fn renewal_input_fixture_preserves_its_independent_v1_json() {
    assert_eq!(
        serde_json::to_string(&binding()).unwrap(),
        r#"{"nodeIdHash":"0x0101010101010101010101010101010101010101010101010101010101010101","enclaveId":"0x0202020202020202020202020202020202020202020202020202020202020202","bindingId":"0x0303030303030303030303030303030303030303030303030303030303030303","intentHash":"0x0404040404040404040404040404040404040404040404040404040404040404","evidenceHash":"0x0505050505050505050505050505050505050505050505050505050505050505","policyHash":"0x0606060606060606060606060606060606060606060606060606060606060606","bindingVersion":1,"registrationVersion":1,"renewalNonce":1,"transitionNonce":0,"leaseStartedAt":100,"validUntil":400,"collateralValidUntil":500,"recipientX25519":"0x0707070707070707070707070707070707070707070707070707070707070707","attestationEd25519":"0x0808080808080808080808080808080808080808080808080808080808080808","noiseResponderX25519":"0x0909090909090909090909090909090909090909090909090909090909090909","mrenclave":"0x0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a","mrsigner":"0x0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b","isvProdId":1,"isvSvn":1,"platformTcbStatus":0,"verdictHash":"0x0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c","nodeHostAuthorizationHash":"0x0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d"}"#
    );
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
fn renewal_window_is_the_last_half_period_and_excludes_the_deadline() {
    let binding = binding();
    let lease_period = 200;
    assert_eq!(renewal_opens_at(&binding, lease_period).unwrap(), 300);
    assert!(!renewal_is_open(&binding, 299, lease_period).unwrap());
    assert!(renewal_is_open(&binding, 300, lease_period).unwrap());
    assert!(renewal_is_open(&binding, 399, lease_period).unwrap());
    assert!(renewal_is_open(&binding, 400, lease_period).is_err());
    assert_eq!(next_renewal_deadline(&binding, lease_period).unwrap(), 600);
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
fn replay_transport_errors_are_classified_narrowly() {
    let report = |msg| eyre::Report::from(RenewalError::TestFault(msg));
    assert!(transaction_is_already_known(&report("already known")));
    assert!(transaction_is_already_known(&report("known transaction")));
    assert!(!transaction_is_already_known(&report("nonce too low")));
    assert!(transaction_nonce_is_too_low(&report(
        "nonce too low: next nonce 1, tx nonce 0"
    )));
    assert!(!transaction_nonce_is_too_low(&report(
        "replacement transaction underpriced"
    )));
}

#[tokio::test]
async fn direct_dev_prepare_builds_the_shared_calldata_and_relay_transaction() {
    let genesis_hash = B256::repeat_byte(0x51);
    let policy = initial_tee_policy_v1(
        InitialTeeProfileV1::GramineDirectDev,
        DEVNET_CHAIN_ID,
        genesis_hash,
    )
    .unwrap();
    let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[0x52; 32]);
    let node_signer = k256::ecdsa::SigningKey::from_bytes((&[0x53; 32]).into()).unwrap();
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
        initialization_challenge: [0x54; 32],
        node_host_noise_x25519: [0x55; 32],
        recipient_x25519: [0x56; 32],
        attestation_ed25519: enclave_signer.verifying_key().to_bytes(),
        noise_responder_x25519: [0x57; 32],
    };
    let binding = RenewalBindingV1 {
        node_id_hash: node_id.node_id_hash().unwrap(),
        enclave_id: manifest.enclave_id().unwrap(),
        binding_id: B256::repeat_byte(0x58),
        intent_hash: B256::repeat_byte(0x59),
        evidence_hash: B256::repeat_byte(0x5a),
        policy_hash: policy.policy_hash().unwrap(),
        binding_version: 1,
        registration_version: 3,
        renewal_nonce: 2,
        transition_nonce: 0,
        lease_started_at: 100,
        valid_until: 1_000,
        collateral_valid_until: u64::MAX,
        recipient_x25519: B256::from(manifest.recipient_x25519),
        attestation_ed25519: B256::from(manifest.attestation_ed25519),
        noise_responder_x25519: B256::from(manifest.noise_responder_x25519),
        mrenclave: policy.measurement_rules[0].mrenclave,
        mrsigner: policy.measurement_rules[0].mrsigner,
        isv_prod_id: policy.measurement_rules[0].isv_prod_id,
        isv_svn: policy.measurement_rules[0].minimum_isv_svn,
        platform_tcb_status: 0,
        verdict_hash: B256::repeat_byte(0x5b),
        node_host_authorization_hash: manifest.node_host_authorization_hash().unwrap(),
    };
    let schedule = TeeRenewalScheduleV1 {
        finalized_height: 120,
        finalized_hash: B256::repeat_byte(0x5c),
        finalized_timestamp: 900,
        epoch_number: 2,
        epoch_start_height: 100,
        epoch_length_blocks: 100,
        next_freeze_height: 180,
        planned_activation_height: 200,
        dkg_prepare_window_blocks: 20,
        minimum_block_time_millis: 2_000,
    };
    let view = FinalizedRenewalChainViewV1 {
        schedule,
        policy,
        binding,
        tribute_offer_public: B256::repeat_byte(0x5d),
    };
    let config = RenewalServiceConfigV1 {
        node_data_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
        selector: NodeBindingSelectorV1::NodeHost(node_id.reth_p2p_public),
        manifest,
    };
    let relay = RelaySignerV1::new(&hex::encode([0x5e; 32])).unwrap();
    let mut enclave = DirectOnlyEnclave {
        signer: enclave_signer,
        dcap_calls: 0,
        direct_calls: 0,
    };

    assert_eq!(
        PreparationRpc.balance(relay.address()).await.unwrap(),
        U256::ZERO
    );
    let attempt = prepare_attempt(
        &mut RenewalPreparation {
            rpc: &PreparationRpc,
            evm_signer: &relay,
            enclave: &mut enclave,
            node_signer: &|_| Ok([0x5f; 65]),
            config: &config,
        },
        &view,
    )
    .await
    .unwrap();
    assert_eq!(enclave.dcap_calls, 0);
    assert_eq!(enclave.direct_calls, 1);
    assert_eq!(attempt.collateral_valid_until, u64::MAX);
    assert_eq!(attempt.collateral_margin, 0);
    let decoded_intent = RegistrationIntentV1::decode_canonical(&attempt.intent).unwrap();
    let decoded_evidence = AttestationEvidenceV1::decode_canonical(&attempt.evidence).unwrap();
    assert_eq!(
        decoded_evidence.evidence_hash().unwrap(),
        attempt.evidence_hash
    );
    let AttestationEvidenceV1::GramineDirectDev(direct) = decoded_evidence else {
        panic!("expected GramineDirectDev evidence");
    };
    assert_eq!(direct.intent, decoded_intent);
    assert_eq!(
        direct.dev_attestation_public,
        decoded_intent.attestation_ed25519
    );
    assert_eq!(
        direct.dev_signature.as_slice(),
        attempt.enclave_signature.as_slice()
    );
    let call = ITeeRegistryV1::renewEnclaveCall::abi_decode(&attempt.calldata).unwrap();
    assert_eq!(call.evidence.as_ref(), attempt.evidence);
    assert_eq!(call.nodeSignature.as_ref(), attempt.node_signature);
    assert_eq!(call.enclaveSignature.as_ref(), attempt.enclave_signature);
    assert_eq!(attempt.relay, relay.address());
    assert_eq!(attempt.relay_variants.len(), 1);
    assert_eq!(attempt.relay_variants[0].account_nonce, 7);
    assert_eq!(
        attempt.relay_variants[0].calldata_hash,
        attempt.calldata_hash
    );
    let raw_variant = &attempt.relay_variants[0];
    let mut raw = raw_variant.raw_transaction.as_slice();
    let envelope = EthereumTxEnvelope::<TxEip4844>::decode_2718(&mut raw).unwrap();
    assert!(raw.is_empty());
    let EthereumTxEnvelope::Eip1559(signed) = &envelope else {
        panic!("new renewal attempt must be EIP-1559");
    };
    assert_eq!(signed.tx().max_priority_fee_per_gas, 0);
    assert!(signed.tx().access_list.is_empty());
    assert_eq!(envelope.recover_signer().unwrap(), relay.address());
    assert_eq!(envelope.chain_id(), Some(DEVNET_CHAIN_ID));
    assert_eq!(envelope.nonce(), 7);
    let normative_gas = TeeRegistryGasScheduleV1::normative()
        .maximum_transaction_gas(
            RegistryMutatorV1::RenewEnclave,
            attempt.calldata.len(),
            attempt.evidence.len(),
            view.policy.measurement_rules.len(),
            AttestationMode::GramineDirectDev,
        )
        .unwrap();
    assert_eq!(raw_variant.gas_limit, normative_gas);
    assert_eq!(envelope.gas_limit(), raw_variant.gas_limit);
    assert_eq!(
        envelope.max_fee_per_gas(),
        raw_variant.gas_price.to::<u128>()
    );
    assert_eq!(envelope.max_priority_fee_per_gas(), Some(0));
    assert_eq!(envelope.kind(), TxKind::Call(TEE_REGISTRY_ADDRESS));
    assert_eq!(envelope.value(), U256::ZERO);
    assert_eq!(envelope.input().as_ref(), attempt.calldata.as_slice());
    assert_eq!(*envelope.tx_hash(), raw_variant.transaction_hash);
}
