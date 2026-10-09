use alloy_primitives::{Address, B256, U256};
use outbe_ocomp_protocol::test_utils::nod_action_population;
use outbe_ocomp_protocol::{
    abi::{
        encode_materialize_certified_nods_calldata,
        encode_protected_materialize_certified_nods_calldata, NOD_FACTORY_ADDRESS,
    },
    common::BoundedBytes,
    nod_materialization::{
        verify_nod_materialization_batch, NodMaterializationBatchV1, NodMaterializationHeadV1,
        ProtectedNodMaterializationV2,
    },
    profile::poc_schema_limits,
    result::NodActionV1,
    system_carrier::{
        classify_ocomp_system_carrier, OcompSystemCarrierCandidate, OcompSystemCarrierError,
        OcompSystemCarrierView, MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS,
        OCOMP_SYSTEM_CARRIER_GAS_LIMIT,
    },
    transaction_call::TransactionCallFields,
};

const WWD: u32 = 20_260_812;

fn protected_batch() -> ProtectedNodMaterializationV2 {
    ProtectedNodMaterializationV2 {
        queue_sequence: 1,
        first_nod_ordinal: 0,
        encryption_binding: B256::repeat_byte(0x31),
        encrypted_witness: BoundedBytes(vec![0x71; 64]),
        encrypted_nods: vec![BoundedBytes(vec![0x81; 96])],
    }
}

fn entity_id(seed: u32) -> B256 {
    let mut bytes = [0_u8; 32];
    bytes[..4].copy_from_slice(&WWD.to_be_bytes());
    bytes[28..].copy_from_slice(&seed.to_be_bytes());
    B256::from(bytes)
}

fn action(ordinal: u32) -> NodActionV1 {
    NodActionV1 {
        raw_ordinal: ordinal,
        tribute_id: entity_id(ordinal + 1),
        nod_id: entity_id(ordinal + 101),
        owner: Address::from_word(B256::from(U256::from(ordinal + 1))),
        wwd: WWD,
        league_id: 1,
        gratis_load_minor: U256::from(1_000),
        entry_price_minor: U256::from(510),
        settlement_cost_minor: U256::from(2),
        issuance_currency: 840,
        reference_currency: 840,
    }
}

fn population(count: u32) -> (Vec<NodActionV1>, B256, Vec<Vec<B256>>) {
    let actions = (0..count).map(action).collect::<Vec<_>>();
    let (root, proofs) = nod_action_population(&actions);
    (actions, root, proofs)
}

fn head(root: B256, count: u32, cursor: u32) -> NodMaterializationHeadV1 {
    NodMaterializationHeadV1 {
        queue_sequence: 1,
        job_id: B256::repeat_byte(0x11),
        program_semantics_hash: B256::repeat_byte(0x22),
        worldwide_day: WWD,
        generation: 1,
        nod_root: root,
        nod_count: count,
        next_nod_ordinal: cursor,
        last_progress_height: 90,
    }
}

struct BatchFixtureSpec {
    ordinals: std::ops::Range<u32>,
    subtree_height: usize,
}

impl BatchFixtureSpec {
    fn new(ordinals: std::ops::Range<u32>, subtree_height: usize) -> Self {
        Self {
            ordinals,
            subtree_height,
        }
    }
}

fn batch(
    actions: &[NodActionV1],
    proofs: &[Vec<B256>],
    spec: BatchFixtureSpec,
) -> NodMaterializationBatchV1 {
    let first = spec.ordinals.start as usize;
    let end = spec.ordinals.end as usize;
    NodMaterializationBatchV1 {
        queue_sequence: 1,
        first_nod_ordinal: spec.ordinals.start,
        actions: actions[first..end].to_vec(),
        root_path: proofs[first][spec.subtree_height..].to_vec(),
    }
}

#[test]
fn canonical_batch_and_head_roundtrip_without_redundant_authority_fields() {
    let limits = poc_schema_limits();
    let (actions, root, proofs) = population(10);
    let head = head(root, 10, 0);
    let batch = batch(&actions, &proofs, BatchFixtureSpec::new(0..8, 3));

    assert_eq!(
        NodMaterializationHeadV1::decode_canonical(
            &head.encode_canonical(&limits).unwrap(),
            &limits
        )
        .unwrap(),
        head
    );
    assert_eq!(
        NodMaterializationBatchV1::decode_canonical(
            &batch.encode_canonical(&limits).unwrap(),
            &limits
        )
        .unwrap(),
        batch
    );
}

#[test]
fn batch_codec_enforces_the_frozen_action_and_root_path_ceilings() {
    let limits = poc_schema_limits();
    let mut empty = NodMaterializationBatchV1 {
        queue_sequence: 1,
        first_nod_ordinal: 0,
        actions: Vec::new(),
        root_path: Vec::new(),
    };
    assert!(empty.encode_canonical(&limits).is_err());

    empty.actions = (0..=256).map(action).collect();
    assert!(empty.encode_canonical(&limits).is_err());

    empty.actions.truncate(1);
    empty.root_path = vec![B256::ZERO; 33];
    assert!(empty.encode_canonical(&limits).is_err());
}

