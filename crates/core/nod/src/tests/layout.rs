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
    use crate::config::{NodParams, PROFILE_AUTO, PROFILE_E2E, PROFILE_MAINNET, PROFILE_TESTNET};
    use outbe_primitives::chain::MAINNET_CHAIN_ID;
    assert_eq!(
        NodParams::from_selector(PROFILE_AUTO, MAINNET_CHAIN_ID).unwrap(),
        NodParams::MAINNET
    );
    assert_eq!(
        NodParams::from_selector(PROFILE_AUTO, 1).unwrap(),
        NodParams::TESTNET
    );
    assert_eq!(
        NodParams::from_selector(PROFILE_TESTNET, MAINNET_CHAIN_ID).unwrap(),
        NodParams::TESTNET
    );
    assert_eq!(
        NodParams::from_selector(PROFILE_MAINNET, 1).unwrap(),
        NodParams::MAINNET
    );
    assert_eq!(
        NodParams::from_selector(PROFILE_E2E, 1).unwrap(),
        NodParams::E2E
    );
    assert!(NodParams::from_selector(4, 1).is_err());
}

#[test]
fn nod_contract_slot_layout_is_pinned() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let nod = NodContract::new(storage);
        assert_eq!(nod.group_nod_count.base_slot(), U256::ZERO);
    });
}
