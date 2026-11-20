use alloy_primitives::U256;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::config::{CredisParams, PROFILE_AUTO, PROFILE_E2E, PROFILE_MAINNET, PROFILE_TESTNET};
use crate::schema::CredisContract;

const CHAIN_ID: u64 = 1;

#[test]
fn the_profile_selector_resolves_by_network_and_rejects_unknown_values() {
    let mainnet = outbe_primitives::chain::MAINNET_CHAIN_ID;
    assert_eq!(
        CredisParams::from_selector(PROFILE_AUTO, CHAIN_ID).unwrap(),
        CredisParams::TESTNET
    );
    assert_eq!(
        CredisParams::from_selector(PROFILE_AUTO, mainnet).unwrap(),
        CredisParams::MAINNET
    );
    assert_eq!(
        CredisParams::from_selector(PROFILE_TESTNET, mainnet).unwrap(),
        CredisParams::TESTNET
    );
    assert_eq!(
        CredisParams::from_selector(PROFILE_MAINNET, CHAIN_ID).unwrap(),
        CredisParams::MAINNET
    );
    assert_eq!(
        CredisParams::from_selector(PROFILE_E2E, CHAIN_ID).unwrap(),
        CredisParams::E2E
    );
    assert!(CredisParams::from_selector(4, CHAIN_ID).is_err());
}

/// Pin the selector slot index: the genesis seeder writes a raw slot.
#[test]
fn config_profile_slot_matches_seeder_layout() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            CredisContract::new(storage).config_profile.slot(),
            U256::from(39)
        );
    });
}
