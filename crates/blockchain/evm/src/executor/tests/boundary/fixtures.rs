use super::super::*;
pub(super) fn state_with_active_and_registered_candidate(
    active: Address,
    candidate: Address,
) -> State<CacheDB<EmptyDBTyped<ProviderError>>> {
    state_with_active_and_registered_candidate_seeded(active, candidate, |_| {})
}

pub(super) fn state_with_active_and_registered_candidate_seeded(
    active: Address,
    candidate: Address,
    seed_extra: impl FnOnce(StorageHandle),
) -> State<CacheDB<EmptyDBTyped<ProviderError>>> {
    let chain_spec = test_chain_spec();
    let mut seed_storage =
        HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, chain_spec.genesis_hash());
    let active_key = dummy_pubkey(0xA2);
    let install = test_ocomp_fork_install(&chain_spec, &[(active, active_key)]);
    StorageHandle::enter(&mut seed_storage, |storage| {
        seed_compressed_entities_genesis(&storage).expect("CE genesis fixture");
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_epoch_length_blocks.write(60).unwrap();
        vs.config_is_initialized.write(true).unwrap();
        register_and_activate_with_ocomp_registration(
            &mut vs,
            active,
            &active_key,
            &install.founder_registrations[0],
        );
        vs.register_validator(OWNER, candidate, &dummy_pubkey(0xB3))
            .unwrap();
        vs.admit_validator_for_boundary_for_test(candidate).unwrap();
        seed_test_committee_snapshot(storage.clone(), &[(active, active_key)]);
        // Seed the COEN/840 oracle pair + a 1.0 rate so begin-block
        // NOD/GEM/INTEX floor-price promotion resolves a live rate instead
        // of soft-skipping the scan. Also push 840 onto the reference currency
        // list to match genesis. The Nod qualifier reads its ISO from there, not
        // from a hard-coded constant.
        outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
            .unwrap();
        outbe_oracle::schema::OracleContract::new(storage.clone())
            .reference_currencies
            .push(outbe_oracle::api::DAY_TYPE_ISO)
            .unwrap();
        outbe_oracle::api::set_exchange_rate(
            storage.clone(),
            Address::ZERO,
            outbe_oracle::api::DAY_TYPE_PAIR,
            U256::from(1_000_000u64),
            0,
            0,
        )
        .unwrap();
        seed_extra(storage);
    });
    seed_test_ocomp_profile(&mut seed_storage, 0, &install);

    let mut db = cache_db_from_storage(seed_storage);
    let marker_code = Bytecode::new_legacy([0xef].into());
    db.insert_account_info(
        outbe_primitives::addresses::VALIDATOR_SET_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code.clone()),
            ..Default::default()
        },
    );
    db.insert_account_info(
        outbe_primitives::addresses::ORACLE_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code.clone()),
            ..Default::default()
        },
    );
    db.insert_account_info(
        outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code.clone()),
            ..Default::default()
        },
    );
    db.insert_account_info(
        outbe_primitives::addresses::METADOSIS_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code.clone()),
            ..Default::default()
        },
    );
    db.insert_account_info(
        outbe_primitives::addresses::OCOMP_REGISTRY_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code),
            ..Default::default()
        },
    );
    State::builder()
        .with_database(db)
        .with_bundle_update()
        .build()
}

pub(super) fn begin_system_tx_kinds(
    txs: &[reth_primitives_traits::Recovered<reth_ethereum::TransactionSigned>],
) -> Vec<crate::system_tx::SystemTxKind> {
    txs.iter()
        .map(|tx| {
            SystemTxInputV2::decode(tx.tx().input().as_ref())
                .expect("begin-zone calldata decodes")
                .kind()
        })
        .collect()
}

pub(super) fn test_register_joining(
    vs: &mut outbe_validatorset::contract::ValidatorSet<'_>,
    validator: Address,
    pubkey: &[u8; 48],
) {
    test_register_waiting(vs, validator, pubkey);
    vs.record_stake_increase(validator, U256::from(1), U256::from(1))
        .unwrap();
    vs.admit_validator_for_boundary_for_test(validator).unwrap();
}
