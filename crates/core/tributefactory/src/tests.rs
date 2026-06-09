use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::runtime::validate_agent_reward_addresses;
use crate::schema::TributeFactoryContract;

const CHAIN_ID: u64 = 1;

#[test]
fn test_validate_agent_reward_both_empty() {
    assert!(validate_agent_reward_addresses(&[], &[]).is_ok());
}

#[test]
fn test_validate_agent_reward_both_present() {
    let wallets = vec!["0x1111111111111111111111111111111111111111".to_string()];
    let sfas = vec!["0x2222222222222222222222222222222222222222".to_string()];
    assert!(validate_agent_reward_addresses(&wallets, &sfas).is_ok());
}

#[test]
fn test_validate_agent_reward_wallets_only() {
    let wallets = vec!["0x1111111111111111111111111111111111111111".to_string()];
    assert!(validate_agent_reward_addresses(&wallets, &[]).is_err());
}

#[test]
fn test_validate_agent_reward_sfa_only() {
    let sfas = vec!["0x2222222222222222222222222222222222222222".to_string()];
    assert!(validate_agent_reward_addresses(&[], &sfas).is_err());
}

#[test]
fn test_validate_agent_reward_invalid_address() {
    let wallets = vec!["not_a_valid_address".to_string()];
    let sfas = vec!["0x2222222222222222222222222222222222222222".to_string()];
    assert!(validate_agent_reward_addresses(&wallets, &sfas).is_err());
}

#[test]
fn test_storage_dsl_layout_is_compatible_with_previous_slots() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let factory = TributeFactoryContract::new(storage.clone());
        assert_eq!(
            factory.used_su_hashes.base_slot(),
            alloy_primitives::U256::ZERO
        );
    });
}
