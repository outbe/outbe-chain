//! The composite series id.

use super::*;

#[test]
fn the_day_key_still_hashes_to_the_slot_a_bare_u32_would() {
    use outbe_primitives::storage::types::StorageKey;
    assert_eq!(
        WorldwideDay::new(20_260_212).key_bytes(),
        20_260_212u32.key_bytes()
    );
}

