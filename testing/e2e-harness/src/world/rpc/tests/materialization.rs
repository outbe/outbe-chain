use crate::world::rpc::*;

#[cfg(feature = "ocomp-integration")]
#[test]
fn materialization_progress_decodes_indexed_identity_and_nonindexed_progress() {
    let mut data = Vec::new();
    for value in [7_u64, 0, 8, 1, 42] {
        data.extend_from_slice(&U256::from(value).to_be_bytes::<32>());
    }
    let log = serde_json::json!({
        "topics": [
            format!("{:#x}", keccak256(b"NodMaterializationProgress(uint64,uint32,uint64,uint32,uint32,bool,uint64)")),
            format!("0x{:064x}", 3),
            format!("0x{:064x}", 20_260_813),
        ],
        "data": format!("0x{}", hex::encode(data)),
    });

    assert_eq!(
        decode_nod_materialization_progress(&log),
        Some(NodMaterializationProgressV1 {
            worldwide_day: 20_260_813,
            generation: 7,
            next_nod_ordinal: 8,
            completed: true,
            block_number: 42,
        })
    );
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn materialization_stall_deadline_resets_only_for_strict_progress() {
    let started = Instant::now();
    let stall = Duration::from_secs(10);
    let mut deadline = MaterializationStallDeadline::new(started, stall);

    assert!(!deadline.observe(started + Duration::from_secs(9), 0));
    assert!(!deadline.observe(started + Duration::from_secs(9), 8));
    assert!(!deadline.observe(started + Duration::from_secs(18), 8));
    assert!(deadline.observe(started + Duration::from_secs(19), 8));
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn materialization_stall_deadline_ignores_regressing_observations() {
    let started = Instant::now();
    let stall = Duration::from_secs(10);
    let mut deadline = MaterializationStallDeadline::new(started, stall);

    assert!(!deadline.observe(started + Duration::from_secs(5), 16));
    assert!(!deadline.observe(started + Duration::from_secs(9), 8));
    assert!(deadline.observe(started + Duration::from_secs(15), 16));
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn first_owner_index_revert_is_not_reported_as_an_absent_nod() {
    let error =
        classify_owner_index_result(0, Err("execution reverted: index out of bounds".to_owned()))
            .expect_err("index zero must preserve the execution failure");

    assert!(error.contains("index out of bounds"));
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn second_owner_index_transport_failure_does_not_prove_uniqueness() {
    let error = classify_owner_index_result(
        1,
        Err("compressed-entity tree unavailable: exact parent mismatch".to_owned()),
    )
    .expect_err("a readiness failure must not be treated as an absent second NOD");

    assert!(error.contains("exact parent mismatch"));
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn exact_second_owner_index_out_of_bounds_proves_uniqueness() {
    assert_eq!(
        classify_owner_index_result(1, Err("execution reverted: index out of bounds".to_owned()),)
            .expect("the canonical bounds error is an expected absence"),
        None,
    );
}
