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
/// Keys narrower than 32 bytes are left-padded to 32 bytes. Wider keys are
/// used as-is, as Solidity does. The result is hashed with the base slot via
/// keccak256.
pub trait StorageKey {
    /// Returns the key bytes.
    fn key_bytes(&self) -> Vec<u8>;

    /// Computes the mapping slot: `keccak256(pad(key) ++ base_slot)`.
    ///
    /// `pad` left-pads a key of up to 32 bytes to one word. It keeps a key
    /// wider than 32 bytes unpadded.
    fn mapping_slot(&self, base_slot: U256) -> U256 {
        use alloy_primitives::keccak256;

        let key = self.key_bytes();
        let key_width = key.len().max(32);
        let mut buf = vec![0u8; key_width + 32];
        buf[key_width - key.len()..key_width].copy_from_slice(&key);
        // Base slot in big-endian
        buf[key_width..].copy_from_slice(&base_slot.to_be_bytes::<32>());
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

impl StorableType for u128 {
    const SLOTS: usize = 1;
}

impl Storable for u128 {
    const BYTES: usize = 16;

    fn from_word(word: U256) -> Self {
        word.to::<u128>()
    }
    fn to_word(&self) -> U256 {
        U256::from(*self)
    }
}

impl StorableType for u64 {
    const SLOTS: usize = 1;
}

impl Storable for u64 {
    const BYTES: usize = 8;

    fn from_word(word: U256) -> Self {
        word.saturating_to::<u64>()
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
        word.saturating_to::<u32>()
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
        word.saturating_to::<u16>()
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
        word.saturating_to::<u8>()
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

#[cfg(test)]
mod tests {
    use super::*;

    struct DefaultDerivation<K>(K);

    impl<K: StorageKey> StorageKey for DefaultDerivation<K> {
        fn key_bytes(&self) -> Vec<u8> {
            self.0.key_bytes()
        }
    }

    #[test]
    fn wide_key_mapping_slot() {
        use crate::address_pair::AddressPair;
        use alloy_primitives::{keccak256, Address};

        let base_slot = U256::from(7);
        let pair =
            AddressPair::from_addresses(Address::repeat_byte(0x11), Address::repeat_byte(0x22));
        assert_eq!(pair.key_bytes().len(), 40);
        assert_eq!(
            DefaultDerivation(pair).mapping_slot(base_slot),
            pair.mapping_slot(base_slot)
        );

        // A narrow key keeps the left-padded derivation.
        let narrow = Address::repeat_byte(0x33);
        let mut padded = [0u8; 64];
        padded[12..32].copy_from_slice(narrow.as_slice());
        padded[32..].copy_from_slice(&base_slot.to_be_bytes::<32>());
        assert_eq!(
            DefaultDerivation(narrow).mapping_slot(base_slot),
            U256::from_be_bytes(keccak256(padded).0)
        );
    }

    #[test]
    fn storable_in_range_roundtrip() {
        for v in [0u64, 1, u64::MAX] {
            assert_eq!(u64::from_word(v.to_word()), v);
        }
        for v in [0u32, 1, u32::MAX] {
            assert_eq!(u32::from_word(v.to_word()), v);
        }
        for v in [0u16, 1, u16::MAX] {
            assert_eq!(u16::from_word(v.to_word()), v);
        }
        for v in [0u8, 1, u8::MAX] {
            assert_eq!(u8::from_word(v.to_word()), v);
        }
    }

    #[test]
    fn storable_narrowing_decodes_do_not_panic() {
        // Out-of-range words saturate to T::MAX (saturating_to rather than wrapping_to).
        for word in [U256::from(u64::MAX) + U256::ONE, U256::MAX] {
            assert_eq!(u64::from_word(word), u64::MAX);
        }
        for word in [U256::from(u32::MAX) + U256::ONE, U256::MAX] {
            assert_eq!(u32::from_word(word), u32::MAX);
        }
        for word in [U256::from(u16::MAX) + U256::ONE, U256::MAX] {
            assert_eq!(u16::from_word(word), u16::MAX);
        }
        for word in [U256::from(u8::MAX) + U256::ONE, U256::MAX] {
            assert_eq!(u8::from_word(word), u8::MAX);
        }
    }
}
