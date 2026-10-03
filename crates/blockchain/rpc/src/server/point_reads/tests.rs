use super::*;
use std::cell::Cell;

#[test]
fn future_or_unknown_finality_never_reads_a_header() {
    for finalized in [None, Some(9)] {
        assert!(finalized_header_at(finalized, 10, || {
            panic!("an ineligible point read must not load a header")
        })
        .is_none());
    }
}

#[test]
fn eligible_finality_loads_once_and_preserves_unavailable_or_selected_header() {
    let calls = Cell::new(0);
    assert!(finalized_header_at(Some(10), 10, || {
        calls.set(calls.get() + 1);
        None
    })
    .is_none());
    let header = finalized_header_at(Some(11), 10, || {
        calls.set(calls.get() + 1);
        Some(SelectedHeaderV1 {
            block_number: 10,
            block_hash: B256::repeat_byte(7),
            extra_data: vec![1, 2, 3],
        })
    })
    .unwrap();
    assert_eq!(calls.get(), 2);
    assert_eq!(header.extra_data, [1, 2, 3]);
    assert_eq!(header.block_hash, B256::repeat_byte(7));
}
