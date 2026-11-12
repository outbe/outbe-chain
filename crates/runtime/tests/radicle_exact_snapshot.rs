use alloy_primitives::{Address, B256, U256};
use commonware_cryptography::{bls12381, Signer as _};
use outbe_coderegistry::CodeRegistry;
use outbe_primitives::code_registry::RepoId;
use outbe_primitives::storage::readonly::StorageReader;
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_radicle::endpoint::PeerId;
use outbe_radicle::manager::{read_snapshot_from_storage, FinalizedBlock};
use outbe_runtime::OutbeRuntime;
use outbe_validatorset::contract::ValidatorSet;

struct MapReader(HashMapStorageProvider);

impl StorageReader for MapReader {
    fn read_storage(&self, address: Address, key: B256) -> outbe_primitives::error::Result<U256> {
        Ok(self
            .0
            .storage
            .get(&(address, U256::from_be_bytes(key.0)))
            .copied()
            .unwrap_or_default())
    }
}

#[test]
fn exact_snapshot() {
    let chain_id = 54_322_345;
    let genesis_hash = B256::repeat_byte(0x44);
    let validator = Address::repeat_byte(0x11);
    let signer = bls12381::PrivateKey::from_seed(7);
    let mut public_key = [0; 48];
    public_key.copy_from_slice(signer.public_key().as_ref());
    let repository = RepoId::repeat_byte(0x22);
    let mut provider = HashMapStorageProvider::new_with_chain_identity(chain_id, genesis_hash);
    provider.set_block_number(1);
    StorageHandle::enter(&mut provider, |storage| {
        let mut validators = ValidatorSet::new(storage.clone());
        validators
            .config_owner
            .write(Address::repeat_byte(0xaa))
            .unwrap();
        validators.set_config_max_validators(4).unwrap();
        validators.config_epoch_length_blocks.write(10).unwrap();
        validators
            .test_register_validator_without_pop(validator, &public_key)
            .unwrap();
        validators.activate_validator(validator).unwrap();

        let mut registry = CodeRegistry::new(storage);
        registry.config_max_repositories.write(4).unwrap();
        registry.register_repository(repository, validator).unwrap();
    });

    let expected = FinalizedBlock {
        number: 9,
        hash: B256::repeat_byte(0x99),
    };
    let snapshot = read_snapshot_from_storage(
        MapReader(provider),
        chain_id,
        genesis_hash,
        expected,
        &OutbeRuntime::new(),
    )
    .unwrap();
    assert_eq!(snapshot.block, expected);
    assert_eq!(snapshot.registry_generation, 1);
    assert_eq!(snapshot.repositories, [repository]);
    assert_eq!(snapshot.validators.len(), 1);
    assert_eq!(snapshot.validators[0].address, validator);
    assert_eq!(
        snapshot.validators[0].peer,
        PeerId::from_public_key(&signer.public_key())
    );
    assert!(snapshot.validators[0].node_id.is_some());
}
