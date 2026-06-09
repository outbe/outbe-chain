use alloy_primitives::{address, Address, B256};
use outbe_primitives::consensus_p2p::{
    encode_v1, P2pAddress, P2pIngress, MAX_P2P_ADDRESS_ENCODED_LEN, P2P_ADDRESS_VERSION_V1,
};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use crate::runtime::status;
use crate::schema::ValidatorSet;

const CHAIN_ID: u64 = 1;

/// Owner address used across tests.
const OWNER: Address = address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");

/// Convenience: set config_owner and config_max_validators, then run test.
fn with_vs_configured<R>(max: u32, f: impl FnOnce(&mut ValidatorSet) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.config_max_validators.write(max).unwrap();
        vs.config_epoch_length_blocks.write(10).unwrap();
        f(&mut vs)
    })
}

/// Generate a dummy 48-byte consensus pubkey with a unique seed byte.
fn dummy_consensus_pubkey(seed: u8) -> [u8; 48] {
    let mut pk = [0u8; 48];
    pk[0] = seed;
    pk
}

fn symmetric_p2p(port: u16) -> Vec<u8> {
    encode_v1(&P2pAddress::Symmetric(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
        port,
    )))
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

#[test]
fn test_activate_missing_validator_returns_revert() {
    let val_addr = address!("0x1111111111111111111111111111111111111111");

    with_vs_configured(10, |vs| {
        let err = vs.activate_validator(val_addr).unwrap_err();
        assert!(
            matches!(err, PrecompileError::Revert(message) if message == "validator not registered")
        );
    });
}

// ---------------------------------------------------------------------------
// 2. test_register_self — A-45: self-registration now requires BLS proof
// ---------------------------------------------------------------------------
#[test]
fn test_register_self_without_sig_rejected() {
    let val_addr = address!("0x2222222222222222222222222222222222222222");
    let pk = dummy_consensus_pubkey(2);

    with_vs_configured(10, |vs| {
        // A-45: Self-registration without BLS signature must fail
        let result = vs.register_validator(val_addr, val_addr, &pk);
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
        // Owner registration path — no BLS sig required
        vs.register_validator(OWNER, val_addr, &pk).unwrap();
        assert!(vs.is_validator(val_addr).unwrap());
    });
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
// 5. test_activate_deactivate
// ---------------------------------------------------------------------------
#[test]
fn test_activate_deactivate() {
    let val_addr = address!("0x4444444444444444444444444444444444444444");

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val_addr, &dummy_consensus_pubkey(4))
            .unwrap();

        // Initially REGISTERED
        assert_eq!(vs.val_status.read(&val_addr).unwrap(), status::REGISTERED);

        vs.activate_validator(val_addr).unwrap();
        assert_eq!(vs.val_status.read(&val_addr).unwrap(), status::ACTIVE);

        vs.deactivate_validator(OWNER, val_addr).unwrap();
        // In the new lifecycle, deactivation transitions to EXITING (not INACTIVE)
        assert_eq!(vs.val_status.read(&val_addr).unwrap(), status::EXITING);

        // pending_set_change should be set after deactivation
        assert!(vs.pending_set_change.read().unwrap());
    });
}

// ---------------------------------------------------------------------------
// 6. test_force_exit
// ---------------------------------------------------------------------------
#[test]
fn test_force_exit() {
    let val_addr = address!("0x5555555555555555555555555555555555555555");

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val_addr, &dummy_consensus_pubkey(5))
            .unwrap();
        vs.activate_validator(val_addr).unwrap();
        vs.val_has_bls_share.write(&val_addr, true).unwrap();

        vs.force_exit_validator(val_addr).unwrap();
        assert_eq!(vs.val_status.read(&val_addr).unwrap(), status::EXITING);
        assert_eq!(vs.val_slash_count.read(&val_addr).unwrap(), 1);
        assert!(vs.pending_set_change.read().unwrap());
    });
}

// ---------------------------------------------------------------------------
// 7. test_record_proposer
// ---------------------------------------------------------------------------
#[test]
fn test_record_proposer() {
    let val_addr = address!("0x6666666666666666666666666666666666666666");

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val_addr, &dummy_consensus_pubkey(6))
            .unwrap();
        vs.activate_validator(val_addr).unwrap();
        vs.val_has_bls_share.write(&val_addr, true).unwrap();

        assert_eq!(vs.val_blocks_proposed.read(&val_addr).unwrap(), 0);
        assert_eq!(vs.epoch_start_block.read().unwrap(), 0);

        vs.record_proposer(val_addr).unwrap();
        assert_eq!(vs.val_blocks_proposed.read(&val_addr).unwrap(), 1);
        assert_eq!(vs.epoch_start_block.read().unwrap(), 0);

        vs.record_proposer(val_addr).unwrap();
        assert_eq!(vs.val_blocks_proposed.read(&val_addr).unwrap(), 2);
        assert_eq!(vs.epoch_start_block.read().unwrap(), 0);
    });
}

