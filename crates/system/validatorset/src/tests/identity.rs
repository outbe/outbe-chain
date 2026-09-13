use super::*;

fn symmetric_p2p(port: u16) -> Vec<u8> {
    encode_v1(&P2pAddress::Symmetric(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
        port,
    )))
}

#[test]
fn test_set_p2p_address_owner_or_self_and_get() {
    let val_addr = address!("0x2222222222222222222222222222222222222223");
    let pk = dummy_consensus_pubkey(23);

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val_addr, &pk).unwrap();

        let encoded = symmetric_p2p(30400);
        vs.set_p2p_address(OWNER, val_addr, P2P_ADDRESS_VERSION_V1, &encoded)
            .unwrap();
        assert_eq!(
            vs.get_p2p_address(val_addr).unwrap(),
            Some((P2P_ADDRESS_VERSION_V1, encoded.clone()))
        );

        let replacement = symmetric_p2p(30401);
        vs.set_p2p_address(val_addr, val_addr, P2P_ADDRESS_VERSION_V1, &replacement)
            .unwrap();
        assert_eq!(
            vs.get_p2p_address(val_addr).unwrap(),
            Some((P2P_ADDRESS_VERSION_V1, replacement))
        );
    });
}

#[test]
fn test_set_p2p_address_rejects_unauthorized_and_malformed() {
    let val_addr = address!("0x2222222222222222222222222222222222222224");
    let stranger = address!("0x9999999999999999999999999999999999999999");
    let pk = dummy_consensus_pubkey(24);

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val_addr, &pk).unwrap();
        let encoded = symmetric_p2p(30400);

        let err = vs
            .set_p2p_address(stranger, val_addr, P2P_ADDRESS_VERSION_V1, &encoded)
            .unwrap_err();
        assert!(
            matches!(err, PrecompileError::Revert(message) if message.contains("unauthorized"))
        );

        let err = vs
            .set_p2p_address(OWNER, val_addr, 2, &encoded)
            .unwrap_err();
        assert!(
            matches!(err, PrecompileError::Revert(message) if message.contains("unsupported p2p address version"))
        );

        let malformed = [0u8; 3];
        let err = vs
            .set_p2p_address(OWNER, val_addr, P2P_ADDRESS_VERSION_V1, &malformed)
            .unwrap_err();
        assert!(
            matches!(err, PrecompileError::Revert(message) if message.contains("invalid p2p address"))
        );
    });
}

#[test]
fn test_set_p2p_address_rejects_oversized_and_accepts_asymmetric() {
    let val_addr = address!("0x2222222222222222222222222222222222222225");
    let pk = dummy_consensus_pubkey(25);

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val_addr, &pk).unwrap();

        let oversized = vec![0u8; MAX_P2P_ADDRESS_ENCODED_LEN + 1];
        let err = vs
            .set_p2p_address(OWNER, val_addr, P2P_ADDRESS_VERSION_V1, &oversized)
            .unwrap_err();
        assert!(
            matches!(err, PrecompileError::Revert(message) if message.contains("exceeds max length"))
        );

        let asymmetric = encode_v1(&P2pAddress::Asymmetric {
            ingress: P2pIngress::Dns {
                host: "validator-1.example.com".to_owned(),
                port: 30400,
            },
            egress: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)), 30401),
        });
        vs.set_p2p_address(OWNER, val_addr, P2P_ADDRESS_VERSION_V1, &asymmetric)
            .unwrap();
        assert_eq!(
            vs.get_p2p_address(val_addr).unwrap(),
            Some((P2P_ADDRESS_VERSION_V1, asymmetric))
        );
    });
}

#[test]
fn malformed_p2p_fails_all_complete_record_projections_closed() {
    let val_addr = address!("0x2222222222222222222222222222222222222226");
    let pk = dummy_consensus_pubkey(6);

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val_addr, &pk).unwrap();
        vs.val_p2p_address_version
            .write(&val_addr, P2P_ADDRESS_VERSION_V1)
            .unwrap();
        vs.val_p2p_address_payload
            .get_bytes(&val_addr)
            .write(&[0xFF])
            .unwrap();

        // Every complete projection now passes through the canonical aggregate;
        // malformed coupled P2P fields therefore fail closed consistently.
        assert!(matches!(
            vs.get_validator(val_addr),
            Err(PrecompileError::Fatal(_))
        ));
        assert!(matches!(
            vs.get_all_validators(),
            Err(PrecompileError::Fatal(_))
        ));
        assert!(matches!(
            vs.validator_state(val_addr),
            Err(PrecompileError::Fatal(_))
        ));
    });
}

