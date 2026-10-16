use super::{
    boundary::{signing_counts, signing_heights},
    rejection::{verify_deactivation_rejection, DeactivationRejection, RejectionEvidence},
    *,
};
use alloy_sol_types::SolCall;
use outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K;

#[derive(Clone)]
struct RejectionFixture {
    outcome: TxOutcome,
    transaction: Value,
    block: Value,
    before: Account,
    caller: Address,
}

impl RejectionFixture {
    fn new(rejection: DeactivationRejection) -> Self {
        let mut before = fixture();
        before.status = rejection.status();
        before.participant = true;
        before.has_share = true;
        before.stake = U256::from(100);
        before.mirrored_stake = before.stake;
        let caller = match rejection {
            DeactivationRejection::Unauthorized => Address::repeat_byte(9),
            DeactivationRejection::Repeated => before.address,
        };
        let hash = B256::repeat_byte(1);
        let block_hash = B256::repeat_byte(2);
        let input = Bytes::from(
            eth::IValidatorSet::deactivateValidatorCall {
                validatorAddress: before.address,
            }
            .abi_encode(),
        );
        let outcome = TxOutcome {
            transaction_hash: format!("{hash:#x}"),
            success: false,
            receipt: json!({"transactionHash": hash, "status": "0x0",
                "blockNumber": "0x2", "blockHash": block_hash,
                "transactionIndex": "0x1", "logs": []}),
        };
        let transaction = json!({"hash": hash, "blockHash": block_hash,
            "blockNumber": "0x2", "transactionIndex": "0x1",
            "from": caller, "to": addresses::VS_ADDR, "input": input, "value": "0x0"});
        let block = json!({"hash": block_hash, "number": "0x2",
            "parentHash": B256::repeat_byte(3),
            "transactions": [B256::repeat_byte(4), hash]});
        Self {
            outcome,
            transaction,
            block,
            before,
            caller,
        }
    }

    fn verify(&self, rejection: DeactivationRejection) -> Result<()> {
        verify_deactivation_rejection(
            &RejectionEvidence {
                outcome: &self.outcome,
                transaction: &self.transaction,
                block: &self.block,
            },
            &self.before,
            self.caller,
            rejection,
        )
    }
}

#[test]
fn lifecycle_canonical_rejection_accepts_both_phases_and_nonzero_index() {
    for rejection in [
        DeactivationRejection::Unauthorized,
        DeactivationRejection::Repeated,
    ] {
        assert!(RejectionFixture::new(rejection).verify(rejection).is_ok());
    }
}

#[test]
fn lifecycle_canonical_rejection_requires_failed_receipt_without_logs() {
    let rejection = DeactivationRejection::Unauthorized;
    for status in [Value::Null, json!("0x1"), json!("invalid")] {
        let mut proof = RejectionFixture::new(rejection);
        proof.outcome.receipt["status"] = status;
        assert!(proof.verify(rejection).is_err());
    }
    let mut proof = RejectionFixture::new(rejection);
    proof.outcome.success = true;
    assert!(proof.verify(rejection).is_err());
    let mut proof = RejectionFixture::new(rejection);
    proof.outcome.receipt["logs"] = json!([{"address": addresses::VS_ADDR}]);
    assert!(proof.verify(rejection).is_err());
}

#[test]
fn lifecycle_rejection_binds_canonical_transaction_and_receipt_position() {
    let rejection = DeactivationRejection::Unauthorized;
    for field in ["hash", "blockHash", "blockNumber", "transactionIndex"] {
        let mut proof = RejectionFixture::new(rejection);
        proof.transaction[field] = match field {
            "hash" | "blockHash" => json!(B256::repeat_byte(8)),
            _ => json!("0x0"),
        };
        assert!(proof.verify(rejection).is_err());
    }
    for field in ["hash", "number", "transactions"] {
        let mut proof = RejectionFixture::new(rejection);
        proof.block[field] = match field {
            "hash" => json!(B256::repeat_byte(8)),
            "number" => json!("0x3"),
            _ => json!([B256::repeat_byte(1), B256::repeat_byte(4)]),
        };
        assert!(proof.verify(rejection).is_err());
    }
    for index in [Value::Null, json!("0x0"), json!("0x2"), json!(U256::MAX)] {
        let mut proof = RejectionFixture::new(rejection);
        proof.outcome.receipt["transactionIndex"] = index;
        assert!(proof.verify(rejection).is_err());
    }
}

