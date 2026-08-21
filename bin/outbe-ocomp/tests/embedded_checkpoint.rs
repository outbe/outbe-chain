use alloy_primitives::B256;
use outbe_ocomp::embedded_checkpoint::{OcompExExCheckpointStoreV1, OcompExExCheckpointV1};

#[test]
fn checkpoint_is_durable_and_exact_across_restart() {
    let temporary = tempfile::tempdir().unwrap();
    let first = OcompExExCheckpointV1 {
        block_number: 41,
        block_hash: B256::repeat_byte(0x41),
    };
    let second = OcompExExCheckpointV1 {
        block_number: 42,
        block_hash: B256::repeat_byte(0x42),
    };

    let mut store = OcompExExCheckpointStoreV1::open(temporary.path()).unwrap();
    assert_eq!(store.load(), None);
    store.persist(first).unwrap();
    store.persist(second).unwrap();
    drop(store);

    let reopened = OcompExExCheckpointStoreV1::open(temporary.path()).unwrap();
    assert_eq!(reopened.load(), Some(second));
}

/// Startup reconciliation may move the watermark back when it outran block
/// persistence, and the lower value must survive a restart. Ordinary advancement
/// stays strictly monotonic.
#[test]
fn rewind_moves_the_watermark_back_without_relaxing_persist() {
    let temporary = tempfile::tempdir().unwrap();
    let ahead = OcompExExCheckpointV1 {
        block_number: 139,
        block_hash: B256::repeat_byte(0x8b),
    };
    let canonical_tip = OcompExExCheckpointV1 {
        block_number: 137,
        block_hash: B256::repeat_byte(0x89),
    };

    let mut store = OcompExExCheckpointStoreV1::open(temporary.path()).unwrap();
    store.persist(ahead).unwrap();
    store.persist(canonical_tip).unwrap_err();

    store.rewind_to(canonical_tip).unwrap();
    assert_eq!(store.load(), Some(canonical_tip));
    // Re-scanning forward from the rewound point is ordinary advancement again.
    store.persist(ahead).unwrap();
    drop(store);

    let reopened = OcompExExCheckpointStoreV1::open(temporary.path()).unwrap();
    assert_eq!(reopened.load(), Some(ahead));
}