// ---------------------------------------------------------------------------
// 8. test_record_participation
// ---------------------------------------------------------------------------
#[test]
fn test_record_participation() {
    let val1 = address!("0x0000000000000000000000000000000000000071");
    let val2 = address!("0x0000000000000000000000000000000000000072");
    let val3 = address!("0x0000000000000000000000000000000000000073");

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val1, &dummy_consensus_pubkey(71))
            .unwrap();
        vs.register_validator(OWNER, val2, &dummy_consensus_pubkey(72))
            .unwrap();
        vs.register_validator(OWNER, val3, &dummy_consensus_pubkey(73))
            .unwrap();
        for val in [val1, val2, val3] {
            vs.activate_validator(val).unwrap();
            vs.val_has_bls_share.write(&val, true).unwrap();
        }

        // val3 is absent
        let voters = vec![val1, val2];
        let absent = vec![val3];
        vs.record_participation(&voters, &absent).unwrap();

        assert_eq!(vs.val_missed_votes.read(&val1).unwrap(), 0);
        assert_eq!(vs.val_missed_votes.read(&val2).unwrap(), 0);
        assert_eq!(vs.val_missed_votes.read(&val3).unwrap(), 1);

        // Record again — val2 also absent this time
        let voters2 = vec![val1];
        let absent2 = vec![val2, val3];
        vs.record_participation(&voters2, &absent2).unwrap();

        assert_eq!(vs.val_missed_votes.read(&val2).unwrap(), 1);
        assert_eq!(vs.val_missed_votes.read(&val3).unwrap(), 2);
    });
}

// ---------------------------------------------------------------------------
// 8b. test_record_finalized_participation
// ---------------------------------------------------------------------------
#[test]
fn test_record_finalized_participation_accepts_historical_validators() {
    let val_active = address!("0x0000000000000000000000000000000000000081");
    let val_unbonding = address!("0x0000000000000000000000000000000000000082");

    with_vs_configured(10, |vs| {
        // Active current participant.
        vs.register_validator(OWNER, val_active, &dummy_consensus_pubkey(81))
            .unwrap();
        vs.activate_validator(val_active).unwrap();
        vs.val_has_bls_share.write(&val_active, true).unwrap();

        // Registered historical participant: status UNBONDING, no BLS share.
        // record_participation would reject this, but record_finalized_participation must accept.
        vs.register_validator(OWNER, val_unbonding, &dummy_consensus_pubkey(82))
            .unwrap();
        vs.val_status
            .write(&val_unbonding, status::UNBONDING)
            .unwrap();
        vs.val_has_bls_share.write(&val_unbonding, false).unwrap();

        // Sanity: record_participation rejects historical val_unbonding.
        // Participation/registration checks revert (not Fatal) so the error
        // message propagates instead of being masked as OutOfGas (see commit
        // c879d4e: Fatal → Revert for system/core checks).
        let err = vs
            .record_participation(&[val_active], &[val_unbonding])
            .unwrap_err();
        assert!(matches!(err, PrecompileError::Revert(_)));

        // record_finalized_participation accepts both, increments missed_votes for absent.
        vs.record_finalized_participation(&[val_active], &[val_unbonding])
            .unwrap();
        assert_eq!(vs.val_missed_votes.read(&val_active).unwrap(), 0);
        assert_eq!(vs.val_missed_votes.read(&val_unbonding).unwrap(), 1);
    });
}

#[test]
fn test_record_finalized_participation_rejects_unregistered() {
    let val = address!("0x0000000000000000000000000000000000000091");
    let stranger = address!("0x9999999999999999999999999999999999999999");

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val, &dummy_consensus_pubkey(91))
            .unwrap();

        let err = vs
            .record_finalized_participation(&[val], &[stranger])
            .unwrap_err();
        // Registration check reverts (not Fatal) so the message propagates
        // cleanly instead of being masked as OutOfGas (see commit c879d4e).
        match err {
            PrecompileError::Revert(msg) => {
                assert!(
                    msg.contains("not a registered validator"),
                    "unexpected error: {msg}"
                );
            }
            other => panic!("expected Revert, got {other:?}"),
        }
    });
}

// ---------------------------------------------------------------------------
// 9. test_update_epoch
// ---------------------------------------------------------------------------
#[test]
fn test_update_epoch() {
    let val_addr = address!("0x0000000000000000000000000000000000000091");

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val_addr, &dummy_consensus_pubkey(91))
            .unwrap();
        vs.activate_validator(val_addr).unwrap();
        vs.val_has_bls_share.write(&val_addr, true).unwrap();

        // Accumulate some stats
        vs.record_proposer(val_addr).unwrap();
        vs.record_missed_block(val_addr).unwrap();
        vs.record_participation(&[], &[val_addr]).unwrap();

        assert_eq!(vs.val_blocks_proposed.read(&val_addr).unwrap(), 1);
        assert_eq!(vs.val_missed_blocks.read(&val_addr).unwrap(), 1);
        assert_eq!(vs.val_missed_votes.read(&val_addr).unwrap(), 1);
        assert_eq!(vs.epoch_start_block.read().unwrap(), 0);
        assert_eq!(vs.epoch_number.read().unwrap(), 0);

        vs.update_epoch(5000, 77).unwrap();

        // Counters reset
        assert_eq!(vs.val_blocks_proposed.read(&val_addr).unwrap(), 0);
        assert_eq!(vs.val_missed_blocks.read(&val_addr).unwrap(), 0);
        assert_eq!(vs.val_missed_votes.read(&val_addr).unwrap(), 0);
        assert_eq!(vs.epoch_start_block.read().unwrap(), 77);

        // Epoch number incremented, timestamp and start block updated.
        assert_eq!(vs.epoch_number.read().unwrap(), 1);
        assert_eq!(vs.epoch_start_timestamp.read().unwrap(), 5000);
    });
}

#[test]
fn test_epoch_boundary_uses_block_height_not_timestamp() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let vs = ValidatorSet::new(storage.clone());
        vs.config_epoch_length_blocks.write(100).unwrap();
        vs.epoch_start_block.write(25).unwrap();
        vs.epoch_start_timestamp.write(1_000).unwrap();

        assert!(
            !crate::hooks::is_epoch_boundary(storage.clone(), 124).unwrap(),
            "block before start+length must not transition even if wall-clock advanced"
        );
        assert!(
            crate::hooks::is_epoch_boundary(storage.clone(), 125).unwrap(),
            "block at start+length must transition"
        );
    });
}

