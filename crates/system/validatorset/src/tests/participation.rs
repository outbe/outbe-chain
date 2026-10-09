use super::*;
use outbe_primitives::storage::finalized_guard_ring::{test_ring_hash, RingPosition};
use outbe_primitives::storage::hashmap::MutationPrefixViews;

// ---------------------------------------------------------------------------
// 7. test_record_proposer
// ---------------------------------------------------------------------------
#[test]
fn test_record_proposer() {
    let val_addr = address!("0x6666666666666666666666666666666666666666");

    with_vs_configured(10, |vs| {
        register_participant(vs, val_addr, 6).unwrap();

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
        register_validators(vs, &[(val1, 71), (val2, 72), (val3, 73)]).unwrap();
        for val in [val1, val2, val3] {
            vs.activate_validator_via_boundary_for_test(val).unwrap();
            vs.val_has_bls_share.write(&val, true).unwrap();
        }

        // val3 is absent
        let voters = vec![val1, val2];
        let absent = vec![val3];
        vs.record_participation(&voters, &absent).unwrap();

        assert_eq!(vs.val_missed_votes.read(&val1).unwrap(), 0);
        assert_eq!(vs.val_missed_votes.read(&val2).unwrap(), 0);
        assert_eq!(vs.val_missed_votes.read(&val3).unwrap(), 1);

        // Record again - val2 also absent this time
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
        register_participant(vs, val_active, 81).unwrap();

        // Registered historical participant: canonically exit the live set to
        // UNBONDING. record_participation rejects it, while finalized-parent
        // accounting still accepts its retained registry/history record.
        register_validators(vs, &[(val_unbonding, 82)]).unwrap();
        activate_for_test(vs, val_unbonding);
        vs.deactivate_validator(OWNER, val_unbonding).unwrap();
        vs.activate_reshared_set(&[val_active], B256::ZERO).unwrap();

        // Sanity: record_participation rejects historical val_unbonding.
        // Participation/registration checks revert (not Fatal) so the error
        // message propagates instead of being masked as OutOfGas (see commit
        // c879d4e: Fatal -> Revert for system/core checks).
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
        register_validators(vs, &[(val, 91)]).unwrap();

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

// ===========================================================================
// EXITING validators get per-epoch counters reset
// ===========================================================================

#[test]
fn test_epoch_reset_includes_exiting() {
    with_vs_configured(10, |vs| {
        let val = address!("0x4444444444444444444444444444444444444444");
        register_boundary_active(vs, val, 44).unwrap();

        // Accumulate counters then transition to EXITING
        vs.val_missed_blocks.write(&val, 10).unwrap();
        vs.val_missed_votes.write(&val, 5).unwrap();
        vs.val_blocks_proposed.write(&val, 3).unwrap();
        vs.deactivate_validator(OWNER, val).unwrap();

        // Epoch transition should reset counters even for EXITING
        vs.update_epoch(1000, 42).unwrap();

        assert_eq!(vs.val_missed_blocks.read(&val).unwrap(), 0);
        assert_eq!(vs.val_missed_votes.read(&val).unwrap(), 0);
        assert_eq!(vs.val_blocks_proposed.read(&val).unwrap(), 0);
    });
}

#[test]
fn finalized_participation_guard_prune_ring_bounds_growth() {
    use outbe_primitives::storage::StorageHandle;
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        let v = address!("0x0101010101010101010101010101010101010101");
        register_boundary_active(&mut vs, v, 0x01).unwrap();

        let retain = crate::hooks::FINALIZED_PARTICIPATION_RETAIN;
        let total = retain + 3;
        let hashes: Vec<B256> = (0..total)
            .map(|i| B256::with_last_byte((i + 1) as u8))
            .collect();
        for h in &hashes {
            crate::hooks::record_finalized_participation(storage.clone(), *h, &[v], &[]).unwrap();
        }
        // The oldest (total - retain) guard flags are evicted (slots reclaimed).
        // The last `retain` finalized blocks are still guarded against replay.
        for i in 0..(total - retain) {
            assert!(
                !vs.finalized_participation_recorded
                    .read(&hashes[i as usize])
                    .unwrap(),
                "guard entry {i} must be pruned"
            );
        }
        for i in (total - retain)..total {
            assert!(
                vs.finalized_participation_recorded
                    .read(&hashes[i as usize])
                    .unwrap(),
                "guard entry {i} must be retained"
            );
        }
    });
}

/// The ACTIVE validator that misses every finalized block of the
/// participation-ring characterization tests.
const RING_ABSENTEE: Address = address!("0x0101010101010101010101010101010101010101");

/// The absentee's missed votes, the replay guards of the recorded and the
/// evicted block and the ring position.
#[derive(Debug, PartialEq)]
struct ParticipationRingView {
    missed_votes: u64,
    recorded_guard: bool,
    evicted_guard: bool,
    ring: RingPosition,
}

/// Storage with [`RING_ABSENTEE`] ACTIVE, the ring cursor `seq`, the entry at
/// `seq % RETAIN` and the replay guard of every block in `guarded`.
fn seeded_participation_ring(seq: u64, entry: B256, guarded: &[B256]) -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.enter(|storage| {
        let mut vs = ValidatorSet::new(storage);
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(8).unwrap();
        register_boundary_active(&mut vs, RING_ABSENTEE, 0x01).unwrap();
        vs.participation_prune_ring()
            .seed_for_test(seq, entry)
            .unwrap();
        for hash in guarded {
            vs.finalized_participation_recorded
                .write(hash, true)
                .unwrap();
        }
    });
    provider
}

