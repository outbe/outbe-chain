//! Storage layout and the profile selector.

use super::*;

/// Pin the selector slot index: the genesis seeder writes a raw slot.
#[test]
fn config_profile_slot_matches_seeder_layout() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            NodContract::new(storage).config_profile.slot(),
            U256::from(27)
        );
    });
}

#[test]
fn the_profile_selector_resolves_like_gem_and_intex() {
    use crate::config::{NodParams, PROFILE_AUTO, PROFILE_DEV, PROFILE_PROD};
    use outbe_primitives::chain::MAINNET_CHAIN_ID;
    assert_eq!(
        NodParams::from_selector(PROFILE_AUTO, MAINNET_CHAIN_ID).unwrap(),
        NodParams::PROD
    );
    assert_eq!(
        NodParams::from_selector(PROFILE_AUTO, 1).unwrap(),
        NodParams::DEV
    );
    assert_eq!(
        NodParams::from_selector(PROFILE_DEV, MAINNET_CHAIN_ID).unwrap(),
        NodParams::DEV
    );
    assert_eq!(
        NodParams::from_selector(PROFILE_PROD, 1).unwrap(),
        NodParams::PROD
    );
    assert!(NodParams::from_selector(3, 1).is_err());
}

/// Dense `order`-packing puts these fields at contiguous offsets 0..=8.
#[test]
fn nod_contract_slot_layout_is_pinned() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let nod = NodContract::new(storage);
        for (index, actual) in [nod.total_supply.slot(), nod.group_worldwide_day.base_slot()]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                actual,
                U256::from(index),
                "NodContract field #{index} moved slot"
            );
        }
    });
}
