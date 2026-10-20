//! A `StorageArray<U256, 4>` at slot 0 occupies slots 0 to 3. An explicit
//! `#[slot(2)]` on the next field puts that field inside the array. The
//! layout assertion of `#[contract]` must stop the build.

use alloy_primitives::U256;
use outbe_macros::contract;
use outbe_primitives::storage::types::StorageArray;

#[contract]
pub struct Overlay {
    pub words: StorageArray<U256, 4>,
    #[slot(2)]
    pub inside: Value<u64>,
}

fn main() {}