fn participation_ring_view(
    provider: &mut HashMapStorageProvider,
    recorded: B256,
    evicted: B256,
    idx: u64,
) -> ParticipationRingView {
    provider.enter(|storage| {
        let vs = ValidatorSet::new(storage);
        ParticipationRingView {
            missed_votes: vs.val_missed_votes.read(&RING_ABSENTEE).unwrap(),
            recorded_guard: vs.finalized_participation_recorded.read(&recorded).unwrap(),
            evicted_guard: vs.finalized_participation_recorded.read(&evicted).unwrap(),
            ring: vs
                .participation_prune_ring()
                .position_for_test(idx)
                .unwrap(),
        }
    })
}

/// The view with `guards` = [recorded block guard, evicted block guard].
fn ring_view(missed_votes: u64, guards: [bool; 2], entry: B256, seq: u64) -> ParticipationRingView {
    ParticipationRingView {
        missed_votes,
        recorded_guard: guards[0],
        evicted_guard: guards[1],
        ring: RingPosition { entry, seq },
    }
}

/// Records one finalized block that [`RING_ABSENTEE`] missed.
fn record_absence(storage: StorageHandle, fb_hash: B256) -> Result<(), PrecompileError> {
    crate::hooks::record_finalized_participation(storage, fb_hash, &[], &[RING_ABSENTEE])
}

fn record_ring_absence(
    provider: &mut HashMapStorageProvider,
    fb_hash: B256,
) -> Result<(), PrecompileError> {
    provider.enter(|storage| record_absence(storage, fb_hash))
}

/// Characterizes the write order at a wrapped cursor: the participation
/// counters, then the replay guard of `fb_hash`, then the guard of the evicted
/// block, then the ring entry, then the cursor. A failure before write `n`
/// leaves exactly the first `n` writes applied.
#[test]
fn finalized_participation_ring_write_order_at_a_wrapped_cursor() {
    let evicted = test_ring_hash(1);
    let fb_hash = test_ring_hash(2);
    let seq = crate::hooks::FINALIZED_PARTICIPATION_RETAIN + 5;
    let views = HashMapStorageProvider::mutation_prefix_views(
        || seeded_participation_ring(seq, evicted, &[evicted]),
        |storage| record_absence(storage, fb_hash),
        |provider| participation_ring_view(provider, fb_hash, evicted, 5),
    )
    .unwrap();
    assert_eq!(
        views,
        MutationPrefixViews {
            before_mutation: vec![
                ring_view(0, [false, true], evicted, seq),
                ring_view(1, [false, true], evicted, seq),
                ring_view(1, [true, true], evicted, seq),
                ring_view(1, [true, false], evicted, seq),
                ring_view(1, [true, false], fb_hash, seq),
            ],
            complete: ring_view(1, [true, false], fb_hash, seq + 1),
            mutations: 5,
        }
    );
}

