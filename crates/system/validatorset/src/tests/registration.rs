use super::*;

fn test_radicle_node_id(validator: Address) -> B256 {
    keccak256(validator.as_slice())
}

// ---------------------------------------------------------------------------
// 1. test_register_validator
// ---------------------------------------------------------------------------
#[test]
fn test_register_validator() {
    let val_addr = address!("0x1111111111111111111111111111111111111111");
    let pk = dummy_consensus_pubkey(1);

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val_addr, &pk).unwrap();

        // Index must be 1
        assert_eq!(vs.address_to_index.read(&val_addr).unwrap(), 1);
        assert_eq!(vs.index_to_address.read(&1u64).unwrap(), val_addr);

        // Status must be REGISTERED after registration
        assert_eq!(vs.val_status.read(&val_addr).unwrap(), status::REGISTERED);

        // Consensus pubkey stored correctly (read back via get_validator)
        let record = vs.get_validator(val_addr).unwrap().unwrap();
        assert_eq!(record.consensus_pubkey, pk);

        // Reverse lookup by pubkey hash
        let pk_hash = ValidatorSet::consensus_pubkey_hash(&pk);
        assert_eq!(
            vs.consensus_pubkey_hash_to_address.read(&pk_hash).unwrap(),
            val_addr
        );

        // Count incremented
        assert_eq!(vs.validator_count.read().unwrap(), 1);

        // pending_set_change should be set
        assert!(vs.pending_set_change.read().unwrap());
    });
}

// ---------------------------------------------------------------------------
// 2. test_register_self - self-registration now requires BLS proof
// ---------------------------------------------------------------------------
#[test]
fn test_register_self_without_sig_rejected() {
    let val_addr = address!("0x2222222222222222222222222222222222222222");
    let pk = dummy_consensus_pubkey(2);

    with_vs_configured(10, |vs| {
        // Self-registration without BLS signature must fail
        let result = vs.register_validator_with_sig(
            val_addr,
            val_addr,
            &pk,
            test_radicle_node_id(val_addr),
            None,
        );
        assert!(
            result.is_err(),
            "self-registration without BLS sig must be rejected"
        );
    });
}

#[test]
fn test_register_via_owner() {
    let val_addr = address!("0x2222222222222222222222222222222222222222");
    let pk = dummy_consensus_pubkey(2);

    with_vs_configured(10, |vs| {
        // Owner registration path - no BLS sig required
        vs.register_validator(OWNER, val_addr, &pk).unwrap();
        assert!(vs.is_validator(val_addr).unwrap());
    });
}

// ---------------------------------------------------------------------------
// 3. test_register_duplicate_fails
// ---------------------------------------------------------------------------
#[test]
fn test_register_duplicate_fails() {
    let val_addr = address!("0x3333333333333333333333333333333333333333");
    let pk = dummy_consensus_pubkey(3);

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val_addr, &pk).unwrap();
        let result = vs.register_validator(OWNER, val_addr, &dummy_consensus_pubkey(30));
        assert!(result.is_err(), "duplicate registration must fail");
    });
}

// ---------------------------------------------------------------------------
// 4. test_register_max_validators
// ---------------------------------------------------------------------------
#[test]
fn test_register_max_validators() {
    with_vs_configured(2, |vs| {
        let addr1 = address!("0x0000000000000000000000000000000000000011");
        let addr2 = address!("0x0000000000000000000000000000000000000022");
        let addr3 = address!("0x0000000000000000000000000000000000000033");

        vs.register_validator(OWNER, addr1, &dummy_consensus_pubkey(11))
            .unwrap();
        vs.register_validator(OWNER, addr2, &dummy_consensus_pubkey(22))
            .unwrap();

        let result = vs.register_validator(OWNER, addr3, &dummy_consensus_pubkey(33));
        assert!(result.is_err(), "should fail when max validators reached");
    });
}

// ---------------------------------------------------------------------------
// 10. test_get_active_validators
// ---------------------------------------------------------------------------
#[test]
fn test_get_active_validators() {
    let val1 = address!("0x00000000000000000000000000000000000000A1");
    let val2 = address!("0x00000000000000000000000000000000000000A2");
    let val3 = address!("0x00000000000000000000000000000000000000A3");

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val1, &dummy_consensus_pubkey(0xA1))
            .unwrap();
        vs.register_validator(OWNER, val2, &dummy_consensus_pubkey(0xA2))
            .unwrap();
        vs.register_validator(OWNER, val3, &dummy_consensus_pubkey(0xA3))
            .unwrap();

        // Activate only val1 and val3
        vs.activate_validator_via_boundary_for_test(val1).unwrap();
        vs.activate_validator_via_boundary_for_test(val3).unwrap();

        let active = vs.get_active_validators().unwrap();
        let active_addrs: Vec<Address> = active.iter().map(|v| v.validator_address).collect();

        assert_eq!(active.len(), 2);
        assert!(active_addrs.contains(&val1));
        assert!(!active_addrs.contains(&val2));
        assert!(active_addrs.contains(&val3));
    });
}

