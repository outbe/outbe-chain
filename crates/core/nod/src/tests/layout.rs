//! Storage layout and the profile selector.

use super::*;

/// Pin the selector slot index: the genesis seeder writes a raw slot.
#[test]
fn config_profile_slot_matches_seeder_layout() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            NodContract::new(storage).config_profile.slot(),
            U256::from(24)
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

/// PROD keeps 3.56 times the entry price. DEV uses 1.08 times the entry price.
#[test]
fn the_call_rate_is_pinned_per_profile() {
    use crate::config::NodParams;
    assert_eq!(NodParams::PROD.call_rate, crate::constants::CALL_RATE_PCT);
    assert_eq!(NodParams::PROD.call_rate, 256);
    assert_eq!(NodParams::DEV.call_rate, 8);
}

#[test]
fn nod_contract_slot_layout_is_pinned() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let nod = NodContract::new(storage);
        assert_eq!(nod.group_nod_count.base_slot(), U256::ZERO);
    });
}