/// An empty ring entry and an entry equal to `fb_hash` evict nothing. The
/// recorded block keeps its new replay guard.
#[test]
fn finalized_participation_ring_skips_an_empty_entry_and_the_recorded_block() {
    let fb_hash = test_ring_hash(7);
    // The ZERO entry has a stale guard that must stay. The `fb_hash` entry
    // reads the same guard as the recorded block.
    let zero = B256::ZERO;
    let cases = [
        (zero, vec![zero], [true, true, true, true], zero),
        (fb_hash, Vec::new(), [false, false, true, true], fb_hash),
    ];
    for (entry, guarded, evicted_guard, first_entry) in cases {
        let views = HashMapStorageProvider::mutation_prefix_views(
            || seeded_participation_ring(3, entry, &guarded),
            |storage| record_absence(storage, fb_hash),
            |provider| participation_ring_view(provider, fb_hash, entry, 3),
        )
        .unwrap();
        assert_eq!(
            views,
            MutationPrefixViews {
                before_mutation: vec![
                    ring_view(0, [false, evicted_guard[0]], first_entry, 3),
                    ring_view(1, [false, evicted_guard[1]], first_entry, 3),
                    ring_view(1, [true, evicted_guard[2]], first_entry, 3),
                    ring_view(1, [true, evicted_guard[3]], fb_hash, 3),
                ],
                complete: ring_view(1, [true, true], fb_hash, 4),
                mutations: 4,
            },
            "entry {entry}"
        );
    }
}

/// Empty participation and a replay of a recorded block return before any
/// write. The ring does not advance.
#[test]
fn finalized_participation_ring_ignores_empty_and_replayed_blocks() {
    let evicted = test_ring_hash(1);
    let fb_hash = test_ring_hash(2);
    let mut provider = seeded_participation_ring(5, evicted, &[evicted, fb_hash]);
    provider.clear_mutation_failure();
    provider
        .enter(|storage| {
            crate::hooks::record_finalized_participation(storage, test_ring_hash(3), &[], &[])
        })
        .unwrap();
    record_ring_absence(&mut provider, fb_hash).unwrap();
    assert_eq!(provider.clear_mutation_failure(), 0);
    assert_eq!(
        participation_ring_view(&mut provider, fb_hash, evicted, 5),
        ring_view(0, [true, true], evicted, 5)
    );
}

/// At the last cursor value the call still records participation, evicts and
/// writes the ring entry. Then it fails with the original fatal error and
/// leaves the cursor unchanged.
#[test]
fn finalized_participation_ring_cursor_overflow_is_fatal_after_the_ring_write() {
    let evicted = test_ring_hash(1);
    let fb_hash = test_ring_hash(2);
    let mut provider = seeded_participation_ring(u64::MAX, evicted, &[evicted]);
    let result = record_ring_absence(&mut provider, fb_hash);
    assert!(
        matches!(&result, Err(PrecompileError::Fatal(message)) if message == "finalized participation ring sequence overflow"),
        "{result:?}"
    );
    assert_eq!(
        participation_ring_view(
            &mut provider,
            fb_hash,
            evicted,
            u64::MAX % crate::hooks::FINALIZED_PARTICIPATION_RETAIN
        ),
        ring_view(1, [true, false], fb_hash, u64::MAX)
    );
}
