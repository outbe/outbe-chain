use super::*;
use crate::snapshot::validation::ocomp::verify_canonical_obligations;

#[test]
fn empty_canonical_composition_needs_no_job_cas_or_payout_directories() {
    for version in [1, 2] {
        with_canonical_frontiers(
            version,
            |_| {},
            |_| queued_owner(0),
            |state, source, layout, scratch| {
                for maximum in [None, Some(0)] {
                    let audit =
                        verify_canonical_obligations(state, source, layout, scratch, maximum, None)
                            .unwrap();
                    assert_eq!(audit.projection.block_number, 100);
                    assert_eq!(audit.closure.checkpoint.current.block_number, 100);
                    assert_eq!(audit.closure.replay.blocks, 0);
                    assert_eq!(audit.bounds.active_intents, 0);
                    assert_eq!(audit.bounds.nod_entries, 0);
                    assert_eq!(audit.bounds.unpaid_days, 0);
                    assert!(audit.active.is_empty());
                    assert!(audit.pins.is_empty());
                    assert_eq!(audit.source_leases, 0);
                    assert_eq!(audit.complete_exports, 0);
                    assert_eq!(audit.input_chunks, 0);
                    assert_eq!(audit.nod.jobs, 0);
                    assert_eq!(audit.payout_days, 0);
                }
                for absent in [
                    "cas-v1",
                    "supervisor-v1/jobs",
                    "node-v1/local-results",
                    "supervisor-v1/materialization-references",
                ] {
                    assert!(!layout.ocomp_root.join(absent).exists());
                }
                assert!(!layout.consensus_root.join("ocomp_retention").exists());
            },
        );
    }
}

#[test]
fn canonical_inventory_failure_precedes_missing_projection_and_local_population() {
    with_canonical_frontiers(
        2,
        |layout| {
            std::fs::remove_dir_all(&layout.projection.as_ref().unwrap().root).unwrap();
        },
        |_| {
            let mut owner = queued_owner(0);
            // Exact corruption already exercised by active_inventory.
            StorageHandle::enter(&mut owner, |storage| {
                outbe_primitives::storage::types::StorageBytes::new(
                    U256::from(18),
                    outbe_primitives::addresses::METADOSIS_ADDRESS,
                    storage,
                )
                .write(&[0; 8])
                .unwrap();
            });
            owner
        },
        |state, source, layout, scratch| {
            let error = verify_canonical_obligations(state, source, layout, scratch, None, None)
                .err()
                .expect("invalid canonical inventory cannot be skipped");
            assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
            assert!(
                error
                    .to_string()
                    .contains("OCOMP live index magic/version mismatch"),
                "{error:#}"
            );
        },
    );
}
