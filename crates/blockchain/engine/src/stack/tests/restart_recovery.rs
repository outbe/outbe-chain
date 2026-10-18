use super::*;

#[test]
fn native_recovery_uses_certified_archive_identity_when_ack_floor_lags() {
    let archive_height = 302;
    let archive_hash = B256::repeat_byte(0x42);
    let round = Round::new(Epoch::new(3), View::new(17));
    let (height, hash, recovered_round) = reconcile_recovered_execution_head(
        archive_height,
        archive_hash,
        Some(RecoveredApplicationFinalization {
            round,
            digest: Digest(archive_hash),
        }),
    )
    .unwrap();
    assert_eq!(
        (height, hash, recovered_round),
        (302, archive_hash, Some(round))
    );
    assert_ne!(
        height, 301,
        "the lagging ACK floor does not replace archive finality"
    );
}

#[test]
fn unfinalized_head_lead_is_recoverable_covers_bounds_and_zero_tip() {
    for (last_execution_height, finalized_tip, expected) in [
        // Steady state: head is exactly one block ahead of the finalized tip.
        (70, 69, true),
        // A few blocks ahead during a finalization hiccup, up to the bound.
        (69 + MAX_UNFINALIZED_HEAD_LEAD, 69, true),
        // head == finalized: recover(head) would have succeeded. Not this arm.
        (69, 69, false),
        // head behind finalized (execution lags): saturating lead is 0.
        (68, 69, false),
        // No durable finalized tip at all -> fresh/corrupt, never the benign case.
        (5, 0, false),
        // A head far ahead of the finalized tip is suspicious, not an in-flight head.
        (69 + MAX_UNFINALIZED_HEAD_LEAD + 1, 69, false),
    ] {
        assert_eq!(
            unfinalized_head_lead_is_recoverable(last_execution_height, finalized_tip),
            expected,
            "last_execution_height={last_execution_height}, finalized_tip={finalized_tip}"
        );
    }
}

#[test]
fn recovery_anchor_never_promotes_an_execution_only_head_to_finalized() {
    assert_eq!(durable_recovery_anchor_height(70, 69), 69);
    assert_eq!(durable_recovery_anchor_height(69, 69), 69);
    assert_eq!(durable_recovery_anchor_height(68, 69), 68);
    assert_eq!(durable_recovery_anchor_height(0, 0), 0);
}

#[cfg(feature = "snapshot-integration")]
pub(in crate::stack::tests) mod copied_native;