#[test]
fn test_transition_epoch_updates_start_block_and_timestamp() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let vs = ValidatorSet::new(storage.clone());
        vs.config_epoch_length_blocks.write(100).unwrap();

        crate::hooks::transition_epoch(storage.clone(), 1_234, 456).unwrap();

        let vs = ValidatorSet::new(storage);
        assert_eq!(vs.epoch_number.read().unwrap(), 1);
        assert_eq!(vs.epoch_start_timestamp.read().unwrap(), 1_234);
        assert_eq!(vs.epoch_start_block.read().unwrap(), 456);
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
        vs.activate_validator(val1).unwrap();
        vs.activate_validator(val3).unwrap();

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

// ---------------------------------------------------------------------------
// 12. test_consensus_set
// ---------------------------------------------------------------------------
#[test]
fn test_consensus_set() {
    let val1 = address!("0x00000000000000000000000000000000000000C1");
    let val2 = address!("0x00000000000000000000000000000000000000C2");
    let val3 = address!("0x00000000000000000000000000000000000000C3");
    let group_key = B256::with_last_byte(0xFF);

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val1, &dummy_consensus_pubkey(0xC1))
            .unwrap();
        vs.register_validator(OWNER, val2, &dummy_consensus_pubkey(0xC2))
            .unwrap();
        vs.register_validator(OWNER, val3, &dummy_consensus_pubkey(0xC3))
            .unwrap();

        // All start as REGISTERED
        assert_eq!(vs.val_status.read(&val1).unwrap(), status::REGISTERED);
        assert_eq!(vs.val_status.read(&val2).unwrap(), status::REGISTERED);

        // Activate reshared set with val1 and val2 (not val3)
        vs.activate_reshared_set(&[val1, val2], group_key).unwrap();

        // val1 and val2 should be ACTIVE with has_bls_share
        assert_eq!(vs.val_status.read(&val1).unwrap(), status::ACTIVE);
        assert_eq!(vs.val_status.read(&val2).unwrap(), status::ACTIVE);
        assert!(vs.val_has_bls_share.read(&val1).unwrap());
        assert!(vs.val_has_bls_share.read(&val2).unwrap());

        // val3 remains REGISTERED, no BLS share
        assert_eq!(vs.val_status.read(&val3).unwrap(), status::REGISTERED);
        assert!(!vs.val_has_bls_share.read(&val3).unwrap());

        // Consensus set contains only val1 and val2
        let consensus_set = vs.get_active_consensus_set().unwrap();
        assert_eq!(consensus_set.len(), 2);
        assert_eq!(vs.active_consensus_count().unwrap(), 2);

        // pending_set_change should be cleared
        assert!(!vs.pending_set_change.read().unwrap());

        // is_consensus_participant checks
        assert!(vs.is_consensus_participant(val1).unwrap());
        assert!(vs.is_consensus_participant(val2).unwrap());
        assert!(!vs.is_consensus_participant(val3).unwrap());
    });
}

// ---------------------------------------------------------------------------
// 13. test_exiting_to_unbonding_via_reshare
// ---------------------------------------------------------------------------
#[test]
fn test_exiting_to_unbonding_via_reshare() {
    let val1 = address!("0x00000000000000000000000000000000000000D1");
    let val2 = address!("0x00000000000000000000000000000000000000D2");
    let group_key = B256::with_last_byte(0xFE);

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val1, &dummy_consensus_pubkey(0xD1))
            .unwrap();
        vs.register_validator(OWNER, val2, &dummy_consensus_pubkey(0xD2))
            .unwrap();

        // First reshare: both active
        vs.activate_reshared_set(&[val1, val2], group_key).unwrap();
        assert_eq!(vs.val_status.read(&val1).unwrap(), status::ACTIVE);
        assert_eq!(vs.val_status.read(&val2).unwrap(), status::ACTIVE);

        // val2 requests deactivation → EXITING
        vs.deactivate_validator(OWNER, val2).unwrap();
        assert_eq!(vs.val_status.read(&val2).unwrap(), status::EXITING);

        // Second reshare: only val1 in new set
        let group_key2 = B256::with_last_byte(0xFD);
        vs.activate_reshared_set(&[val1], group_key2).unwrap();

        // val1 still ACTIVE with BLS share
        assert_eq!(vs.val_status.read(&val1).unwrap(), status::ACTIVE);
        assert!(vs.val_has_bls_share.read(&val1).unwrap());

        // val2 transitioned from EXITING → UNBONDING, no BLS share
        assert_eq!(vs.val_status.read(&val2).unwrap(), status::UNBONDING);
        assert!(!vs.val_has_bls_share.read(&val2).unwrap());
    });
}

