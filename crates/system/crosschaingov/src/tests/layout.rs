//! Storage layout: the CrosschainGov and the Hyperlane controller share the
//! account `0x...EE14`, so their slots must not overlap.

use alloy_primitives::U256;
use outbe_hyperlanecontroller::test_support::provider;
use outbe_hyperlanecontroller::HyperlaneControllerContract;
use outbe_primitives::storage::StorageHandle;

use crate::schema::{CrosschainGov, FIRST_SLOT};

#[test]
fn crosschaingov_slots_do_not_overlap_the_hyperlane_controller_slots() {
    let mut p = provider();
    StorageHandle::enter(&mut p, |storage| {
        let h = HyperlaneControllerContract::new(storage.clone());
        let hyperlane = [
            h.ica_router.slot(),
            h.ism_by_domain.base_slot(),
            h.domains.base_slot(),
            h.hook_by_domain.base_slot(),
            h.signer_of.base_slot(),
            h.submitted_index.base_slot(),
            h.submitted_block.base_slot(),
            h.miss_count.base_slot(),
            h.validator_announce.slot(),
            h.bootstrap_admin.slot(),
        ];
        let g = CrosschainGov::new(storage);
        let gov = [g.approved_payload.base_slot(), g.approved_at.base_slot()];

        assert_eq!(
            hyperlane.to_vec(),
            (0..10u64).map(U256::from).collect::<Vec<_>>()
        );
        assert_eq!(gov, [U256::from(FIRST_SLOT), U256::from(FIRST_SLOT + 1)]);
        assert!(hyperlane.iter().all(|slot| *slot < U256::from(FIRST_SLOT)));
        // The genesis CLI seeds the bootstrap admin at slot 9.
        assert_eq!(h.bootstrap_admin.slot(), U256::from(9));
    });
}