#[test]
fn lifecycle_rejection_binds_sender_target_calldata_and_zero_value() {
    let rejection = DeactivationRejection::Repeated;
    for field in ["from", "to", "input", "value"] {
        let mut proof = RejectionFixture::new(rejection);
        proof.transaction[field] = match field {
            "from" | "to" => json!(Address::repeat_byte(8)),
            "input" => json!(Bytes::from(
                eth::IValidatorSet::deactivateValidatorCall {
                    validatorAddress: Address::repeat_byte(8),
                }
                .abi_encode()
            )),
            _ => json!("0x1"),
        };
        assert!(proof.verify(rejection).is_err());
    }
}

#[test]
fn lifecycle_rejection_requires_the_expected_bonded_phase_and_actor() {
    for rejection in [
        DeactivationRejection::Unauthorized,
        DeactivationRejection::Repeated,
    ] {
        for status in 0..=5 {
            let mut proof = RejectionFixture::new(rejection);
            proof.before.status = status;
            assert_eq!(
                proof.verify(rejection).is_ok(),
                status == rejection.status()
            );
        }
        for missing in 0..3 {
            let mut proof = RejectionFixture::new(rejection);
            match missing {
                0 => proof.before.participant = false,
                1 => proof.before.has_share = false,
                _ => proof.before.stake = U256::ZERO,
            }
            assert!(proof.verify(rejection).is_err());
        }
        let mut proof = RejectionFixture::new(rejection);
        proof.caller = match rejection {
            DeactivationRejection::Unauthorized => proof.before.address,
            DeactivationRejection::Repeated => Address::repeat_byte(9),
        };
        proof.transaction["from"] = json!(proof.caller);
        assert!(proof.verify(rejection).is_err());
    }
}

#[test]
fn lifecycle_rejection_missing_rpc_evidence_fails_closed() {
    let rejection = DeactivationRejection::Unauthorized;
    for missing in 0..3 {
        let mut proof = RejectionFixture::new(rejection);
        match missing {
            0 => proof.transaction = Value::Null,
            1 => proof.block = Value::Null,
            _ => proof.outcome.receipt = Value::Null,
        }
        assert!(proof.verify(rejection).is_err());
    }
}

fn fixture() -> Account {
    Account {
        address: Address::repeat_byte(1),
        consensus_public_key: Bytes::from(vec![2; 48]),
        status: 4,
        has_share: false,
        participant: false,
        stake: U256::ZERO,
        mirrored_stake: U256::ZERO,
        total_staked: U256::from(400),
        staking_balance: U256::from(500),
        balance: U256::from(100),
        unbonding_end: 8,
        voter_misses: 0,
        p2p_version: 1,
        p2p_encoded: Bytes::new(),
    }
}

fn observation() -> Observation {
    Observation {
        supply: U256::from(3),
        active: vec![],
        consensus: vec![],
        epoch: U256::ONE,
        account: Some(fixture()),
    }
}

#[test]
fn lifecycle_requires_each_expected_peer_and_exact_state() {
    let expected = [10, 11, 12, 13, 14];
    let rows = expected.map(|port| (port, observation()));
    assert!(unanimous(&expected, &rows).is_ok());
    assert!(unanimous(&expected, &rows[..4]).is_err());
    let mut wrong = rows.clone();
    wrong[4].0 = 13;
    assert!(unanimous(&expected, &wrong).is_err());
    let mut wrong = rows;
    wrong[4].1.supply = U256::from(2);
    assert!(unanimous(&expected, &wrong).is_err());
    assert!(unanimous(&[], &[]).is_err());
}