#[test]
fn typed_storage_adapter_rejects_pre_registration_stake_and_fails_closed() {
    let addr = address!("0x2222222222222222222222222222222222222227");

    with_vs_configured(10, |vs| {
        vs.val_stake.write(&addr, U256::from(900)).unwrap();
        vs.val_unbonding_end.write(&addr, 55).unwrap();

        assert!(matches!(
            vs.validator_state(addr),
            Err(PrecompileError::Fatal(_))
        ));

        vs.val_stake.write(&addr, U256::ZERO).unwrap();
        vs.val_unbonding_end.write(&addr, 0).unwrap();
        assert_eq!(
            vs.validator_state(addr).unwrap().lifecycle(),
            &crate::ValidatorLifecycle::Absent
        );

        vs.val_join_confirmed.write(&addr, true).unwrap();
        assert!(matches!(
            vs.validator_state(addr),
            Err(PrecompileError::Fatal(_))
        ));

        vs.val_join_confirmed.write(&addr, false).unwrap();
        vs.val_status.write(&addr, 7).unwrap();
        assert!(matches!(
            vs.validator_state(addr),
            Err(PrecompileError::Fatal(_))
        ));
    });
}

#[test]
fn full_and_hot_lifecycle_reads_both_reject_combined_residue() {
    let addr = address!("0x2222222222222222222222222222222222222228");

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, addr, &dummy_consensus_pubkey(28))
            .unwrap();
        vs.val_status.write(&addr, status::UNBONDING).unwrap();
        vs.val_join_confirmed.write(&addr, true).unwrap();
        vs.val_has_bls_share.write(&addr, true).unwrap();
        vs.val_jailed_at_height.write(&addr, 99).unwrap();

        assert!(matches!(
            vs.validator_state(addr),
            Err(PrecompileError::Fatal(_))
        ));
        assert!(matches!(
            vs.validator_lifecycle(addr),
            Err(PrecompileError::Fatal(_))
        ));
    });
}

fn signed_radicle_registration(
    seed: u8,
    chain_id: u64,
    validator: Address,
    node_id: B256,
) -> ([u8; 48], [u8; 96]) {
    use blst::min_pk::SecretKey;

    let sk = SecretKey::key_gen(&[seed; 32], &[]).unwrap();
    let public_key = sk.sk_to_pk().to_bytes();
    let message = validator_registration_message(chain_id, validator, node_id);
    let signature = sk
        .sign(&message, VALIDATOR_REGISTRATION_DST, &[])
        .to_bytes();
    (public_key, signature)
}

#[test]
fn radicle_node_id_registration_is_bidirectional_and_signature_bound() {
    let validator = Address::repeat_byte(0x71);
    let node_id = B256::repeat_byte(0x81);
    let other_node_id = B256::repeat_byte(0x82);
    let (public_key, signature) = signed_radicle_registration(0x31, CHAIN_ID, validator, node_id);

    with_vs_configured(10, |vs| {
        vs.register_validator_with_sig(
            validator,
            validator,
            &public_key,
            node_id,
            Some(&signature),
        )
        .unwrap();

        assert_eq!(vs.get_radicle_node_id(validator).unwrap(), node_id);
        assert_eq!(vs.validator_by_radicle_node_id(node_id).unwrap(), validator);
        assert!(vs
            .validator_by_radicle_node_id(other_node_id)
            .unwrap()
            .is_zero());
    });

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_block_number(1);
    StorageHandle::enter(&mut provider, |storage| {
        let mut vs = ValidatorSet::new(storage);
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(10).unwrap();
        let err = vs
            .register_validator_with_sig(
                validator,
                validator,
                &public_key,
                other_node_id,
                Some(&signature),
            )
            .unwrap_err();
        assert!(err
            .to_string()
            .contains("invalid BLS registration signature"));
    });
}

#[test]
fn radicle_node_id_rejects_zero_and_duplicate_without_partial_state() {
    let first = Address::repeat_byte(0x72);
    let second = Address::repeat_byte(0x73);
    let node_id = B256::repeat_byte(0x83);
    let (first_key, first_signature) = signed_radicle_registration(0x32, CHAIN_ID, first, node_id);
    let (second_key, second_signature) =
        signed_radicle_registration(0x33, CHAIN_ID, second, node_id);
    let (zero_key, zero_signature) =
        signed_radicle_registration(0x34, CHAIN_ID, second, B256::ZERO);

    with_vs_configured(10, |vs| {
        let zero = vs
            .register_validator_with_sig(
                second,
                second,
                &zero_key,
                B256::ZERO,
                Some(&zero_signature),
            )
            .unwrap_err();
        assert!(zero.to_string().contains("Radicle NodeId must not be zero"));
        assert_eq!(vs.validator_count().unwrap(), 0);

        vs.register_validator_with_sig(first, first, &first_key, node_id, Some(&first_signature))
            .unwrap();
        let duplicate = vs
            .register_validator_with_sig(
                second,
                second,
                &second_key,
                node_id,
                Some(&second_signature),
            )
            .unwrap_err();
        assert!(duplicate
            .to_string()
            .contains("Radicle NodeId already registered"));
        assert_eq!(vs.validator_count().unwrap(), 1);
        assert!(vs.get_radicle_node_id(second).unwrap().is_zero());
        assert_eq!(vs.validator_by_radicle_node_id(node_id).unwrap(), first);
    });
}

