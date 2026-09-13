use super::*;
use crate::hooks;
use alloy_primitives::b256;

const FB_HASH_A: B256 = b256!("0x1111111111111111111111111111111111111111111111111111111111111111");
const FB_HASH_B: B256 = b256!("0x2222222222222222222222222222222222222222222222222222222222222222");

fn dummy_consensus_pubkey_local(seed: u8) -> [u8; 48] {
    let mut pk = [0u8; 48];
    pk[0] = seed;
    pk
}

fn register_active(vs: &mut ValidatorSet, addr: Address, seed: u8) {
    vs.register_validator(OWNER, addr, &dummy_consensus_pubkey_local(seed))
        .unwrap();
    vs.activate_validator_via_boundary_for_test(addr).unwrap();
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
