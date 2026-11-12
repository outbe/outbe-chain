#![cfg(feature = "snapshot-integration")]

#[cfg(feature = "snapshot-integration")]
mod copied_genesis_validator_history {
    use alloy_consensus::Sealable;
    use alloy_primitives::{Address, B256};
    use commonware_codec::Encode as _;
    use commonware_cryptography::{bls12381, Signer as _};
    use outbe_consensus::validators::read_consensus_validators_at_block;
    use outbe_engine::stack::snapshot_native::{open_native, DiskFixture, H};
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
    use reth_ethereum::provider::db::{
        database::Database,
        init_db,
        mdbx::DatabaseArguments,
        tables,
        transaction::{DbTx, DbTxMut},
    };
    use std::{collections::BTreeSet, path::Path};

    const OWNER: Address = Address::repeat_byte(0xa0);
    const FOUNDER: Address = Address::with_last_byte(0x11);
    const JOINER: Address = Address::with_last_byte(0x22);

    fn consensus_key(seed: u64) -> bls12381::PublicKey {
        bls12381::PrivateKey::from_seed(seed).public_key()
    }

    fn activate(provider: &mut HashMapStorageProvider, address: Address, seed: u64) {
        let encoded = consensus_key(seed).encode();
        let public_key: [u8; 48] = encoded.as_ref().try_into().unwrap();
        StorageHandle::enter(provider, |storage| {
            let mut validators = outbe_validatorset::contract::ValidatorSet::new(storage);
            let minimum = alloy_primitives::U256::from(1_000u64);
            validators
                .test_seed_authoritative_min_stake(minimum)
                .unwrap();
            validators
                .register_validator(OWNER, address, &public_key)
                .unwrap();
            validators
                .test_seed_authoritative_bonded(address, minimum)
                .unwrap();
            validators
                .activate_validator_via_boundary_for_test(address)
                .unwrap();
        });
    }

    // Build native contract words with the existing lifecycle fixture, then persist
    // both current state and actual historical before-values. The runtime reader
    // below only sees a reopened Reth provider, never this construction map.
    fn seed_distinct_validator_history(root: &Path) -> BTreeSet<Address> {
        let mut contract = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut contract, |storage| {
            let mut validators = outbe_validatorset::contract::ValidatorSet::new(storage);
            validators.config_owner.write(OWNER).unwrap();
            validators.config_is_initialized.write(true).unwrap();
            validators.set_config_max_validators(128).unwrap();
        });
        activate(&mut contract, FOUNDER, 11);
        let genesis = contract.storage.clone();
        contract.set_block_number(1);
        activate(&mut contract, JOINER, 22);
        let current = contract.storage;
        outbe_engine::stack::snapshot_native::validator_history::persist(
            root,
            outbe_engine::stack::snapshot_native::validator_history::Snapshots {
                before: &genesis,
                after: &current,
                change_height: 1,
            },
            |changed_addresses| {
                assert!(
                    !changed_addresses.is_empty(),
                    "the genesis/current distinction must require native history"
                );
            },
        )
        .expect("native distinct validator history")
    }

    fn assert_genesis_and_current(root: &Path, genesis_hash: B256, current_hash: B256) {
        let provider = open_native(root);

        let genesis = read_consensus_validators_at_block(
            &outbe_runtime::OutbeRuntime::new(),
            &provider,
            genesis_hash,
        )
        .unwrap();
        assert_eq!(genesis.addresses, vec![FOUNDER]);
        assert_eq!(genesis.public_keys, vec![consensus_key(11)]);
        let current = read_consensus_validators_at_block(
            &outbe_runtime::OutbeRuntime::new(),
            &provider,
            current_hash,
        )
        .unwrap();
        assert_eq!(current.addresses, vec![FOUNDER, JOINER]);
        assert_eq!(
            current.public_keys,
            vec![consensus_key(11), consensus_key(22)]
        );
        assert_ne!(genesis.public_keys, current.public_keys);
    }

    #[test]
    fn copied_genesis_validator_trust_anchor_reads_native_history_instead_of_current_set() {
        let fixture = DiskFixture::new(H - 1);
        seed_distinct_validator_history(fixture.root.path());
        let genesis_hash = fixture.headers[0].hash_slow();
        let current_hash = fixture.headers[H as usize].hash_slow();
        assert_genesis_and_current(fixture.root.path(), genesis_hash, current_hash);
        let recipient = tempfile::tempdir().unwrap();
        fixture.copy_to(recipient.path());
        // The donor may disappear; the trust reader is given only the copied native provider.
        drop(fixture);
        assert_genesis_and_current(recipient.path(), genesis_hash, current_hash);
        assert_genesis_and_current(recipient.path(), genesis_hash, current_hash);
    }

    #[test]
    fn copied_genesis_validator_trust_anchor_rejects_missing_required_storage_changeset() {
        let fixture = DiskFixture::new(H - 1);
        let addresses = seed_distinct_validator_history(fixture.root.path());
        let genesis_hash = fixture.headers[0].hash_slow();
        let current_hash = fixture.headers[H as usize].hash_slow();
        let recipient = tempfile::tempdir().unwrap();
        fixture.copy_to(recipient.path());
        assert_genesis_and_current(recipient.path(), genesis_hash, current_hash);
        {
            let db = init_db(recipient.path().join("db"), DatabaseArguments::test()).unwrap();
            let tx = db.tx_mut().unwrap();
            // Leave the history index, headers, latest state at H intact.
            // It now references missing native before-values for the block-1 change.
            for &address in &addresses {
                assert!(tx
                    .delete::<tables::StorageChangeSets>((1, address).into(), None)
                    .unwrap());
            }
            tx.commit().unwrap();
        }
        {
            let provider = open_native(recipient.path());

            let current = read_consensus_validators_at_block(
                &outbe_runtime::OutbeRuntime::new(),
                &provider,
                current_hash,
            )
            .unwrap();
            assert_eq!(current.addresses, vec![FOUNDER, JOINER]);
            assert_eq!(
                current.public_keys,
                vec![consensus_key(11), consensus_key(22)]
            );
            let error = read_consensus_validators_at_block(
                &outbe_runtime::OutbeRuntime::new(),
                &provider,
                genesis_hash,
            )
            .unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains("storage change set"), "{message}");
            assert!(message.contains("at block #1 does not exist"), "{message}");
        }
        // The damaged copy did not repair its history or alter the donor.
        {
            let db = init_db(recipient.path().join("db"), DatabaseArguments::test()).unwrap();
            let tx = db.tx().unwrap();
            for &address in &addresses {
                assert!(tx
                    .get::<tables::StorageChangeSets>((1, address).into())
                    .unwrap()
                    .is_none());
            }
            assert_eq!(
                tx.get::<tables::CanonicalHeaders>(0).unwrap(),
                Some(genesis_hash)
            );
            assert_eq!(
                tx.get::<tables::CanonicalHeaders>(H).unwrap(),
                Some(current_hash)
            );
        }
        assert_genesis_and_current(fixture.root.path(), genesis_hash, current_hash);
    }
}