#[test]
fn test_deactivated_validator_stays_current_consensus_participant_until_reshare() {
    let val1 = address!("0x0000000000000000000000000000000000000CD1");
    let val2 = address!("0x0000000000000000000000000000000000000CD2");

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val1, &dummy_consensus_pubkey(0xD1))
            .unwrap();
        vs.register_validator(OWNER, val2, &dummy_consensus_pubkey(0xD2))
            .unwrap();
        vs.activate_reshared_set(&[val1, val2], B256::with_last_byte(0xD1))
            .unwrap();

        vs.deactivate_validator(OWNER, val2).unwrap();

        assert_eq!(vs.val_status.read(&val2).unwrap(), status::EXITING);
        assert!(vs.val_has_bls_share.read(&val2).unwrap());
        assert!(vs.is_consensus_participant(val2).unwrap());
        assert_eq!(vs.active_consensus_count().unwrap(), 2);

        let current_set = vs.get_active_consensus_set().unwrap();
        let current_addrs: Vec<_> = current_set.iter().map(|v| v.validator_address).collect();
        assert!(current_addrs.contains(&val1));
        assert!(current_addrs.contains(&val2));

        vs.record_proposer(val2).unwrap();
        vs.record_participation(&[val1], &[val2]).unwrap();
        assert_eq!(vs.val_blocks_proposed.read(&val2).unwrap(), 1);
        assert_eq!(vs.val_missed_votes.read(&val2).unwrap(), 1);

        vs.activate_reshared_set(&[val1], B256::with_last_byte(0xD2))
            .unwrap();
        assert_eq!(vs.val_status.read(&val2).unwrap(), status::UNBONDING);
        assert!(!vs.val_has_bls_share.read(&val2).unwrap());
        assert!(!vs.is_consensus_participant(val2).unwrap());
    });
}

#[test]
fn test_force_exited_validator_stays_current_consensus_participant_until_reshare() {
    let val1 = address!("0x0000000000000000000000000000000000000CF1");
    let val2 = address!("0x0000000000000000000000000000000000000CF2");

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val1, &dummy_consensus_pubkey(0xF1))
            .unwrap();
        vs.register_validator(OWNER, val2, &dummy_consensus_pubkey(0xF2))
            .unwrap();
        vs.activate_reshared_set(&[val1, val2], B256::with_last_byte(0xF1))
            .unwrap();

        vs.force_exit_validator(val2).unwrap();

        assert_eq!(vs.val_status.read(&val2).unwrap(), status::EXITING);
        assert!(vs.val_has_bls_share.read(&val2).unwrap());
        assert!(vs.is_consensus_participant(val2).unwrap());
        assert_eq!(vs.val_slash_count.read(&val2).unwrap(), 1);

        vs.record_proposer(val2).unwrap();
        vs.record_participation(&[val1], &[val2]).unwrap();

        vs.force_exit_validator(val2).unwrap();
        assert!(vs.val_has_bls_share.read(&val2).unwrap());
        assert!(vs.is_consensus_participant(val2).unwrap());
        assert_eq!(vs.val_slash_count.read(&val2).unwrap(), 2);

        vs.activate_reshared_set(&[val1], B256::with_last_byte(0xF2))
            .unwrap();
        assert_eq!(vs.val_status.read(&val2).unwrap(), status::UNBONDING);
        assert!(!vs.is_consensus_participant(val2).unwrap());
    });
}

// ---------------------------------------------------------------------------
// 14. test_pending_set_change
// ---------------------------------------------------------------------------
#[test]
fn test_pending_set_change() {
    let val_addr = address!("0x00000000000000000000000000000000000000E1");

    with_vs_configured(10, |vs| {
        // Initially no pending change
        assert!(!vs.has_pending_set_change().unwrap());

        // Registration triggers pending_set_change
        vs.register_validator(OWNER, val_addr, &dummy_consensus_pubkey(0xE1))
            .unwrap();
        assert!(vs.has_pending_set_change().unwrap());

        // activateResharedSet clears it
        let group_key = B256::with_last_byte(0xEE);
        vs.activate_reshared_set(&[val_addr], group_key).unwrap();
        assert!(!vs.has_pending_set_change().unwrap());

        // Forced exit triggers pending_set_change
        vs.force_exit_validator(val_addr).unwrap();
        assert!(vs.has_pending_set_change().unwrap());
    });
}

// ---------------------------------------------------------------------------
// 14b. test_pending_set_change_missed_validator
// ---------------------------------------------------------------------------
#[test]
fn test_pending_set_change_missed_validator() {
    let val1 = address!("0x00000000000000000000000000000000000000A1");
    let val2 = address!("0x00000000000000000000000000000000000000A2");
    let val3 = address!("0x00000000000000000000000000000000000000A3");
    let group_key1 = B256::with_last_byte(0xAA);
    let group_key2 = B256::with_last_byte(0xBB);

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val1, &dummy_consensus_pubkey(0xA1))
            .unwrap();
        vs.register_validator(OWNER, val2, &dummy_consensus_pubkey(0xA2))
            .unwrap();
        vs.register_validator(OWNER, val3, &dummy_consensus_pubkey(0xA3))
            .unwrap();

        // First reshare: all 3 validators participate → all ACTIVE
        vs.activate_reshared_set(&[val1, val2, val3], group_key1)
            .unwrap();
        assert_eq!(vs.val_status.read(&val1).unwrap(), status::ACTIVE);
        assert_eq!(vs.val_status.read(&val2).unwrap(), status::ACTIVE);
        assert_eq!(vs.val_status.read(&val3).unwrap(), status::ACTIVE);
        // All covered → pending cleared
        assert!(!vs.has_pending_set_change().unwrap());

        // Second reshare: val3 missed the ceremony → only val1, val2 in new set
        vs.activate_reshared_set(&[val1, val2], group_key2).unwrap();

        // val1 and val2 have shares
        assert!(vs.val_has_bls_share.read(&val1).unwrap());
        assert!(vs.val_has_bls_share.read(&val2).unwrap());

        // val3 is still ACTIVE but has NO share (missed ceremony)
        assert_eq!(vs.val_status.read(&val3).unwrap(), status::ACTIVE);
        assert!(!vs.val_has_bls_share.read(&val3).unwrap());

        // pending_set_change remains true — triggers another reshare
        assert!(vs.has_pending_set_change().unwrap());

        // Third reshare: all 3 participate again → pending cleared
        let group_key3 = B256::with_last_byte(0xCC);
        vs.activate_reshared_set(&[val1, val2, val3], group_key3)
            .unwrap();
        assert!(!vs.has_pending_set_change().unwrap());
        assert!(vs.val_has_bls_share.read(&val3).unwrap());
    });
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
        vs.activate_validator(val_addr).unwrap();
        vs.val_status.write(&val_addr, status::INACTIVE).unwrap();

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
        vs.activate_validator(val_addr).unwrap();

        // Re-registration of ACTIVE validator must fail
        let result = vs.register_validator(OWNER, val_addr, &dummy_consensus_pubkey(0x22));
        assert!(result.is_err());
    });
}

