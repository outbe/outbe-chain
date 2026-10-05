use super::*;
use crate::error::PrecompileError;
use crate::storage::hashmap::HashMapStorageProvider;
use alloy_primitives::address;

fn with_storage<F: FnOnce(StorageHandle)>(f: F) {
    let mut provider = HashMapStorageProvider::new(1);
    let storage = StorageHandle::new(&mut provider);
    f(storage);
}

const ADDR: Address = address!("0x0000000000000000000000000000000000001004");

fn u64_cmp(a: &u64, b: &u64) -> Result<Ordering> {
    Ok(a.cmp(b))
}

fn u64_cmp_rev(a: &u64, b: &u64) -> Result<Ordering> {
    Ok(b.cmp(a))
}

#[test]
fn test_empty_heap_peek_pop_return_none() {
    with_storage(|storage| {
        let mut h: BinaryHeap<u64> = BinaryHeap::new(U256::ZERO, ADDR, storage);
        assert_eq!(h.len().unwrap(), 0);
        assert!(h.is_empty().unwrap());
        assert_eq!(h.peek().unwrap(), None);
        assert_eq!(h.pop_min(u64_cmp).unwrap(), None);
    });
}

#[test]
fn test_push_peek_is_min() {
    with_storage(|storage| {
        let mut h: BinaryHeap<u64> = BinaryHeap::new(U256::ZERO, ADDR, storage);
        for v in [3u64, 1, 4, 1, 5, 9, 2, 6] {
            h.push(v, u64_cmp).unwrap();
        }
        assert_eq!(h.len().unwrap(), 8);
        assert_eq!(h.peek().unwrap(), Some(1));
    });
}

#[test]
fn test_pop_min_sequence_matches_sorted() {
    with_storage(|storage| {
        let mut h: BinaryHeap<u64> = BinaryHeap::new(U256::ZERO, ADDR, storage);
        for v in [3u64, 1, 4, 1, 5, 9, 2, 6] {
            h.push(v, u64_cmp).unwrap();
        }
        let mut popped = Vec::new();
        while let Some(v) = h.pop_min(u64_cmp).unwrap() {
            popped.push(v);
        }
        assert_eq!(popped, vec![1, 1, 2, 3, 4, 5, 6, 9]);
        assert!(h.is_empty().unwrap());
    });
}

#[test]
fn test_custom_comparator_max_heap() {
    with_storage(|storage| {
        let mut h: BinaryHeap<u64> = BinaryHeap::new(U256::ZERO, ADDR, storage);
        for v in [3u64, 1, 4, 1, 5, 9, 2, 6] {
            h.push(v, u64_cmp_rev).unwrap();
        }
        // pop_min under reverse cmp = pop max under natural order
        let mut popped = Vec::new();
        while let Some(v) = h.pop_min(u64_cmp_rev).unwrap() {
            popped.push(v);
        }
        assert_eq!(popped, vec![9, 6, 5, 4, 3, 2, 1, 1]);
    });
}

#[test]
fn test_remove_by_equality_restores_invariant() {
    with_storage(|storage| {
        let mut h: BinaryHeap<u64> = BinaryHeap::new(U256::ZERO, ADDR, storage);
        for v in [3u64, 1, 4, 1, 5, 9, 2, 6] {
            h.push(v, u64_cmp).unwrap();
        }
        assert!(h.remove(&4, u64_cmp).unwrap());
        assert_eq!(h.len().unwrap(), 7);

        let mut popped = Vec::new();
        while let Some(v) = h.pop_min(u64_cmp).unwrap() {
            popped.push(v);
        }
        assert_eq!(popped, vec![1, 1, 2, 3, 5, 6, 9]);
    });
}

#[test]
fn test_remove_missing_returns_false() {
    with_storage(|storage| {
        let mut h: BinaryHeap<u64> = BinaryHeap::new(U256::ZERO, ADDR, storage);
        h.push(10, u64_cmp).unwrap();
        assert!(!h.remove(&99, u64_cmp).unwrap());
        assert_eq!(h.len().unwrap(), 1);
    });
}

#[test]
fn test_clear_resets_length() {
    with_storage(|storage| {
        let mut h: BinaryHeap<u64> = BinaryHeap::new(U256::ZERO, ADDR, storage);
        for v in [3u64, 1, 4] {
            h.push(v, u64_cmp).unwrap();
        }
        h.clear().unwrap();
        assert!(h.is_empty().unwrap());
        h.push(7, u64_cmp).unwrap();
        assert_eq!(h.peek().unwrap(), Some(7));
    });
}

#[test]
fn test_fallible_comparator_propagates_error() {
    with_storage(|storage| {
        let mut h: BinaryHeap<u64> = BinaryHeap::new(U256::ZERO, ADDR, storage);
        h.push(1, u64_cmp).unwrap();
        let err = h.push(2, |_a, _b| Err(PrecompileError::Revert("boom".into())));
        assert!(err.is_err());
    });
}

#[test]
fn test_read_all_returns_heap_array_order() {
    with_storage(|storage| {
        let mut h: BinaryHeap<u64> = BinaryHeap::new(U256::ZERO, ADDR, storage);
        for v in [3u64, 1, 2] {
            h.push(v, u64_cmp).unwrap();
        }
        // min-heap invariant: root is smallest, other positions are
        // implementation-specific but bounded above by children.
        let snapshot = h.read_all().unwrap();
        assert_eq!(snapshot.len(), 3);
        assert_eq!(snapshot[0], 1);
        assert!(snapshot[1..].iter().all(|v| *v >= 1));
    });
}
