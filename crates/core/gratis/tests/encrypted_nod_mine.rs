//! Pure NOD mine and the resident host path.

use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolError;
use outbe_core_common::confidential_tee::hkdf_sha256;
use outbe_core_common::nod_encryption::NodTermsV1;
use outbe_fidelity::{apply_cohort_op, FidelityCohortOp, FidelityCohortRequest, FidelityOpSection};
use outbe_gratis::enclave_client::mine_encrypted_nod;
use outbe_gratis::nod_mine::{apply, MineEncryptedNodRequestV1, MineKeys};
use outbe_gratis::precompile::IGratis;
use outbe_gratis::{
    apply_op, decrypt_balance, decrypt_gratis_balance, derive_account_keys, derive_view_key,
    modify_mac, GratisOp, GratisOpRequest, GratisOpStatus, ModifyAuth, ModifyOperation,
};
use outbe_nod::nod_encryption::encrypt_nod;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
use outbe_system_common::time::WorldwideDay;
use outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test;
use outbe_tee::resident::{ResidentOfferMaterial, StateKeyHandle};
use outbe_tee::witness::{
    EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessExecutionScope, WitnessState,
};

#[test]
fn private_mine_replays_exactly_and_preserves_other_gratis_writers() {
    let network = StateKeyHandle::for_test([7; 32]);
    let key = StateKeyHandle::for_test([9; 32]);
    let fidelity_key = StateKeyHandle::for_test([13; 32]);
    let keys = MineKeys {
        nod: &network,
        gratis: &key,
        fidelity: &fidelity_key,
    };
    let account = Address::repeat_byte(3);
    let amount = U256::from(123u64);
    let day = WorldwideDay::new(20250115);
    let terms = NodTermsV1 {
        chain_id: 1,
        nod_id: U256::from_be_bytes([4; 32]),
        owner: account,
        emission_day: day.value(),
        entry_price_minor: U256::ONE,
        issuance_currency: 840,
        reference_currency: 978,
    };
    let nod = encrypt_nod(&network, terms, amount, amount).unwrap();
    let modify_key = derive_account_keys(&key, account).unwrap();
    let chain = B256::from(U256::ONE);
    let auth = |op, value, nonce| ModifyAuth {
        mac: modify_mac(
            &modify_key,
            &ModifyOperation {
                account,
                op,
                amount: value,
                op_nonce: nonce,
                chain_id: chain,
            },
        ),
        op_nonce: nonce,
    };
    let request = MineEncryptedNodRequestV1 {
        nod,
        current_balance: vec![],
        modify_auth: auth(GratisOp::Mint, amount, 0),
        fidelity: FidelityOpSection {
            op: FidelityCohortOp::In,
            timestamp: 1_000_000,
            first_qualified_start: 0,
            current_blob: vec![],
        },
    };
    let first = apply(keys, &request).unwrap();
    assert_eq!(first, apply(keys, &request).unwrap());
    assert_eq!(first.next_op_nonce, 1);
    assert!(!first.fidelity.new_blob.is_empty());
    let standalone = apply_cohort_op(
        &fidelity_key,
        &FidelityCohortRequest {
            chain_id: chain,
            account,
            amount: U256::ONE,
            section: FidelityOpSection {
                op: FidelityCohortOp::In,
                timestamp: 1_000_001,
                first_qualified_start: 1_000_000,
                current_blob: first.fidelity.new_blob.clone(),
            },
        },
    )
    .unwrap();
    assert_eq!(&standalone.outcome.new_blob[8..12], b"FID2");
    let mut resumed = request.clone();
    resumed.current_balance = first.new_balance.clone();
    resumed.modify_auth = auth(GratisOp::Mint, amount, 1);
    resumed.fidelity.current_blob = standalone.outcome.new_blob;
    resumed.fidelity.timestamp = 1_000_002;
    resumed.fidelity.first_qualified_start = 1_000_000;
    assert!(apply(keys, &resumed).is_ok());
    let view_key = hkdf_sha256(&[9; 32], account.as_slice(), b"outbe/gratis/view-key/v1").unwrap();
    assert_eq!(
        decrypt_gratis_balance(&view_key, account, &first.new_balance).unwrap(),
        amount
    );
    let encoded = serde_json::to_value(&first).unwrap();
    assert!(encoded.get("amount").is_none());
    assert!(encoded.get("event_amount").is_none());
    let mut wrong = request.clone();
    wrong.modify_auth = auth(GratisOp::Mint, amount + U256::ONE, 0);
    assert!(apply(keys, &wrong).is_err());
    let burn = GratisOpRequest {
        op: GratisOp::Burn,
        chain_id: chain,
        account,
        amount: U256::from(23u64),
        current_balance: first.new_balance.clone(),
        current_pledged: Vec::new(),
        modify_auth: auth(GratisOp::Burn, U256::from(23u64), 1),
        fidelity: None,
        sealed_pledge: None,
    };
    let after = apply_op(&key, &burn);
    assert!(matches!(after.status, GratisOpStatus::Applied));
    assert_eq!(
        decrypt_balance(
            &derive_view_key(&key, account).unwrap(),
            account,
            &after.new_balance
        )
        .unwrap(),
        U256::from(100u64)
    );
    let mut invalid_fidelity = request;
    invalid_fidelity.fidelity.current_blob = vec![1, 2, 3];
    assert!(apply(keys, &invalid_fidelity).is_err());
}

