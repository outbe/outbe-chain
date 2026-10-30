use alloy_primitives::U256;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::config::{CredisParams, PROFILE_AUTO, PROFILE_DEV, PROFILE_PROD};
use crate::schema::CredisContract;

const CHAIN_ID: u64 = 1;

#[test]
fn the_profile_selector_resolves_by_network_and_rejects_unknown_values() {
    let mainnet = outbe_primitives::chain::MAINNET_CHAIN_ID;
    assert_eq!(
        CredisParams::from_selector(PROFILE_AUTO, CHAIN_ID).unwrap(),
        CredisParams::DEV
    );
    assert_eq!(
        CredisParams::from_selector(PROFILE_AUTO, mainnet).unwrap(),
        CredisParams::PROD
    );
    assert_eq!(
        CredisParams::from_selector(PROFILE_DEV, mainnet).unwrap(),
        CredisParams::DEV
    );
    assert_eq!(
        CredisParams::from_selector(PROFILE_PROD, CHAIN_ID).unwrap(),
        CredisParams::PROD
    );
    assert!(CredisParams::from_selector(3, CHAIN_ID).is_err());
}

/// PROD keeps 1.64 times the anchor. DEV uses 1.02 times the anchor.
#[test]
fn the_call_rate_is_pinned_per_profile() {
    assert_eq!(
        CredisParams::PROD.call_rate,
        crate::constants::CALL_RATE_PCT
    );
    assert_eq!(CredisParams::PROD.call_rate, 64);
    assert_eq!(CredisParams::DEV.call_rate, 2);
}

/// Pin the selector slot index: the genesis seeder writes a raw slot.
#[test]
fn config_profile_slot_matches_seeder_layout() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            CredisContract::new(storage).config_profile.slot(),
            U256::from(38)
        );
    });
}
