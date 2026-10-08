use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{
    nod_encryption::NodTermsV2, time::WorldwideDay, wwd_entity_id::WwdEntityId,
};
use outbe_tee::{
    nod_mine::MineEncryptedNodRequestV2,
    protocol::{
        FidelityCohortOp, FidelityOpSection, GratisOp, GratisOpRequest, GratisOpStatus, ModifyAuth,
    },
};
use outbe_tee_enclave::{gratis, nod_encryption::encrypt_nod, nod_mine};
use x25519_dalek::{PublicKey, StaticSecret};

#[test]
fn private_mine_replays_exactly_and_preserves_other_gratis_writers() {
    let network = [7; 32];
    let key = [9; 32];
    let fidelity_key = [13; 32];
    let account = Address::repeat_byte(3);
    let amount = U256::from(123);
    let creator = PublicKey::from(&StaticSecret::from([11; 32])).to_bytes();
    let day = WorldwideDay::new(20250115);
    let terms = NodTermsV2 {
        chain_id: 1,
        nod_id: WwdEntityId::from_day_and_digest(day, B256::repeat_byte(4)),
        owner: account,
        worldwide_day: day,
        league_id: 0,
        entry_price_minor: U256::ONE,
        issuance_currency: 840,
        reference_currency: 978,
    };
    let nod = encrypt_nod(&network, &creator, terms, amount).unwrap();
    let modify_key = gratis::derive_modify_key(&key, account).unwrap();
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
    let request = MineEncryptedNodRequestV2 {
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
    let standalone = outbe_tee_enclave::fidelity::apply_cohort_op(
        &fidelity_key,
        &outbe_tee::protocol::FidelityCohortRequest {
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
    let view_key = gratis::derive_view_key(&key, account).unwrap();
    assert_eq!(
        outbe_tee::gratis_decrypt::decrypt_gratis_balance(&view_key, account, &first.new_balance)
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
        gratis::decrypt_balance(&view_key, account, &after.new_balance).unwrap(),
        U256::from(100)
    );
    let mut invalid_fidelity = request;
    invalid_fidelity.fidelity.current_blob = vec![1, 2, 3];
    assert!(nod_mine::apply(&network, &key, &fidelity_key, &invalid_fidelity).is_err());
}