#[test]
fn resident_dispatch_mines_and_signs_deterministic_refusals() {
    let binding = NetworkBindingV1 {
        chain_id: U256::from(1u64).to_be_bytes(),
        genesis_hash: B256::repeat_byte(2),
        attestation_mode: AttestationMode::NoAttestationDev,
    };
    let context = Arc::new(outbe_tee::test_utils::resident_context(7, binding));
    let material =
        ResidentOfferMaterial::for_test(vec![5; 96], B256::from(U256::ONE), 0, 0).expect("offer");
    context.activate_for_test(material).expect("activate");
    let nod_key = context
        .state_key_handle(outbe_core_common::confidential_tee::NOD.state_info(), 0)
        .unwrap();
    let gratis_key = context
        .state_key_handle(outbe_core_common::confidential_tee::GRATIS.state_info(), 0)
        .unwrap();
    let account = Address::repeat_byte(3);
    let amount = U256::from(123u64);
    let nod = encrypt_nod(
        &nod_key,
        NodTermsV1 {
            chain_id: 1,
            nod_id: U256::from_be_bytes([4; 32]),
            owner: account,
            emission_day: 20250115,
            entry_price_minor: U256::ONE,
            issuance_currency: 840,
            reference_currency: 978,
        },
        amount,
        amount,
    )
    .unwrap();
    let account_keys = derive_account_keys(&gratis_key, account).unwrap();
    let mut request = MineEncryptedNodRequestV1 {
        nod,
        current_balance: Vec::new(),
        modify_auth: ModifyAuth {
            mac: modify_mac(
                &account_keys,
                &ModifyOperation {
                    account,
                    op: GratisOp::Mint,
                    amount,
                    op_nonce: 0,
                    chain_id: B256::from(U256::ONE),
                },
            ),
            op_nonce: 0,
        },
        fidelity: FidelityOpSection {
            op: FidelityCohortOp::In,
            timestamp: 1_000_000,
            first_qualified_start: 0,
            current_blob: Vec::new(),
        },
    };
    let leader = context.attestation_public();
    let _guard = set_in_process_tee_context_for_test(Some(Arc::clone(&context)));
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::ProposerRecord,
        Some(leader),
        EnclaveWitnessTapeV1::default(),
    )));
    let _scope = WitnessExecutionScope::enter(state);
    let first = mine_encrypted_nod(request.clone()).unwrap();
    assert_eq!(first, mine_encrypted_nod(request.clone()).unwrap());
    assert_eq!(first.next_op_nonce, 1);
    assert!(!first.fidelity.new_blob.is_empty());
    assert_eq!(
        decrypt_balance(
            &derive_view_key(&gratis_key, account).unwrap(),
            account,
            &first.new_balance
        )
        .unwrap(),
        amount
    );
    request.modify_auth.mac[0] ^= 1;
    let err = mine_encrypted_nod(request).unwrap_err();
    let PrecompileError::RevertBytes(bytes) = &err else {
        panic!("flipped MAC was not a signed refusal: {err:?}");
    };
    let decoded = IGratis::Rejected::abi_decode(bytes).expect("Rejected");
    assert!(decoded.value.contains("invalid modify authorization"));
}