#[test]
fn shared_root_path_verifies_a_full_batch_and_a_padded_final_remainder() {
    let limits = poc_schema_limits();
    let (actions, root, proofs) = population(10);

    let first = batch(&actions, &proofs, BatchFixtureSpec::new(0..8, 3));
    let verified = verify_nod_materialization_batch(&first, &head(root, 10, 0), 3, &limits)
        .expect("full batch");
    assert_eq!(verified.actions(), &actions[..8]);

    let final_batch = batch(&actions, &proofs, BatchFixtureSpec::new(8..10, 3));
    let verified = verify_nod_materialization_batch(&final_batch, &head(root, 10, 8), 3, &limits)
        .expect("final padded remainder");
    assert_eq!(verified.actions(), &actions[8..]);
}

#[test]
fn configured_height_is_a_ceiling_for_smaller_aligned_certified_subtrees() {
    let limits = poc_schema_limits();
    let (actions, root, proofs) = population(10);
    for (first, count, height) in [(0, 4, 2), (4, 2, 1), (6, 1, 0), (8, 2, 2)] {
        let smaller = batch(
            &actions,
            &proofs,
            BatchFixtureSpec::new(first..first + count as u32, height),
        );
        let verified =
            verify_nod_materialization_batch(&smaller, &head(root, 10, first), 3, &limits).unwrap();
        assert_eq!(
            verified.actions(),
            &actions[first as usize..first as usize + count]
        );
    }
    let (actions, root, proofs) = population(256);
    verify_nod_materialization_batch(
        &batch(&actions, &proofs, BatchFixtureSpec::new(0..256, 8)),
        &head(root, 256, 0),
        8,
        &limits,
    )
    .expect("the existing maximum remains accepted");
}

#[test]
fn smaller_subtrees_still_reject_excess_height_misalignment_count_and_path() {
    let limits = poc_schema_limits();
    let (actions, root, proofs) = population(17);
    for (candidate, cursor, maximum) in [
        (
            batch(&actions, &proofs, BatchFixtureSpec::new(0..8, 3)),
            0,
            2,
        ),
        (
            batch(&actions, &proofs, BatchFixtureSpec::new(2..6, 2)),
            2,
            3,
        ),
        (
            batch(&actions, &proofs, BatchFixtureSpec::new(0..3, 2)),
            0,
            3,
        ),
        (
            batch(&actions, &proofs, BatchFixtureSpec::new(0..5, 2)),
            0,
            3,
        ),
    ] {
        assert!(verify_nod_materialization_batch(
            &candidate,
            &head(root, 17, cursor),
            maximum,
            &limits,
        )
        .is_err());
    }
    let mut candidate = batch(&actions, &proofs, BatchFixtureSpec::new(0..1, 0));
    candidate.root_path.push(B256::ZERO);
    assert!(verify_nod_materialization_batch(&candidate, &head(root, 17, 0), 3, &limits).is_err());
    let mut candidate = batch(&actions, &proofs, BatchFixtureSpec::new(0..2, 1));
    candidate.root_path[0] = B256::ZERO;
    assert!(verify_nod_materialization_batch(&candidate, &head(root, 17, 0), 3, &limits).is_err());
}

#[test]
fn short_nonfinal_misaligned_unordered_and_bad_root_batches_are_rejected() {
    let limits = poc_schema_limits();
    let (actions, root, proofs) = population(17);

    let short = batch(&actions, &proofs, BatchFixtureSpec::new(0..7, 3));
    assert!(verify_nod_materialization_batch(&short, &head(root, 17, 0), 3, &limits).is_err());

    let misaligned = batch(&actions, &proofs, BatchFixtureSpec::new(8..16, 3));
    assert!(
        verify_nod_materialization_batch(&misaligned, &head(root, 17, 7), 3, &limits,).is_err()
    );

    let mut unordered = batch(&actions, &proofs, BatchFixtureSpec::new(0..8, 3));
    unordered.actions.swap(0, 1);
    assert!(verify_nod_materialization_batch(&unordered, &head(root, 17, 0), 3, &limits,).is_err());

    let mut bad_root_path = batch(&actions, &proofs, BatchFixtureSpec::new(0..8, 3));
    bad_root_path.root_path[0] = B256::repeat_byte(0xff);
    assert!(
        verify_nod_materialization_batch(&bad_root_path, &head(root, 17, 0), 3, &limits,).is_err()
    );
}