#[test]
fn inactive_reregistration_preserves_node_id_until_final_cleanup() {
    let validator = Address::repeat_byte(0x74);
    let first_node_id = B256::repeat_byte(0x84);
    let second_node_id = B256::repeat_byte(0x85);
    let (first_key, first_signature) =
        signed_radicle_registration(0x35, CHAIN_ID, validator, first_node_id);
    let (second_key, first_node_signature) =
        signed_radicle_registration(0x36, CHAIN_ID, validator, first_node_id);
    let (_, second_node_signature) =
        signed_radicle_registration(0x36, CHAIN_ID, validator, second_node_id);

    with_vs_configured(10, |vs| {
        vs.register_validator_with_sig(
            validator,
            validator,
            &first_key,
            first_node_id,
            Some(&first_signature),
        )
        .unwrap();
        make_inactive_for_test(vs, validator);

        let changed = vs
            .register_validator_with_sig(
                validator,
                validator,
                &second_key,
                second_node_id,
                Some(&second_node_signature),
            )
            .unwrap_err();
        assert!(changed
            .to_string()
            .contains("inactive validator must keep its Radicle NodeId"));

        vs.register_validator_with_sig(
            validator,
            validator,
            &second_key,
            first_node_id,
            Some(&first_node_signature),
        )
        .unwrap();
        assert_eq!(vs.get_radicle_node_id(validator).unwrap(), first_node_id);

        make_inactive_for_test(vs, validator);
        assert_eq!(vs.cleanup_inactive_validators(0).unwrap(), 1);
        assert!(vs.get_radicle_node_id(validator).unwrap().is_zero());
        assert!(vs
            .validator_by_radicle_node_id(first_node_id)
            .unwrap()
            .is_zero());

        vs.register_validator_with_sig(
            validator,
            validator,
            &second_key,
            second_node_id,
            Some(&second_node_signature),
        )
        .unwrap();
        assert_eq!(vs.get_radicle_node_id(validator).unwrap(), second_node_id);
    });
}

#[test]
fn every_radicle_registration_mutation_rolls_back_atomically() {
    let validator = Address::repeat_byte(0x75);
    let node_id = B256::repeat_byte(0x86);
    let (public_key, signature) = signed_radicle_registration(0x37, CHAIN_ID, validator, node_id);

    let configured_provider = || {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        provider.set_block_number(1);
        StorageHandle::enter(&mut provider, |storage| {
            let vs = ValidatorSet::new(storage);
            vs.config_owner.write(OWNER).unwrap();
            vs.config_max_validators.write(10).unwrap();
        });
        provider
    };

    let mut measured = configured_provider();
    measured.fail_after_mutation_at(usize::MAX);
    StorageHandle::enter(&mut measured, |storage| {
        ValidatorSet::new(storage)
            .register_validator_with_sig(
                validator,
                validator,
                &public_key,
                node_id,
                Some(&signature),
            )
            .unwrap();
    });
    let operation_count = measured.clear_mutation_failure();
    assert!(operation_count > 2);

    for failure_at in 0..operation_count {
        let mut provider = configured_provider();
        provider.fail_after_mutation_at(failure_at);
        StorageHandle::enter(&mut provider, |storage| {
            assert!(ValidatorSet::new(storage)
                .register_validator_with_sig(
                    validator,
                    validator,
                    &public_key,
                    node_id,
                    Some(&signature),
                )
                .is_err());
        });
        provider.clear_mutation_failure();
        StorageHandle::enter(&mut provider, |storage| {
            let vs = ValidatorSet::new(storage);
            assert_eq!(vs.validator_count().unwrap(), 0);
            assert!(vs.get_radicle_node_id(validator).unwrap().is_zero());
            assert!(vs.validator_by_radicle_node_id(node_id).unwrap().is_zero());
            assert!(!vs.is_validator(validator).unwrap());
        });
        assert!(provider
            .get_events(outbe_primitives::addresses::VALIDATOR_SET_ADDRESS)
            .is_empty());
    }
}