#[test]
fn lifecycle_committee_is_identity_not_count() {
    let expected = [Address::repeat_byte(1), Address::repeat_byte(2)];
    assert!(exact_members(&[expected[1], expected[0]], &expected).is_ok());
    assert!(exact_members(&[expected[0], expected[0]], &expected).is_err());
    assert!(exact_members(&[expected[0], Address::repeat_byte(3)], &expected).is_err());
}

#[test]
fn lifecycle_signing_closes_the_entire_delayed_window() {
    let (closed, end) = signing_heights(100).unwrap();
    assert_eq!(closed, 100 + LATE_FINALIZE_WINDOW_K);
    assert_eq!(end, closed + 5 + LATE_FINALIZE_WINDOW_K);
    assert!(signing_heights(u64::MAX).is_err());
    assert!(signing_counts(0, 1, 1).is_ok());
    assert!(signing_counts(5, 5, 5).is_ok());
    assert!(signing_counts(5, 4, 4).is_err());
    assert!(signing_counts(0, 2, 2).is_err());
    assert!(signing_counts(0, 1, 2).is_err());
}

#[test]
fn lifecycle_claim_conserves_exact_value_and_fee() {
    let before = fixture();
    let mut after = before.clone();
    after.status = 5;
    after.staking_balance = U256::from(400);
    after.balance = U256::from(197);
    assert!(claim_accounting(&before, &after, U256::from(100), U256::from(3), 5).is_ok());
    for field in 0..7 {
        let mut wrong = after.clone();
        match field {
            0 => wrong.balance += U256::ONE,
            1 => wrong.staking_balance -= U256::ONE,
            2 => wrong.total_staked -= U256::ONE,
            3 => wrong.status = 4,
            4 => wrong.address = Address::repeat_byte(3),
            5 => wrong.p2p_version = 2,
            _ => wrong.consensus_public_key = Bytes::from(vec![3; 48]),
        }
        assert!(claim_accounting(&before, &wrong, U256::from(100), U256::from(3), 5).is_err());
    }
    assert!(claim_accounting(&before, &after, U256::from(501), U256::ZERO, 5).is_err());
}

#[test]
fn lifecycle_repeat_and_unrelated_claim_cannot_pay_again() {
    let before = fixture();
    assert!(claim_accounting(&before, &before, U256::ZERO, U256::ZERO, 4).is_ok());
    let mut repeated = before.clone();
    repeated.balance -= U256::from(3);
    assert!(claim_accounting(&before, &repeated, U256::ZERO, U256::from(3), 4).is_ok());
    repeated.balance += U256::from(100);
    repeated.staking_balance -= U256::from(100);
    assert!(claim_accounting(&before, &repeated, U256::ZERO, U256::from(3), 4).is_err());
}

#[test]
fn lifecycle_old_demotion_marker_is_not_current_phase_evidence() {
    let old = format!("{DEMOTION}\n");
    assert!(!demotion_seen(&old, old.len()).unwrap());
    assert!(demotion_seen(&format!("{old}{DEMOTION}\n"), old.len()).unwrap());
    assert!(demotion_seen(&old, old.len() + 1).is_err());
}

#[test]
fn lifecycle_rejection_requires_a_real_matching_reverted_receipt() {
    let hash = B256::repeat_byte(1);
    let outcome = TxOutcome {
        transaction_hash: format!("{hash:#x}"),
        success: false,
        receipt: json!({"transactionHash": hash, "status": "0x0", "blockNumber": "0x2",
            "blockHash": B256::repeat_byte(2), "logs": []}),
    };
    assert!(receipt_identity(&outcome, false).is_ok());
    assert!(receipt_identity(&outcome, true).is_err());
    let mut malformed = outcome.clone();
    malformed.receipt["status"] = Value::Null;
    assert!(receipt_identity(&malformed, false).is_err());
    let mut substituted = outcome.clone();
    substituted.receipt["transactionHash"] = json!(B256::repeat_byte(3));
    assert!(receipt_identity(&substituted, false).is_err());
    let mut partial = outcome;
    partial.receipt["logs"] = json!([{}]);
    assert!(receipt_identity(&partial, false).is_err());
}
