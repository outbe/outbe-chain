mod array;
mod binary_heap;
mod bytes_like;
mod circular_buffer;
mod deque;
mod mapping;
mod set;
mod slot;
#[cfg(test)]
mod test_support;
mod vec;

pub use array::StorageArray;
pub use binary_heap::BinaryHeap;
pub use bytes_like::StorageBytes;
pub use circular_buffer::StorageCircularBuffer;
pub use deque::StorageDeque;
pub use mapping::Mapping;
pub use set::StorageSet;
pub use slot::Slot;
pub use vec::StorageVec;

use crate::error::Result;
use alloy_primitives::U256;

/// Describes how a type maps to EVM storage.
pub trait StorableType {
    /// Number of 32-byte slots this type occupies.
    const SLOTS: usize;
}

/// Trait for types that can be stored/loaded from a single EVM storage slot.
pub trait Storable: StorableType + Sized {
    /// Byte width of the encoded value within a 32-byte storage word (`1..=32`).
    const BYTES: usize = 32;

    /// Convert from a U256 storage word.
    fn from_word(word: U256) -> Self;
    /// Convert to a U256 storage word.
    fn to_word(&self) -> U256;
}

/// Extracts a `Storable` value encoded at `byte_offset` (`0..32`) within a 32-byte storage word.
#[inline]
pub fn unpack_from_word<T: Storable>(word: U256, byte_offset: usize) -> T {
    debug_assert!(
        byte_offset + T::BYTES <= 32,
        "sub-word field overflows 32-byte storage slot"
    );
    if T::BYTES == 32 {
        T::from_word(word)
    } else {
        let shift = byte_offset * 8;
        let mask = (U256::ONE << (T::BYTES * 8)) - U256::ONE;
        T::from_word((word >> shift) & mask)
    }
}

/// Packs `value` at `byte_offset` (`0..32`) into `word`, replacing only `[byte_offset .. byte_offset + T::BYTES]`.
#[inline]
pub fn pack_into_word<T: Storable>(word: U256, byte_offset: usize, value: &T) -> U256 {
    debug_assert!(
        byte_offset + T::BYTES <= 32,
        "sub-word field overflows 32-byte storage slot"
    );
    if T::BYTES == 32 {
        value.to_word()
    } else {
        let shift = byte_offset * 8;
        let mask = (U256::ONE << (T::BYTES * 8)) - U256::ONE;
        let cleared = word & !(mask << shift);
        let encoded = (value.to_word() & mask) << shift;
        cleared | encoded
    }
}

/// Clears the `[byte_offset .. byte_offset + T::BYTES]` slice of `word`.
#[inline]
pub fn clear_in_word<T: Storable>(word: U256, byte_offset: usize) -> U256 {
    debug_assert!(
        byte_offset + T::BYTES <= 32,
        "sub-word field overflows 32-byte storage slot"
    );
    if T::BYTES == 32 {
        U256::ZERO
    } else {
        let shift = byte_offset * 8;
        let mask = (U256::ONE << (T::BYTES * 8)) - U256::ONE;
        word & !(mask << shift)
    }
}

/// Trait for types that can be used as mapping keys.
///
/// Keys are left-padded to 32 bytes and hashed with the base slot via keccak256.
pub trait StorageKey {
    /// Returns the key bytes (will be left-padded to 32 bytes).
    fn key_bytes(&self) -> Vec<u8>;

    /// Computes the mapping slot: `keccak256(left_pad(key) ++ base_slot)`.
    fn mapping_slot(&self, base_slot: U256) -> U256 {
        use alloy_primitives::keccak256;

        let key = self.key_bytes();
        let mut buf = [0u8; 64];
        // Left-pad key to 32 bytes
        let start = 32 - key.len();
        buf[start..32].copy_from_slice(&key);
        // Base slot in big-endian
        buf[32..64].copy_from_slice(&base_slot.to_be_bytes::<32>());
        U256::from_be_bytes(keccak256(buf).0)
    }
}