// ---------------------------------------------------------------------------
// 19. test_forced_exit_preserves_staking_lifecycle
// ---------------------------------------------------------------------------
#[test]
fn test_forced_exit_preserves_staking_lifecycle() {
    use alloy_primitives::U256;
    let val_addr = address!("0x3333333333333333333333333333333333333333");

    with_vs_configured(10, |vs| {
        vs.config_min_stake.write(U256::from(1000u64)).unwrap();

        vs.register_validator(OWNER, val_addr, &dummy_consensus_pubkey(0x33))
            .unwrap();
        vs.activate_validator(val_addr).unwrap();

        // Set stake above min then force exit.
        vs.val_stake.write(&val_addr, U256::from(1000u64)).unwrap();
        vs.force_exit_validator(val_addr).unwrap();
        assert_eq!(vs.val_status.read(&val_addr).unwrap(), status::EXITING);

        // Simulate slash reducing stake below min_stake
        vs.val_stake.write(&val_addr, U256::from(500u64)).unwrap();

        // Forced exit never returns through REGISTERED. Staking moves
        // UNBONDING validators to INACTIVE after withdrawability completes.
        assert_eq!(vs.val_status.read(&val_addr).unwrap(), status::EXITING);
    });
}

// ---------------------------------------------------------------------------
// 20. test_cleanup_inactive_validators
// ---------------------------------------------------------------------------
#[test]
fn test_cleanup_inactive_validators() {
    let val1 = address!("0x00000000000000000000000000000000000000A1");
    let val2 = address!("0x00000000000000000000000000000000000000A2");
    let val3 = address!("0x00000000000000000000000000000000000000A3");
    let val4 = address!("0x00000000000000000000000000000000000000A4");
    let val5 = address!("0x00000000000000000000000000000000000000A5");

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val1, &dummy_consensus_pubkey(0xA1))
            .unwrap();
        vs.register_validator(OWNER, val2, &dummy_consensus_pubkey(0xA2))
            .unwrap();
        vs.register_validator(OWNER, val3, &dummy_consensus_pubkey(0xA3))
            .unwrap();
        vs.register_validator(OWNER, val4, &dummy_consensus_pubkey(0xA4))
            .unwrap();
        vs.register_validator(OWNER, val5, &dummy_consensus_pubkey(0xA5))
            .unwrap();
        assert_eq!(vs.validator_count.read().unwrap(), 5);

        // Mark val2 and val4 as INACTIVE
        vs.val_status.write(&val2, status::INACTIVE).unwrap();
        vs.val_status.write(&val4, status::INACTIVE).unwrap();

        // Cleanup all INACTIVE entries
        let removed = vs.cleanup_inactive_validators(0).unwrap();
        assert_eq!(removed, 2);
        assert_eq!(vs.validator_count.read().unwrap(), 3);

        // Cleaned-up validators have index 0
        assert_eq!(vs.address_to_index.read(&val2).unwrap(), 0);
        assert_eq!(vs.address_to_index.read(&val4).unwrap(), 0);

        // Remaining validators are still accessible
        assert!(vs.address_to_index.read(&val1).unwrap() > 0);
        assert!(vs.address_to_index.read(&val3).unwrap() > 0);
        assert!(vs.address_to_index.read(&val5).unwrap() > 0);
    });
}

// ---------------------------------------------------------------------------
// 21. test_cleanup_capped
// ---------------------------------------------------------------------------
#[test]
fn test_cleanup_capped() {
    let val1 = address!("0x00000000000000000000000000000000000000B1");
    let val2 = address!("0x00000000000000000000000000000000000000B2");
    let val3 = address!("0x00000000000000000000000000000000000000B3");

    with_vs_configured(10, |vs| {
        vs.register_validator(OWNER, val1, &dummy_consensus_pubkey(0xB1))
            .unwrap();
        vs.register_validator(OWNER, val2, &dummy_consensus_pubkey(0xB2))
            .unwrap();
        vs.register_validator(OWNER, val3, &dummy_consensus_pubkey(0xB3))
            .unwrap();

        // Mark all as INACTIVE
        vs.val_status.write(&val1, status::INACTIVE).unwrap();
        vs.val_status.write(&val2, status::INACTIVE).unwrap();
        vs.val_status.write(&val3, status::INACTIVE).unwrap();

        // Cap at 2
        let removed = vs.cleanup_inactive_validators(2).unwrap();
        assert_eq!(removed, 2);
        assert_eq!(vs.validator_count.read().unwrap(), 1);

        // Second call gets the remaining one
        let removed2 = vs.cleanup_inactive_validators(2).unwrap();
        assert_eq!(removed2, 1);
        assert_eq!(vs.validator_count.read().unwrap(), 0);
    });
}

// ---------------------------------------------------------------------------
// P2-3: Re-registration cooldown tests
// ---------------------------------------------------------------------------