#[test]
fn materialization_uses_the_existing_strict_ocomp_system_carrier_lane() {
    let limits = poc_schema_limits();
    let input =
        encode_protected_materialize_certified_nods_calldata(&protected_batch(), &limits).unwrap();
    let candidate = classify_ocomp_system_carrier(
        OcompSystemCarrierView {
            is_eip1559: true,
            call: TransactionCallFields {
                to: Some(NOD_FACTORY_ADDRESS),
                value: U256::ZERO,
                input: &input,
                gas_limit: OCOMP_SYSTEM_CARRIER_GAS_LIMIT,
                max_fee_per_gas: MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS,
                max_priority_fee_per_gas: Some(0),
            },
        },
        &limits,
    )
    .unwrap()
    .expect("materialization carrier");

    assert!(matches!(
        candidate,
        OcompSystemCarrierCandidate::NodMaterialization {
            queue_sequence: 1,
            first_nod_ordinal: 0
        }
    ));
}

#[test]
fn materialization_carrier_rejects_every_noncanonical_envelope_field() {
    let limits = poc_schema_limits();
    let input =
        encode_protected_materialize_certified_nods_calldata(&protected_batch(), &limits).unwrap();
    let canonical = OcompSystemCarrierView {
        is_eip1559: true,
        call: TransactionCallFields {
            to: Some(NOD_FACTORY_ADDRESS),
            value: U256::ZERO,
            input: &input,
            gas_limit: OCOMP_SYSTEM_CARRIER_GAS_LIMIT,
            max_fee_per_gas: MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS,
            max_priority_fee_per_gas: Some(0),
        },
    };

    assert!(classify_ocomp_system_carrier(
        OcompSystemCarrierView {
            call: TransactionCallFields {
                to: Some(Address::repeat_byte(0xaa)),
                ..canonical.call
            },
            ..canonical
        },
        &limits,
    )
    .unwrap()
    .is_none());
    assert!(classify_ocomp_system_carrier(
        OcompSystemCarrierView {
            is_eip1559: false,
            ..canonical
        },
        &limits,
    )
    .is_err_and(|error| matches!(error, OcompSystemCarrierError::NotEip1559)));
    assert!(classify_ocomp_system_carrier(
        OcompSystemCarrierView {
            call: TransactionCallFields {
                value: U256::from(1),
                ..canonical.call
            },
            ..canonical
        },
        &limits,
    )
    .is_err_and(|error| matches!(error, OcompSystemCarrierError::NonZeroValue)));
    assert!(classify_ocomp_system_carrier(
        OcompSystemCarrierView {
            call: TransactionCallFields {
                gas_limit: OCOMP_SYSTEM_CARRIER_GAS_LIMIT + 1,
                ..canonical.call
            },
            ..canonical
        },
        &limits,
    )
    .is_err_and(|error| matches!(error, OcompSystemCarrierError::WrongGasLimit { .. })));
    assert!(classify_ocomp_system_carrier(
        OcompSystemCarrierView {
            call: TransactionCallFields {
                max_fee_per_gas: MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS - 1,
                ..canonical.call
            },
            ..canonical
        },
        &limits,
    )
    .is_err_and(|error| matches!(error, OcompSystemCarrierError::FeeCapTooLow { .. })));
    assert!(classify_ocomp_system_carrier(
        OcompSystemCarrierView {
            call: TransactionCallFields {
                max_priority_fee_per_gas: Some(1),
                ..canonical.call
            },
            ..canonical
        },
        &limits,
    )
    .is_err_and(|error| matches!(error, OcompSystemCarrierError::NonZeroPriorityFee)));

    let malformed = &input[..4];
    assert!(classify_ocomp_system_carrier(
        OcompSystemCarrierView {
            call: TransactionCallFields {
                input: malformed,
                ..canonical.call
            },
            ..canonical
        },
        &limits,
    )
    .is_err_and(|error| matches!(error, OcompSystemCarrierError::MalformedMaterialization(_))));
}

#[test]
fn plaintext_calculation_batches_are_not_public_materialization_carriers() {
    let limits = poc_schema_limits();
    let (actions, _root, proofs) = population(8);
    let input = encode_materialize_certified_nods_calldata(
        &batch(&actions, &proofs, BatchFixtureSpec::new(0..8, 3)),
        &limits,
    )
    .unwrap();
    assert!(classify_ocomp_system_carrier(
        OcompSystemCarrierView {
            is_eip1559: true,
            call: TransactionCallFields {
                to: Some(NOD_FACTORY_ADDRESS),
                value: U256::ZERO,
                input: &input,
                gas_limit: OCOMP_SYSTEM_CARRIER_GAS_LIMIT,
                max_fee_per_gas: MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS,
                max_priority_fee_per_gas: Some(0),
            },
        },
        &limits
    )
    .is_err_and(|error| matches!(error, OcompSystemCarrierError::MalformedMaterialization(_))));
}
