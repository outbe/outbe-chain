use super::super::*;
#[test]
fn genesis_validation_rejects_active_validator_with_zero_stake() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let validator = address!("0x1111111111111111111111111111111111111111");
        let pk = dummy_pubkey(0xA1);
        seed_registered_active_validator(storage.clone(), validator, &pk);

        let staking = outbe_staking::contract::Staking::new(storage.clone());
        staking.config_min_stake.write(U256::from(100u64)).unwrap();

        let genesis = GenesisValidators {
            validators: vec![GenesisValidator {
                address: validator,
                consensus_pubkey: pk,
            }],
            epoch_length_blocks: 60,
        };

        let err = super::super::validate_genesis_state(storage.clone(), &genesis).unwrap_err();
        assert!(err.to_string().contains("stake below min_stake"));
    });
}

#[test]
fn genesis_validation_accepts_staked_active_validator() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let validator = address!("0x1111111111111111111111111111111111111111");
        let pk = dummy_pubkey(0xA1);
        let stake = U256::from(100u64);
        seed_registered_active_validator(storage.clone(), validator, &pk);

        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.test_set_stake_projection(
            validator,
            outbe_validatorset::StakeProjection::new(stake, None),
        )
        .unwrap();

        let staking = outbe_staking::contract::Staking::new(storage.clone());
        staking.config_min_stake.write(stake).unwrap();
        staking.stake_amount.write(&validator, stake).unwrap();
        staking.total_staked.write(stake).unwrap();

        let genesis = GenesisValidators {
            validators: vec![GenesisValidator {
                address: validator,
                consensus_pubkey: pk,
            }],
            epoch_length_blocks: 60,
        };

        super::super::validate_genesis_state(storage.clone(), &genesis).unwrap();
    });
}