/// Storage operations for reading/writing U256 values at slots.
pub trait StorageOps {
    /// Stores a value at the provided slot.
    fn store(&mut self, slot: U256, value: U256) -> Result<()>;
    /// Loads a value from the provided slot.
    fn load(&self, slot: U256) -> Result<U256>;
}

// --- Storable implementations for primitive types ---

impl StorableType for U256 {
    const SLOTS: usize = 1;
}

impl Storable for U256 {
    fn from_word(word: U256) -> Self {
        word
    }
    fn to_word(&self) -> U256 {
        *self
    }
}

impl StorableType for u64 {
    const SLOTS: usize = 1;
}

impl Storable for u64 {
    const BYTES: usize = 8;

    fn from_word(word: U256) -> Self {
        word.to::<u64>()
    }
    fn to_word(&self) -> U256 {
        U256::from(*self)
    }
}

impl StorableType for u32 {
    const SLOTS: usize = 1;
}

impl Storable for u32 {
    const BYTES: usize = 4;

    fn from_word(word: U256) -> Self {
        word.to::<u32>()
    }
    fn to_word(&self) -> U256 {
        U256::from(*self)
    }
}

impl StorableType for u16 {
    const SLOTS: usize = 1;
}

impl Storable for u16 {
    const BYTES: usize = 2;

    fn from_word(word: U256) -> Self {
        word.to::<u16>()
    }
    fn to_word(&self) -> U256 {
        U256::from(*self)
    }
}

impl StorableType for bool {
    const SLOTS: usize = 1;
}

impl Storable for bool {
    const BYTES: usize = 1;

    fn from_word(word: U256) -> Self {
        !word.is_zero()
    }
    fn to_word(&self) -> U256 {
        if *self {
            U256::from(1)
        } else {
            U256::ZERO
        }
    }
}

impl StorableType for u8 {
    const SLOTS: usize = 1;
}

impl Storable for u8 {
    const BYTES: usize = 1;

    fn from_word(word: U256) -> Self {
        word.to::<u8>()
    }
    fn to_word(&self) -> U256 {
        U256::from(*self)
    }
}

use alloy_primitives::Address;

impl StorableType for Address {
    const SLOTS: usize = 1;
}

impl Storable for Address {
    const BYTES: usize = 20;

    fn from_word(word: U256) -> Self {
        Address::from_word(word.into())
    }
    fn to_word(&self) -> U256 {
        U256::from_be_bytes(self.into_word().0)
    }
}

use alloy_primitives::B256;

impl StorableType for B256 {
    const SLOTS: usize = 1;
}

impl Storable for B256 {
    fn from_word(word: U256) -> Self {
        B256::from(word.to_be_bytes::<32>())
    }
    fn to_word(&self) -> U256 {
        U256::from_be_bytes(self.0)
    }
}

// --- StorageKey implementations ---

impl StorageKey for Address {
    fn key_bytes(&self) -> Vec<u8> {
        self.as_slice().to_vec()
    }
}

impl StorageKey for U256 {
    fn key_bytes(&self) -> Vec<u8> {
        self.to_be_bytes::<32>().to_vec()
    }
}

impl StorageKey for u8 {
    fn key_bytes(&self) -> Vec<u8> {
        vec![*self]
    }
}

impl StorageKey for u64 {
    fn key_bytes(&self) -> Vec<u8> {
        self.to_be_bytes().to_vec()
    }
}

impl StorageKey for u32 {
    fn key_bytes(&self) -> Vec<u8> {
        self.to_be_bytes().to_vec()
    }
}

impl StorageKey for u16 {
    fn key_bytes(&self) -> Vec<u8> {
        self.to_be_bytes().to_vec()
    }
}

impl StorageKey for alloy_primitives::B256 {
    fn key_bytes(&self) -> Vec<u8> {
        self.as_slice().to_vec()
    }
}

use crate::address_pair::AddressPair;

/// Two words wide, which is exactly why there is no `Storable` impl: 40 bytes
/// cannot round-trip through a single one. As a mapping value it goes through
/// the `Mapping<K, AddressPair>` accessors instead.
impl StorableType for AddressPair {
    const SLOTS: usize = 2;
}
