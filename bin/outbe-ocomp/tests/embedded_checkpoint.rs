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
