use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{
    time::WorldwideDay,
    tribute_encryption::{TributeAmountsV2, TributeContextV2},
    wwd_entity_id::WwdEntityId,
};
use outbe_tee::tribute_day::{TributeDayOpRequestV2, TributeDayOperationV2};
use outbe_tee_enclave::{
    tribute_day::{apply_day_operation, read_day_amount},
    tribute_encryption::encrypt_tribute,
};

#[test]
fn encrypted_day_issue_burn_freeze_and_retirement_preserve_frozen_history() {
    let secret = [7; 32];
    let day = WorldwideDay::new(20250115);
    let context = TributeContextV2 {
        chain_id: 54322345,
        tribute_id: WwdEntityId::from_day_and_digest(day, [0x22; 32]),
        owner: Address::repeat_byte(0x33),
        worldwide_day: day,
        issuance_currency: 840,
        reference_currency: 978,
        tribute_price_minor: U256::from(2_000_000),
        exclude_from_intex_issuance: false,
        offer_input_hash: B256::repeat_byte(0x44),
    };
    let tribute = encrypt_tribute(
        &secret,
        &outbe_tee_enclave::crypto::x25519_public(&[11; 32]),
        context,
        &TributeAmountsV2 {
            issuance_amount_minor: U256::from(100_000_000),
            nominal_amount_minor: U256::from(50_000_000),
        },
    )
    .unwrap();
    let mut request = TributeDayOpRequestV2 {
        chain_id: 54322345,
        worldwide_day: day,
        previous: None,
        public_state_hash: B256::repeat_byte(0x55),
        operation: TributeDayOperationV2::Adjust {
            tribute: Box::new(tribute.clone()),
            add: true,
        },
    };
    let issued = apply_day_operation(&secret, &request).unwrap();
    assert_eq!(
        read_day_amount(&secret, &issued).unwrap(),
        U256::from(50_000_000)
    );
    assert_eq!(apply_day_operation(&secret, &request).unwrap(), issued);
    request.previous = Some(issued.clone());
    request.operation = TributeDayOperationV2::Adjust {
        tribute: Box::new(tribute),
        add: false,
    };
    let burned = apply_day_operation(&secret, &request).unwrap();
    assert_eq!(read_day_amount(&secret, &burned).unwrap(), U256::ZERO);
    assert!(apply_day_operation(
        &secret,
        &TributeDayOpRequestV2 {
            previous: Some(burned),
            ..request.clone()
        }
    )
    .is_err());
    request.previous = Some(issued.clone());
    request.operation = TributeDayOperationV2::Freeze;
    let frozen = apply_day_operation(&secret, &request).unwrap();
    assert_eq!(
        read_day_amount(&secret, &frozen).unwrap(),
        U256::from(50_000_000)
    );
    request.operation = TributeDayOperationV2::Reset {
        expected_total: U256::from(50_000_000),
    };
    let retired = apply_day_operation(&secret, &request).unwrap();
    assert_eq!(read_day_amount(&secret, &retired).unwrap(), U256::ZERO);
    assert_eq!(
        read_day_amount(&secret, &frozen).unwrap(),
        U256::from(50_000_000)
    );
    request.previous = Some(frozen);
    assert!(apply_day_operation(&secret, &request).is_err());
}

fn day_request(amount: U256) -> TributeDayOpRequestV2 {
    TributeDayOpRequestV2 {
        chain_id: 54322345,
        worldwide_day: WorldwideDay::new(20250115),
        previous: None,
        public_state_hash: B256::repeat_byte(0x55),
        operation: TributeDayOperationV2::AdjustTransient {
            nominal_amount_minor: amount,
            add: true,
        },
    }
}

#[test]
fn encrypted_day_rejects_wrong_key_and_modified_context_or_ciphertext() {
    let secret = [7; 32];
    let record = apply_day_operation(&secret, &day_request(U256::from(71))).unwrap();
    assert!(read_day_amount(&[8; 32], &record).is_err());
    for change in [
        (|record: &mut outbe_primitives::tribute_day_encryption::EncryptedTributeDayAmountV2| {
            record.chain_id += 1
        }) as fn(&mut _),
        |record| record.worldwide_day = WorldwideDay::new(20250116),
        |record| record.frozen = true,
        |record| record.operation_hash = B256::repeat_byte(9),
        |record| record.encrypted_amount[8] ^= 1,
        |record| record.encrypted_amount[7] = 0,
        |record| {
            record.encrypted_amount.pop();
        },
    ] {
        let mut corrupted = record.clone();
        change(&mut corrupted);
        assert!(read_day_amount(&secret, &corrupted).is_err());
    }
}

#[test]
fn exact_predecessor_and_public_state_change_the_deterministic_write_context() {
    let secret = [7; 32];
    let request = day_request(U256::from(71));
    let record = apply_day_operation(&secret, &request).unwrap();
    let branch = apply_day_operation(
        &secret,
        &TributeDayOpRequestV2 {
            public_state_hash: B256::repeat_byte(0x56),
            ..request.clone()
        },
    )
    .unwrap();
    assert_ne!(record.encrypted_amount, branch.encrypted_amount);
    assert_ne!(
        record.operation_hash,
        outbe_tee::tribute_day::day_operation_inputs_hash(&request).unwrap()
    );
    assert_eq!(read_day_amount(&secret, &branch).unwrap(), U256::from(71));
    let freeze = TributeDayOpRequestV2 {
        previous: Some(record.clone()),
        operation: TributeDayOperationV2::Freeze,
        ..request.clone()
    };
    let first = apply_day_operation(&secret, &freeze).unwrap();
    let second = apply_day_operation(
        &secret,
        &TributeDayOpRequestV2 {
            previous: Some(branch),
            ..freeze
        },
    )
    .unwrap();
    assert_ne!(first.encrypted_amount, second.encrypted_amount);
    assert_eq!(first.version(), Some(2));
    for change in [
        (|request: &mut TributeDayOpRequestV2| request.chain_id += 1) as fn(&mut _),
        |request| request.worldwide_day = WorldwideDay::new(20250116),
    ] {
        let mut wrong = TributeDayOpRequestV2 {
            previous: Some(record.clone()),
            ..request.clone()
        };
        change(&mut wrong);
        assert!(apply_day_operation(&secret, &wrong).is_err());
    }
}

#[test]
fn overflow_and_mismatched_retirement_fail_without_replacing_previous_state() {
    let secret = [7; 32];
    let request = day_request(U256::MAX);
    let previous = apply_day_operation(&secret, &request).unwrap();
    for operation in [
        TributeDayOperationV2::AdjustTransient {
            nominal_amount_minor: U256::from(1),
            add: true,
        },
        TributeDayOperationV2::Reset {
            expected_total: U256::ZERO,
        },
    ] {
        assert!(apply_day_operation(
            &secret,
            &TributeDayOpRequestV2 {
                previous: Some(previous.clone()),
                operation,
                ..request.clone()
            }
        )
        .is_err());
        assert_eq!(read_day_amount(&secret, &previous).unwrap(), U256::MAX);
    }
}