#[test]
fn test_reregistration_cooldown_blocks() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_block_number(100);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.config_max_validators.write(128).unwrap();
        vs.config_reregistration_cooldown.write(1000).unwrap(); // 1000 block cooldown

        let val = address!("0x1111111111111111111111111111111111111111");
        let pk = dummy_consensus_pubkey(0xCC);

        // Register and transition to INACTIVE with deactivated_at_height = 100
        vs.register_validator(OWNER, val, &pk).unwrap();
        vs.activate_validator(val).unwrap();
        vs.val_status.write(&val, status::INACTIVE).unwrap();
        vs.val_deactivated_at_height.write(&val, 100).unwrap();

        // Try re-register at block 500 (only 400 blocks passed, need 1000)
        // We need to simulate block_number = 500
    });

    storage.set_block_number(500);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = ValidatorSet::new(storage.clone());
        let val = address!("0x1111111111111111111111111111111111111111");
        let pk_new = dummy_consensus_pubkey(0xDD);

        let result = vs.register_validator(OWNER, val, &pk_new);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("cooldown"),
            "error should mention cooldown"
        );
    });
}

#[test]
fn test_reregistration_after_cooldown() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_block_number(100);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.config_max_validators.write(128).unwrap();
        vs.config_reregistration_cooldown.write(1000).unwrap();

        let val = address!("0x2222222222222222222222222222222222222222");
        let pk = dummy_consensus_pubkey(0xEE);

        vs.register_validator(OWNER, val, &pk).unwrap();
        vs.activate_validator(val).unwrap();
        vs.val_status.write(&val, status::INACTIVE).unwrap();
        vs.val_deactivated_at_height.write(&val, 100).unwrap();
    });

    // Advance past cooldown: block 100 + 1000 = 1100
    storage.set_block_number(1100);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = ValidatorSet::new(storage.clone());
        let val = address!("0x2222222222222222222222222222222222222222");
        let pk_new = dummy_consensus_pubkey(0xFF);

        // Should succeed — cooldown expired
        vs.register_validator(OWNER, val, &pk_new).unwrap();
        assert_eq!(vs.val_status.read(&val).unwrap(), status::REGISTERED);
    });
}

#[test]
fn test_reregistration_no_cooldown_configured() {
    with_vs_configured(128, |vs| {
        let val = address!("0x3333333333333333333333333333333333333333");
        let pk = dummy_consensus_pubkey(0xAA);

        // cooldown = 0 (default)
        assert_eq!(vs.config_reregistration_cooldown.read().unwrap(), 0);

        vs.register_validator(OWNER, val, &pk).unwrap();
        vs.activate_validator(val).unwrap();
        vs.val_status.write(&val, status::INACTIVE).unwrap();
        vs.val_deactivated_at_height.write(&val, 50).unwrap();

        // Re-register immediately — should succeed (no cooldown)
        let pk_new = dummy_consensus_pubkey(0xBB);
        vs.register_validator(OWNER, val, &pk_new).unwrap();
        assert_eq!(vs.val_status.read(&val).unwrap(), status::REGISTERED);
    });
}

// ---------------------------------------------------------------------------
// Task 04: validator join race tests
// ---------------------------------------------------------------------------

#[test]
fn test_activate_validator_sets_pending_set_change() {
    // register → reshare (without new validator) → stake triggers activate_validator
    // which must set pending_set_change = true so consensus picks up the new participant.
    with_vs_configured(128, |vs| {
        let val = address!("0x1111111111111111111111111111111111111111");
        let pk = dummy_consensus_pubkey(0x01);

        // Register → REGISTERED, pending_set_change = true
        vs.register_validator(OWNER, val, &pk).unwrap();
        assert!(vs.has_pending_set_change().unwrap());

        // Simulate reshare completed (without this validator, they had no stake).
        // activate_reshared_set with empty set → clears pending.
        vs.activate_reshared_set(&[], B256::with_last_byte(0x01))
            .unwrap();
        assert!(!vs.has_pending_set_change().unwrap());

        // Now activate_validator (from staking auto-activate) → must set pending again.
        vs.activate_validator(val).unwrap();
        assert_eq!(vs.val_status.read(&val).unwrap(), status::ACTIVE);
        assert!(
            vs.has_pending_set_change().unwrap(),
            "activate_validator must set pending_set_change = true"
        );
    });
}

#[test]
fn test_activate_reshared_set_clears_pending_after_join() {
    // After a new validator is activated and reshare includes them,
    // pending_set_change should be cleared.
    with_vs_configured(128, |vs| {
        let val = address!("0x1111111111111111111111111111111111111111");
        let pk = dummy_consensus_pubkey(0x01);

        vs.register_validator(OWNER, val, &pk).unwrap();
        vs.activate_validator(val).unwrap();
        assert!(vs.has_pending_set_change().unwrap());

        // Reshare now includes the new validator.
        vs.activate_reshared_set(&[val], B256::with_last_byte(0x02))
            .unwrap();
        assert!(
            !vs.has_pending_set_change().unwrap(),
            "pending should be cleared after reshare includes all active validators"
        );
    });
}

#[test]
fn test_already_active_validator_does_not_raise_pending() {
    // Calling activate_validator on an already-ACTIVE validator is a no-op.
    with_vs_configured(128, |vs| {
        let val = address!("0x1111111111111111111111111111111111111111");
        let pk = dummy_consensus_pubkey(0x01);

        vs.register_validator(OWNER, val, &pk).unwrap();
        vs.activate_validator(val).unwrap();

        // Clear pending by completing reshare.
        vs.activate_reshared_set(&[val], B256::with_last_byte(0x01))
            .unwrap();
        assert!(!vs.has_pending_set_change().unwrap());

        // Calling activate_validator again should NOT re-raise pending.
        vs.activate_validator(val).unwrap();
        assert!(
            !vs.has_pending_set_change().unwrap(),
            "already-active validator should not trigger spurious pending_set_change"
        );
    });
}

