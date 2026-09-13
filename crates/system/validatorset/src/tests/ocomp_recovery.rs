use super::*;

#[test]
fn ocomp_miss_opens_one_fixed_recovery_window_and_repeats_do_not_extend_it() {
    let validator = address!("0x9191919191919191919191919191919191919191");
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_block_number(100);

    StorageHandle::enter(&mut provider, |storage| {
        let mut validators = ValidatorSet::new(storage.clone());
        validators.config_owner.write(OWNER).unwrap();
        validators.set_config_max_validators(1).unwrap();
        validators
            .register_validator(OWNER, validator, &dummy_consensus_pubkey(0x91))
            .unwrap();
        validators
            .activate_validator_via_boundary_for_test(validator)
            .unwrap();

        let first = validators.record_ocomp_miss(validator).unwrap();
        assert_eq!(
            first,
            crate::runtime::OcompMissRecord::Opened {
                miss_count: 1,
                recovery_deadline: 43_300,
            }
        );
        assert_eq!(
            validators
                .val_ocomp_recovery_deadline
                .read(&validator)
                .unwrap(),
            43_300
        );
    });

    provider.set_block_number(101);
    StorageHandle::enter(&mut provider, |storage| {
        let mut validators = ValidatorSet::new(storage);
        let repeated = validators.record_ocomp_miss(validator).unwrap();
        assert_eq!(
            repeated,
            crate::runtime::OcompMissRecord::Repeated {
                miss_count: 2,
                recovery_deadline: 43_300,
            }
        );
        assert_eq!(
            validators
                .val_ocomp_recovery_deadline
                .read(&validator)
                .unwrap(),
            43_300
        );
    });
}

#[test]
fn validator_reregistration_cannot_erase_an_open_ocomp_recovery_window() {
    with_vs_configured(1, |validators| {
        let validator = address!("0x9292929292929292929292929292929292929292");
        let public_key = dummy_consensus_pubkey(0x92);
        validators
            .register_validator(OWNER, validator, &public_key)
            .unwrap();
        validators
            .activate_validator_via_boundary_for_test(validator)
            .unwrap();
        validators.record_ocomp_miss(validator).unwrap();
        make_inactive_for_test(validators, validator);
        assert!(validators
            .ocomp_recovery_window(validator)
            .unwrap()
            .is_some());

        assert!(matches!(
            validators.register_validator(OWNER, validator, &public_key),
            Err(PrecompileError::Revert(message))
                if message.contains("OCOMP recovery window is open")
        ));
        assert!(validators
            .ocomp_recovery_window(validator)
            .unwrap()
            .is_some());
        assert_eq!(validators.val_ocomp_miss_count.read(&validator).unwrap(), 1);
        assert_eq!(validators.cleanup_inactive_validators(1).unwrap(), 0);
        assert!(validators.get_validator(validator).unwrap().is_some());
    });
}
