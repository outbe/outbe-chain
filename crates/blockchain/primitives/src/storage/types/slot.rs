use alloy_primitives::{Address, U256};
use std::marker::PhantomData;

use crate::error::Result;
use crate::storage::types::{clear_in_word, pack_into_word, unpack_from_word, Storable};
use crate::storage::StorageHandle;

/// Type-safe accessor for a single EVM storage slot (or a packed sub-word slice within a slot).
///
/// `Slot<T>` knows the contract address, the slot index, the optional byte offset,
/// and the Rust type `T` stored there.
pub struct Slot<'storage, T> {
    slot: U256,
    offset: usize,
    packed: bool,
    address: Address,
    storage: StorageHandle<'storage>,
    _ty: PhantomData<T>,
}

impl<'storage, T> Slot<'storage, T> {
    /// Creates a new whole-slot accessor (`offset = 0`, unpacked).
    pub fn new(slot: U256, address: Address, storage: StorageHandle<'storage>) -> Self {
        Self {
            slot,
            offset: 0,
            packed: false,
            address,
            storage,
            _ty: PhantomData,
        }
    }

    /// Creates a sub-word packed slot accessor at `offset` bytes (`0..32`) within `slot`.
    pub fn new_with_offset(
        slot: U256,
        offset: usize,
        address: Address,
        storage: StorageHandle<'storage>,
    ) -> Self {
        Self {
            slot,
            offset,
            packed: true,
            address,
            storage,
            _ty: PhantomData,
        }
    }

    /// Returns the raw slot index.
    pub fn slot(&self) -> U256 {
        self.slot
    }

    /// Returns the byte offset within the 32-byte slot (`0` for unpacked slots).
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Returns the contract address.
    pub fn address(&self) -> Address {
        self.address
    }
}

impl<T: Storable> Slot<'_, T> {
    /// Reads the value from storage.
    pub fn read(&self) -> Result<T> {
        let word = self.storage.sload(self.address, self.slot)?;
        if self.packed && T::BYTES < 32 {
            Ok(unpack_from_word::<T>(word, self.offset))
        } else {
            Ok(T::from_word(word))
        }
    }

    /// Writes a value to storage.
    pub fn write(&self, value: T) -> Result<()> {
        if self.packed && T::BYTES < 32 {
            let current = self.storage.sload(self.address, self.slot)?;
            let updated = pack_into_word(current, self.offset, &value);
            self.storage.sstore(self.address, self.slot, updated)
        } else {
            self.storage
                .sstore(self.address, self.slot, value.to_word())
        }
    }

    /// Deletes (zeroes) the storage slot (or its packed sub-word slice).
    pub fn delete(&self) -> Result<()> {
        if self.packed && T::BYTES < 32 {
            let current = self.storage.sload(self.address, self.slot)?;
            let updated = clear_in_word::<T>(current, self.offset);
            self.storage.sstore(self.address, self.slot, updated)
        } else {
            self.storage.sstore(self.address, self.slot, U256::ZERO)
        }
    }
}
