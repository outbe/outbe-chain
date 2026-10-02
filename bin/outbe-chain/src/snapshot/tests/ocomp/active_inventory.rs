use super::{queued_owner, with_owner_storage, CanonicalInventory};
use alloy_primitives::U256;
use outbe_primitives::{
    addresses::METADOSIS_ADDRESS,
    storage::{types::StorageBytes, StorageHandle},
};

#[test]
fn empty_native_aggregate_has_no_active_intents_without_local_job_files() {
    for version in [1, 2] {
        with_owner_storage(version, queued_owner(0), |state, source| {
            let scratch = tempfile::tempdir().unwrap();
            for maximum in [None, Some(0)] {
                let inventory =
                    CanonicalInventory::scan(state, scratch.path(), &source.protected, maximum)
                        .unwrap();
                assert!(inventory.active_jobs().is_empty());
                assert_eq!(inventory.bounds.active_intents, 0);
                drop(inventory);
                assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
            }
            // with_owner_storage also compares the complete source fingerprint.
        });
    }
}

#[test]
fn malformed_native_scheduler_propagates_through_inventory_without_source_writes() {
    // Current Metadosis schema places the one-slot scheduler StorageBytes
    // immediately before the fixed OCOMP job-records mapping base slot 20.
    // Raw corruption is test setup only; the inventory calls the owner view.
    let scheduler_slot = U256::from(19);
    for version in [1, 2] {
        for oversized in [false, true] {
            let mut owner = queued_owner(0);
            if oversized {
                // Solidity long-bytes marker: 2 * length + 1. This length
                // exceeds the native u16 live-index capacity before payload reads.
                owner.storage.insert(
                    (METADOSIS_ADDRESS, scheduler_slot),
                    U256::from(40_000_001_u64),
                );
            } else {
                StorageHandle::enter(&mut owner, |storage| {
                    StorageBytes::new(scheduler_slot, METADOSIS_ADDRESS, storage)
                        .write(&[0; 8])
                        .unwrap();
                });
            }
            with_owner_storage(version, owner, |state, source| {
                let scratch = tempfile::tempdir().unwrap();
                let error =
                    CanonicalInventory::scan(state, scratch.path(), &source.protected, None)
                        .err()
                        .expect("malformed live index cannot become an empty inventory");
                let expected = if oversized {
                    "OCOMP live scheduler exceeds native byte cap"
                } else {
                    "OCOMP live index magic/version mismatch"
                };
                assert!(error.to_string().contains(expected), "{error:#}");
                assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
            });
        }
    }
}