// ===========================================================================
// A-09: Forced-exit validator status guard tests
// ===========================================================================

#[test]
fn test_force_exit_active_succeeds() {
    with_vs_configured(10, |vs| {
        let val = address!("0x0909090909090909090909090909090909090909");
        vs.register_validator(OWNER, val, &dummy_consensus_pubkey(90))
            .unwrap();
        vs.activate_validator(val).unwrap();
        vs.force_exit_validator(val).unwrap();
        assert_eq!(vs.val_status.read(&val).unwrap(), status::EXITING);
    });
}

#[test]
fn test_force_exit_exiting_succeeds() {
    with_vs_configured(10, |vs| {
        let val = address!("0x0909090909090909090909090909090909090909");
        vs.register_validator(OWNER, val, &dummy_consensus_pubkey(91))
            .unwrap();
        vs.activate_validator(val).unwrap();
        vs.val_status.write(&val, status::EXITING).unwrap();
        vs.force_exit_validator(val).unwrap();
        assert_eq!(vs.val_status.read(&val).unwrap(), status::EXITING);
    });
}

#[test]
fn test_repeated_force_exit_remains_exiting() {
    with_vs_configured(10, |vs| {
        let val = address!("0x0909090909090909090909090909090909090909");
        vs.register_validator(OWNER, val, &dummy_consensus_pubkey(92))
            .unwrap();
        vs.activate_validator(val).unwrap();
        vs.force_exit_validator(val).unwrap();
        vs.force_exit_validator(val).unwrap();
        assert_eq!(vs.val_status.read(&val).unwrap(), status::EXITING);
        assert_eq!(vs.val_slash_count.read(&val).unwrap(), 2);
    });
}

#[test]
fn test_force_exit_registered_rejected() {
    with_vs_configured(10, |vs| {
        let val = address!("0x0909090909090909090909090909090909090909");
        vs.register_validator(OWNER, val, &dummy_consensus_pubkey(93))
            .unwrap();
        assert!(vs.force_exit_validator(val).is_err());
    });
}

#[test]
fn test_force_exit_unbonding_idempotent() {
    with_vs_configured(10, |vs| {
        let val = address!("0x0909090909090909090909090909090909090909");
        vs.register_validator(OWNER, val, &dummy_consensus_pubkey(94))
            .unwrap();
        vs.val_status.write(&val, status::UNBONDING).unwrap();
        assert!(vs.force_exit_validator(val).is_ok());
        assert_eq!(vs.val_status.read(&val).unwrap(), status::UNBONDING);
    });
}

#[test]
fn test_force_exit_inactive_idempotent() {
    with_vs_configured(10, |vs| {
        let val = address!("0x0909090909090909090909090909090909090909");
        vs.register_validator(OWNER, val, &dummy_consensus_pubkey(95))
            .unwrap();
        vs.val_status.write(&val, status::INACTIVE).unwrap();
        assert!(vs.force_exit_validator(val).is_ok());
        assert_eq!(vs.val_status.read(&val).unwrap(), status::INACTIVE);
    });
}

// ===========================================================================
// A-18: BLS pubkey uniqueness tests
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
// A-21: Activate validator status guard tests
// ===========================================================================

#[test]
fn test_activate_forced_exiting_rejected() {
    with_vs_configured(10, |vs| {
        let val = address!("0x2121212121212121212121212121212121212121");
        vs.register_validator(OWNER, val, &dummy_consensus_pubkey(21))
            .unwrap();
        vs.activate_validator(val).unwrap();
        vs.force_exit_validator(val).unwrap();
        let result = vs.activate_validator(val);
        assert!(result.is_err(), "exiting validator must not be activated");
    });
}

#[test]
fn test_activate_exiting_rejected() {
    with_vs_configured(10, |vs| {
        let val = address!("0x2121212121212121212121212121212121212121");
        vs.register_validator(OWNER, val, &dummy_consensus_pubkey(22))
            .unwrap();
        vs.activate_validator(val).unwrap();
        vs.val_status.write(&val, status::EXITING).unwrap();
        let result = vs.activate_validator(val);
        assert!(result.is_err(), "exiting validator must not be activated");
    });
}

#[test]
fn test_activate_unbonding_rejected() {
    with_vs_configured(10, |vs| {
        let val = address!("0x2121212121212121212121212121212121212121");
        vs.register_validator(OWNER, val, &dummy_consensus_pubkey(23))
            .unwrap();
        vs.val_status.write(&val, status::UNBONDING).unwrap();
        let result = vs.activate_validator(val);
        assert!(result.is_err(), "unbonding validator must not be activated");
    });
}

#[test]
fn test_activate_inactive_rejected() {
    with_vs_configured(10, |vs| {
        let val = address!("0x2121212121212121212121212121212121212121");
        vs.register_validator(OWNER, val, &dummy_consensus_pubkey(24))
            .unwrap();
        vs.val_status.write(&val, status::INACTIVE).unwrap();
        let result = vs.activate_validator(val);
        assert!(result.is_err(), "inactive validator must not be activated");
    });
}

// ===========================================================================
// A-44: EXITING validators get per-epoch counters reset
// ===========================================================================