// ---------------------------------------------------------------------------
// 11. test_is_validator
// ---------------------------------------------------------------------------
#[test]
fn test_is_validator() {
    let registered = address!("0x00000000000000000000000000000000000000B1");
    let stranger = address!("0x00000000000000000000000000000000000000B2");

    with_vs_configured(10, |vs| {
        assert!(!vs.is_validator(registered).unwrap());
        assert!(!vs.is_validator(stranger).unwrap());

        vs.register_validator(OWNER, registered, &dummy_consensus_pubkey(0xB1))
            .unwrap();

        assert!(vs.is_validator(registered).unwrap());
        assert!(!vs.is_validator(stranger).unwrap());
    });
}

#[test]
fn config_max_validators_cannot_exceed_consensus_bound() {
    with_vs_configured(10, |vs| {
        let consensus_bound = outbe_consensus::bls::MAX_VALIDATORS;
        vs.set_config_max_validators(consensus_bound).unwrap();
        assert_eq!(vs.config_max_validators.read().unwrap(), consensus_bound);

        assert!(vs
            .set_config_max_validators(consensus_bound.saturating_add(1))
            .is_err());
        assert_eq!(
            vs.config_max_validators.read().unwrap(),
            consensus_bound,
            "rejected bound must not mutate config",
        );
    });
}

#[test]
fn production_validator_set_selector_allow_list_is_exact() {
    // Explicit public signatures pin the security boundary independently of
    // the generated ABI enum; adding any entry requires reviewing this list.
    let signatures = [
        "getValidators()",
        "getActiveValidators()",
        "getActiveConsensusSet()",
        "validatorByAddress(address)",
        "validatorByIndex(uint64)",
        "validatorCount()",
        "activeValidatorCount()",
        "activeConsensusCount()",
        "isValidator(address)",
        "isConsensusParticipant(address)",
        "hasPendingSetChange()",
        "getEpochNumber()",
        "getEpochStartTimestamp()",
        "getEpochStartBlock()",
        "setDelegate(uint8,address)",
        "revokeDelegate(uint8)",
        "getDelegate(address,uint8)",
        "resolveValidator(uint8,address)",
        "registerValidator(address,bytes,bytes32,bytes)",
        "getRadicleNodeId(address)",
        "validatorByRadicleNodeId(bytes32)",
        "setP2pAddress(address,uint8,bytes)",
        "getP2pAddress(address)",
        "deactivateValidator(address)",
        "confirmValidatorReady(bytes)",
    ];
    let mut expected = signatures.map(|signature| {
        let digest = keccak256(signature);
        <[u8; 4]>::try_from(&digest[..4]).unwrap()
    });
    expected.sort_unstable();
    assert!(
        expected.windows(2).all(|pair| pair[0] != pair[1]),
        "selector collision in allow-list"
    );
    let mut actual = crate::precompile::IValidatorSet::IValidatorSetCalls::SELECTORS.to_vec();
    actual.sort_unstable();
    assert_eq!(actual, expected, "public ValidatorSet selectors changed");
}

#[test]
fn owner_manual_reshare_selector_is_not_exposed() {
    let digest = keccak256("activateResharedSet(address[],bytes32)");
    let selector: [u8; 4] = digest[..4].try_into().unwrap();
    assert!(
        !crate::precompile::IValidatorSet::IValidatorSetCalls::SELECTORS.contains(&selector),
        "owner/manual activation must not be reachable through ValidatorSet ABI",
    );
}

// ---------------------------------------------------------------------------
// 15. test_consensus_pubkey_roundtrip
// ---------------------------------------------------------------------------
#[test]
fn test_consensus_pubkey_roundtrip() {
    let val_addr = address!("0x00000000000000000000000000000000000000F1");
    // Build a non-trivial 48-byte key
    let mut pk = [0u8; 48];
    for (i, byte) in pk.iter_mut().enumerate() {
        *byte = (i as u8).wrapping_add(0x10);
    }

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val_addr, &pk).unwrap();
        let record = vs.get_validator(val_addr).unwrap().unwrap();
        assert_eq!(record.consensus_pubkey, pk);
    });
}

// ---------------------------------------------------------------------------
// 16. test_pubkey_hash_lookup
// ---------------------------------------------------------------------------
#[test]
fn test_pubkey_hash_lookup() {
    let val_addr = address!("0x00000000000000000000000000000000000000F2");
    let pk = dummy_consensus_pubkey(0xF2);

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val_addr, &pk).unwrap();

        let pk_hash = ValidatorSet::consensus_pubkey_hash(&pk);
        let looked_up = vs.lookup_by_pubkey_hash(pk_hash).unwrap();
        assert_eq!(looked_up, val_addr);
    });
}

