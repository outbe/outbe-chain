use alloy_primitives::{Address, B256, U256};
use outbe_core_common::nod_encryption::NodTermsV1;
use outbe_enclave::{gratis, nod_encryption::encrypt_nod, nod_mine};
use outbe_enclave::{
    nod_mine::MineEncryptedNodRequestV1,
    protocol::{
        FidelityCohortOp, FidelityOpSection, GratisOp, GratisOpRequest, GratisOpStatus, ModifyAuth,
    },
};
use outbe_system_common::time::WorldwideDay;
use outbe_tee::resident::StateKeyHandle;

#[test]
fn private_mine_replays_exactly_and_preserves_other_gratis_writers() {
    let network = StateKeyHandle::for_test([7; 32]);
    let key = StateKeyHandle::for_test([9; 32]);
    let fidelity_key = StateKeyHandle::for_test([13; 32]);
    let account = Address::repeat_byte(3);
    let amount = U256::from(123);
    let day = WorldwideDay::new(20250115);
    let terms = NodTermsV1 {
        chain_id: 1,
        nod_id: U256::from_be_bytes([4; 32]),
        owner: account,
        emission_day: day.value(),
        league_id: 0,
        entry_price_minor: U256::ONE,
        issuance_currency: 840,
        reference_currency: 978,
    };
    let nod = encrypt_nod(&network, terms, amount).unwrap();
    let modify_key = gratis::derive_account_keys(&key, account).unwrap();
    let chain = B256::from(U256::ONE);
    let auth = |op, value, nonce| ModifyAuth {
        mac: gratis::modify_mac(
            &modify_key,
            &gratis::ModifyOperation {
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
    let first = nod_mine::apply(&network, &key, &fidelity_key, &request).unwrap();
    assert_eq!(
        first,
        nod_mine::apply(&network, &key, &fidelity_key, &request).unwrap()
    );
    assert_eq!(first.next_op_nonce, 1);
    assert!(!first.fidelity.new_blob.is_empty());
    let standalone = outbe_enclave::fidelity::apply_cohort_op(
        &fidelity_key,
        &outbe_enclave::protocol::FidelityCohortRequest {
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
    assert!(nod_mine::apply(&network, &key, &fidelity_key, &resumed).is_ok());
    let view_key = outbe_enclave::crypto::hkdf_sha256(
        &[9; 32],
        account.as_slice(),
        b"outbe/gratis/view-key/v1",
    )
    .unwrap();
    assert_eq!(
        outbe_enclave::gratis_decrypt::decrypt_gratis_balance(
            &view_key,
            account,
            &first.new_balance
        )
        .unwrap(),
        amount
    );
    let encoded = serde_json::to_value(&first).unwrap();
    assert!(encoded.get("amount").is_none());
    assert!(encoded.get("event_amount").is_none());
    let mut wrong = request.clone();
    wrong.modify_auth = auth(GratisOp::Mint, amount + U256::ONE, 0);
    assert!(nod_mine::apply(&network, &key, &fidelity_key, &wrong).is_err());
    let burn = GratisOpRequest {
        op: GratisOp::Burn,
        chain_id: chain,
        account,
        amount: U256::from(23),
        current_balance: first.new_balance.clone(),
        current_pledged: Vec::new(),
        modify_auth: auth(GratisOp::Burn, U256::from(23), 1),
        fidelity: None,
    };
    let after = gratis::apply_op(&key, &burn);
    assert!(matches!(after.status, GratisOpStatus::Applied));
    assert_eq!(
        gratis::decrypt_balance(
            &gratis::derive_view_key(&key, account).unwrap(),
            account,
            &after.new_balance
        )
        .unwrap(),
        U256::from(100)
    );
    let mut invalid_fidelity = request;
    invalid_fidelity.fidelity.current_blob = vec![1, 2, 3];
    assert!(nod_mine::apply(&network, &key, &fidelity_key, &invalid_fidelity).is_err());
}

#[path = "encrypted_tribute/resident.rs"]
mod resident;

#[test]
fn resident_dispatch_mines_and_signs_deterministic_refusals() {
    use outbe_enclave::protocol::{EnclaveRequest, EnclaveResponse};
    let context = resident::context(1);
    let nod_key = resident::state(
        &context,
        outbe_enclave::nod_encryption::derive_nod_state_key,
    );
    let gratis_key = resident::state(&context, gratis::derive_gratis_state_key);
    let account = Address::repeat_byte(3);
    let amount = U256::from(123);
    let nod = encrypt_nod(
        &nod_key,
        NodTermsV1 {
            chain_id: 1,
            nod_id: U256::from_be_bytes([4; 32]),
            owner: account,
            emission_day: 20250115,
            league_id: 0,
            entry_price_minor: U256::ONE,
            issuance_currency: 840,
            reference_currency: 978,
        },
        amount,
    )
    .unwrap();
    let keys = gratis::derive_account_keys(&gratis_key, account).unwrap();
    let mut request = MineEncryptedNodRequestV1 {
        nod,
        current_balance: Vec::new(),
        modify_auth: ModifyAuth {
            mac: gratis::modify_mac(
                &keys,
                &gratis::ModifyOperation {
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
    let mine = |request| {
        resident::call(
            &context,
            &EnclaveRequest::MineEncryptedNodV1 {
                request: Box::new(request),
            },
        )
    };
    let first = mine(request.clone()).unwrap();
    assert_eq!(first, mine(request.clone()).unwrap());
    let EnclaveResponse::EncryptedNodMinedV1 { result } = first else {
        panic!("expected mined NOD")
    };
    assert_eq!(result.next_op_nonce, 1);
    assert!(!result.fidelity.new_blob.is_empty());
    assert_eq!(
        gratis::decrypt_balance(
            &gratis::derive_view_key(&gratis_key, account).unwrap(),
            account,
            &result.new_balance
        )
        .unwrap(),
        amount
    );
    request.modify_auth.mac[0] ^= 1;
    assert!(matches!(
        mine(request).unwrap(),
        EnclaveResponse::EncryptedNodMintRejectedV1 { .. }
    ));
}