#[test]
fn test_epoch_reset_includes_exiting() {
    with_vs_configured(10, |vs| {
        let val = address!("0x4444444444444444444444444444444444444444");
        vs.register_validator(OWNER, val, &dummy_consensus_pubkey(44))
            .unwrap();
        vs.activate_validator(val).unwrap();

        // Accumulate counters then transition to EXITING
        vs.val_missed_blocks.write(&val, 10).unwrap();
        vs.val_missed_votes.write(&val, 5).unwrap();
        vs.val_blocks_proposed.write(&val, 3).unwrap();
        vs.val_status.write(&val, status::EXITING).unwrap();

        // Epoch transition should reset counters even for EXITING
        vs.update_epoch(1000, 42).unwrap();

        assert_eq!(vs.val_missed_blocks.read(&val).unwrap(), 0);
        assert_eq!(vs.val_missed_votes.read(&val).unwrap(), 0);
        assert_eq!(vs.val_blocks_proposed.read(&val).unwrap(), 0);
    });
}

// ===========================================================================
// A-45: Invalid BLS signature rejected for self-registration
// ===========================================================================

#[test]
fn test_register_self_invalid_sig_rejected() {
    with_vs_configured(10, |vs| {
        let val = address!("0x4545454545454545454545454545454545454545");
        let pk = dummy_consensus_pubkey(45);
        let bad_sig = [0xFFu8; 96]; // garbage signature

        let result = vs.register_validator_with_sig(val, val, &pk, Some(&bad_sig));
        assert!(result.is_err(), "invalid BLS sig must be rejected");
    });
}

/// A-45: Valid self-registration with correct BLS signature succeeds.
#[test]
fn test_register_self_valid_sig_accepted() {
    use blst::min_pk::SecretKey;

    with_vs_configured(10, |vs| {
        let val = address!("0x4646464646464646464646464646464646464646");
        let ikm = [46u8; 32];
        let sk = SecretKey::key_gen(&ikm, &[]).unwrap();
        let pk = sk.sk_to_pk();
        let pk_bytes: [u8; 48] = pk.to_bytes();

        // Sign validator address with registration DST
        let dst = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_NUL_outbe_REGISTER";
        let sig = sk.sign(val.as_slice(), dst, &[]);
        let sig_bytes: [u8; 96] = sig.to_bytes();

        vs.register_validator_with_sig(val, val, &pk_bytes, Some(&sig_bytes))
            .unwrap();
        assert!(vs.is_validator(val).unwrap());
    });
}

// ---- Step 8: idempotent record_finalized_participation hook tests --------

mod record_finalized_participation_idempotency {
    use super::*;
    use crate::hooks;
    use alloy_primitives::b256;

    const FB_HASH_A: B256 =
        b256!("0x1111111111111111111111111111111111111111111111111111111111111111");
    const FB_HASH_B: B256 =
        b256!("0x2222222222222222222222222222222222222222222222222222222222222222");

    fn dummy_consensus_pubkey_local(seed: u8) -> [u8; 48] {
        let mut pk = [0u8; 48];
        pk[0] = seed;
        pk
    }

    fn register_active(vs: &mut ValidatorSet, addr: Address, seed: u8) {
        vs.register_validator(OWNER, addr, &dummy_consensus_pubkey_local(seed))
            .unwrap();
        vs.activate_validator(addr).unwrap();
        vs.val_has_bls_share.write(&addr, true).unwrap();
    }

    #[test]
    fn replay_for_same_fb_hash_is_noop() {
        let val_a = address!("0x00000000000000000000000000000000000000A1");
        let val_b = address!("0x00000000000000000000000000000000000000B2");
        with_vs_configured(10, |vs| {
            register_active(vs, val_a, 1);
            register_active(vs, val_b, 2);

            let storage = vs.storage.clone();
            // First call: increments missed_votes for absent val_b.
            hooks::record_finalized_participation(storage.clone(), FB_HASH_A, &[val_a], &[val_b])
                .unwrap();
            assert_eq!(vs.val_missed_votes.read(&val_b).unwrap(), 1);

            // Replay same fb_hash: must not bump again.
            hooks::record_finalized_participation(storage.clone(), FB_HASH_A, &[val_a], &[val_b])
                .unwrap();
            assert_eq!(vs.val_missed_votes.read(&val_b).unwrap(), 1);

            // Triple replay: still 1.
            hooks::record_finalized_participation(storage.clone(), FB_HASH_A, &[val_a], &[val_b])
                .unwrap();
            assert_eq!(vs.val_missed_votes.read(&val_b).unwrap(), 1);
        });
    }

    #[test]
    fn different_fb_hash_increments_independently() {
        let val_a = address!("0x00000000000000000000000000000000000000A1");
        let val_b = address!("0x00000000000000000000000000000000000000B2");
        with_vs_configured(10, |vs| {
            register_active(vs, val_a, 1);
            register_active(vs, val_b, 2);
            let storage = vs.storage.clone();

            hooks::record_finalized_participation(storage.clone(), FB_HASH_A, &[val_a], &[val_b])
                .unwrap();
            hooks::record_finalized_participation(storage.clone(), FB_HASH_B, &[val_a], &[val_b])
                .unwrap();

            assert_eq!(
                vs.val_missed_votes.read(&val_b).unwrap(),
                2,
                "two distinct finalized blocks count independently"
            );
        });
    }

    #[test]
    fn empty_voters_and_absent_is_noop() {
        with_vs_configured(10, |vs| {
            let storage = vs.storage.clone();
            hooks::record_finalized_participation(storage, FB_HASH_A, &[], &[]).unwrap();
            assert!(!vs
                .finalized_participation_recorded
                .read(&FB_HASH_A)
                .unwrap());
        });
    }
}