// ---------------------------------------------------------------------------
// 17. test_reregister_inactive_validator
// ---------------------------------------------------------------------------
#[test]
fn test_reregister_inactive_validator() {
    let val_addr = address!("0x1111111111111111111111111111111111111111");
    let pk_old = dummy_consensus_pubkey(0x11);
    let pk_new = dummy_consensus_pubkey(0x22);

    with_vs_configured(10, |vs| {
        // Register and transition to INACTIVE
        vs.register_validator(OWNER, val_addr, &pk_old).unwrap();
        make_inactive_for_test(vs, val_addr);

        // Re-register with a new pubkey
        vs.register_validator(OWNER, val_addr, &pk_new).unwrap();

        // Status reset to REGISTERED
        assert_eq!(vs.val_status.read(&val_addr).unwrap(), status::REGISTERED);

        // New pubkey stored
        let record = vs.get_validator(val_addr).unwrap().unwrap();
        assert_eq!(record.consensus_pubkey, pk_new);

        // Old pubkey hash cleared
        let old_hash = ValidatorSet::consensus_pubkey_hash(&pk_old);
        assert_eq!(
            vs.consensus_pubkey_hash_to_address.read(&old_hash).unwrap(),
            Address::ZERO
        );

        // New pubkey hash set
        let new_hash = ValidatorSet::consensus_pubkey_hash(&pk_new);
        assert_eq!(
            vs.consensus_pubkey_hash_to_address.read(&new_hash).unwrap(),
            val_addr
        );

        // Count unchanged (reused existing index)
        assert_eq!(vs.validator_count.read().unwrap(), 1);

        // Counters reset
        assert_eq!(record.slash_count, 0);
        assert_eq!(record.missed_blocks, 0);
        assert!(record.stake.is_zero());
        assert!(!vs.val_join_confirmed.read(&val_addr).unwrap());
        assert_eq!(vs.val_jailed_at_height.read(&val_addr).unwrap(), 0);
    });
}

// ---------------------------------------------------------------------------
// 18. test_reregister_active_fails
// ---------------------------------------------------------------------------
#[test]
fn test_reregister_active_fails() {
    let val_addr = address!("0x2222222222222222222222222222222222222222");

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val_addr, &dummy_consensus_pubkey(0x21))
            .unwrap();
        vs.activate_validator_via_boundary_for_test(val_addr)
            .unwrap();

        // Re-registration of ACTIVE validator must fail
        let result = vs.register_validator(OWNER, val_addr, &dummy_consensus_pubkey(0x22));
        assert!(result.is_err());
    });
}

// ===========================================================================
// BLS pubkey uniqueness tests
// ===========================================================================

#[test]
fn test_duplicate_pubkey_rejected() {
    with_vs_configured(10, |vs| {
        let val_a = address!("0x1818181818181818181818181818181818181818");
        let val_b = address!("0x1919191919191919191919191919191919191919");
        let pk = dummy_consensus_pubkey(18);

        vs.register_validator(OWNER, val_a, &pk).unwrap();
        // Same pubkey for different validator must fail
        let result = vs.register_validator(OWNER, val_b, &pk);
        assert!(result.is_err(), "duplicate BLS pubkey must be rejected");
    });
}

// ===========================================================================
// Invalid BLS signature rejected for self-registration
// ===========================================================================

#[test]
fn test_register_self_invalid_sig_rejected() {
    with_vs_configured(10, |vs| {
        let val = address!("0x4545454545454545454545454545454545454545");
        let pk = dummy_consensus_pubkey(45);
        let bad_sig = [0xFFu8; 96]; // garbage signature

        let result = vs.register_validator_with_sig(
            val,
            val,
            &pk,
            test_radicle_node_id(val),
            Some(&bad_sig),
        );
        assert!(result.is_err(), "invalid BLS sig must be rejected");
    });
}

