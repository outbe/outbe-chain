use super::*;
use outbe_primitives::{
    addresses::RADICLE_REGISTRY_ADDRESS,
    radicle_genesis::{GovernmentsDescriptor, GENESIS_GOVERNMENTS_FIELD},
};

fn seeded() -> (HashMapStorageProvider, serde_json::Value) {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/governments-genesis-storage.json"
    ))
    .unwrap();
    let mut provider = HashMapStorageProvider::new(2026);
    for (key, value) in fixture["storage"].as_object().unwrap() {
        provider.storage.insert(
            (RADICLE_REGISTRY_ADDRESS, key.parse().unwrap()),
            value.as_str().unwrap().parse().unwrap(),
        );
    }
    (provider, fixture)
}

#[test]
fn python_genesis_allocation_decodes_through_typed_registry() {
    let (mut provider, fixture) = seeded();
    StorageHandle::enter(&mut provider, |storage| {
        let registry = RadicleRegistry::new(storage);
        let descriptor = registry.governments_descriptor().unwrap().unwrap();
        assert_eq!(descriptor.description, "Network governance");
        assert_eq!(descriptor.chain_id, 2026);
        assert_eq!(descriptor.genesis_timestamp, 1700000000);
        assert_eq!(registry.config_max_repositories.read().unwrap(), 8);
        let from_config = GovernmentsDescriptor::from_genesis_field(
            Some(&fixture["config"][GENESIS_GOVERNMENTS_FIELD]),
            2026,
            1700000000,
            registry.config_descriptor_hash.read().unwrap(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(descriptor, from_config);
        assert_eq!(
            registry.config_descriptor_bytes.len().unwrap(),
            descriptor.to_bytes().unwrap().len()
        );
        assert_eq!(registry.repository_count.read().unwrap(), 0);
    });
}

#[test]
fn malformed_or_missing_genesis_commitment_fails_before_unbounded_bytes_read() {
    for (slot, word) in [
        (10, U256::ZERO),
        (10, U256::from(1)),
        (11, U256::MAX),
        (11, U256::from(254)),
        (11, U256::ZERO),
    ] {
        let (mut provider, _) = seeded();
        provider
            .storage
            .insert((RADICLE_REGISTRY_ADDRESS, U256::from(slot)), word);
        StorageHandle::enter(&mut provider, |storage| {
            assert!(RadicleRegistry::new(storage)
                .governments_descriptor()
                .is_err());
        });
    }
    let (mut provider, _) = seeded();
    provider
        .storage
        .remove(&(RADICLE_REGISTRY_ADDRESS, U256::from(10)));
    provider
        .storage
        .remove(&(RADICLE_REGISTRY_ADDRESS, U256::from(11)));
    StorageHandle::enter(&mut provider, |storage| {
        assert!(RadicleRegistry::new(storage)
            .governments_descriptor()
            .unwrap()
            .is_none());
    });
}