/// Valid self-registration with correct BLS signature succeeds.
// the free, permissionless self-registration surface is capped at
// MAX_SELF_REGISTERED_UNSTAKED; owner registrations bypass the cap.
#[test]
fn m27_self_registration_capped_owner_bypasses() {
    use crate::runtime::MAX_SELF_REGISTERED_UNSTAKED;
    use blst::min_pk::SecretKey;

    fn self_reg_inputs(i: u32) -> (Address, [u8; 48], [u8; 96]) {
        let mut ikm = [7u8; 32];
        ikm[28..].copy_from_slice(&i.to_be_bytes());
        let sk = SecretKey::key_gen(&ikm, &[]).unwrap();
        let mut ab = [0u8; 20];
        ab[16..].copy_from_slice(&i.to_be_bytes());
        // avoid the zero address (index 0 sentinel) by setting a high byte.
        ab[0] = 0x5a;
        let val = Address::from(ab);
        let pk: [u8; 48] = sk.sk_to_pk().to_bytes();
        let node_id = test_radicle_node_id(val);
        let message = validator_registration_message(CHAIN_ID, val, node_id);
        let sig: [u8; 96] = sk
            .sign(&message, VALIDATOR_REGISTRATION_DST, &[])
            .to_bytes();
        (val, pk, sig)
    }

    // max_validators well above the self-registration cap so the cap, not the
    // global capacity, is what bites.
    with_vs_configured(200, |vs| {
        for i in 0..MAX_SELF_REGISTERED_UNSTAKED {
            let (val, pk, sig) = self_reg_inputs(i);
            vs.register_validator_with_sig(val, val, &pk, test_radicle_node_id(val), Some(&sig))
                .unwrap_or_else(|e| panic!("self-registration {i} within cap must succeed: {e}"));
        }
        assert_eq!(
            vs.registered_count().unwrap(),
            MAX_SELF_REGISTERED_UNSTAKED,
            "exactly the cap of self-registrations should be REGISTERED"
        );

        // The next self-registration is rejected before consuming a slot.
        let (val, pk, sig) = self_reg_inputs(MAX_SELF_REGISTERED_UNSTAKED);
        let err = vs
            .register_validator_with_sig(val, val, &pk, test_radicle_node_id(val), Some(&sig))
            .unwrap_err();
        assert!(
            err.to_string().contains("self-registration limit reached"),
            "over-cap self-registration must be rejected, got: {err}"
        );

        // The owner can still register validators directly, bypassing the cap.
        let owner_val = address!("0x000000000000000000000000000000000000beef");
        let mut ikm = [9u8; 32];
        ikm[28..].copy_from_slice(&(MAX_SELF_REGISTERED_UNSTAKED + 1).to_be_bytes());
        let owner_sk = SecretKey::key_gen(&ikm, &[]).unwrap();
        let owner_pk: [u8; 48] = owner_sk.sk_to_pk().to_bytes();
        let owner_node_id = test_radicle_node_id(owner_val);
        let owner_message = validator_registration_message(CHAIN_ID, owner_val, owner_node_id);
        let owner_sig: [u8; 96] = owner_sk
            .sign(&owner_message, VALIDATOR_REGISTRATION_DST, &[])
            .to_bytes();
        vs.register_validator_with_sig(
            OWNER,
            owner_val,
            &owner_pk,
            owner_node_id,
            Some(&owner_sig),
        )
        .expect("owner registration must bypass the self-registration cap");
        assert!(vs.is_validator(owner_val).unwrap());
    });
}

#[test]
fn test_register_self_valid_sig_accepted() {
    use blst::min_pk::SecretKey;

    with_vs_configured(10, |vs| {
        let val = address!("0x4646464646464646464646464646464646464646");
        let ikm = [46u8; 32];
        let sk = SecretKey::key_gen(&ikm, &[]).unwrap();
        let pk = sk.sk_to_pk();
        let pk_bytes: [u8; 48] = pk.to_bytes();

        let node_id = test_radicle_node_id(val);
        let message = validator_registration_message(CHAIN_ID, val, node_id);
        let sig = sk.sign(&message, VALIDATOR_REGISTRATION_DST, &[]);
        let sig_bytes: [u8; 96] = sig.to_bytes();

        vs.register_validator_with_sig(val, val, &pk_bytes, node_id, Some(&sig_bytes))
            .unwrap();
        assert!(vs.is_validator(val).unwrap());
    });
}

#[test]
fn registration_pop_cannot_be_replayed_on_another_chain() {
    use blst::min_pk::SecretKey;

    let val = address!("0x4747474747474747474747474747474747474747");
    let sk = SecretKey::key_gen(&[47u8; 32], &[]).unwrap();
    let pk: [u8; 48] = sk.sk_to_pk().to_bytes();
    let node_id = test_radicle_node_id(val);
    let message = validator_registration_message(1, val, node_id);
    let sig: [u8; 96] = sk
        .sign(&message, VALIDATOR_REGISTRATION_DST, &[])
        .to_bytes();

    let register_on_chain = |chain_id| {
        let mut storage = HashMapStorageProvider::new(chain_id);
        StorageHandle::enter(&mut storage, |storage| {
            let mut vs = ValidatorSet::new(storage);
            vs.config_owner.write(OWNER).unwrap();
            vs.config_max_validators.write(10).unwrap();
            vs.register_validator_with_sig(val, val, &pk, node_id, Some(&sig))
        })
    };

    register_on_chain(1).expect("proof must be valid on the chain it was created for");
    assert!(
        register_on_chain(2).is_err(),
        "registration proof from chain 1 must be rejected on chain 2"
    );
}
