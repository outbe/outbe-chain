use alloy_primitives::{keccak256, Address, U256};

use crate::error::Result;
use crate::storage::StorageHandle;

use super::Storable;

/// EVM-compatible dynamic array (Solidity `T[]`).
///
/// Storage layout:
/// - Base slot: stores length as U256
/// - Data slots: `keccak256(base_slot) + index * T::SLOTS`
pub struct StorageVec<'storage, T> {
    base_slot: U256,
    address: Address,
    storage: StorageHandle<'storage>,
    _marker: std::marker::PhantomData<T>,
}

impl<'storage, T: Storable> StorageVec<'storage, T> {
    pub fn new(base_slot: U256, address: Address, storage: StorageHandle<'storage>) -> Self {
        Self {
            base_slot,
            address,
            storage,
            _marker: std::marker::PhantomData,
        }
    }

    pub fn len(&self) -> Result<u32> {
        let word = self.storage.sload(self.address, self.base_slot)?;
        Ok(word.to::<u32>())
    }

    pub fn base_slot(&self) -> U256 {
        self.base_slot
    }

    pub fn is_empty(&self) -> Result<bool> {
        self.len().map(|l| l == 0)
    }

    pub fn get(&self, index: u32) -> Result<Option<T>> {
        let length = self.len()?;
        if index >= length {
            return Ok(None);
        }
        let word = self.storage.sload(self.address, self.data_slot(index))?;
        Ok(Some(T::from_word(word)))
    }

    pub fn set(&self, index: u32, value: T) -> Result<()> {
        let length = self.len()?;
        if index >= length {
            return Err(crate::error::PrimitivesStorageError::VecIndexOutOfBounds.into());
        }
        self.storage
            .sstore(self.address, self.data_slot(index), value.to_word())
    }

    pub fn push(&self, value: T) -> Result<()> {
        let length = self.len()?;
        self.storage
            .sstore(self.address, self.data_slot(length), value.to_word())?;
        self.storage
            .sstore(self.address, self.base_slot, U256::from(length + 1))
    }

    pub fn pop(&self) -> Result<Option<T>> {
        let length = self.len()?;
        if length == 0 {
            return Ok(None);
        }
        let last = length - 1;
        let slot = self.data_slot(last);
        let word = self.storage.sload(self.address, slot)?;
        let value = T::from_word(word);
        self.storage.sstore(self.address, slot, U256::ZERO)?;
        self.storage
            .sstore(self.address, self.base_slot, U256::from(last))?;
        Ok(Some(value))
    }

    pub fn read_all(&self) -> Result<Vec<T>> {
        let length = self.len()?;
        let mut result = Vec::with_capacity(length as usize);
        for i in 0..length {
            let word = self.storage.sload(self.address, self.data_slot(i))?;
            result.push(T::from_word(word));
        }
        Ok(result)
    }

    pub fn clear(&self) -> Result<()> {
        let length = self.len()?;
        for i in 0..length {
            self.storage
                .sstore(self.address, self.data_slot(i), U256::ZERO)?;
        }
        self.storage
            .sstore(self.address, self.base_slot, U256::ZERO)
    }

    pub fn data_slot(&self, index: u32) -> U256 {
        let start = U256::from_be_bytes(keccak256(self.base_slot.to_be_bytes::<32>()).0);
        start + U256::from(index as u64 * T::SLOTS as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::hashmap::HashMapStorageProvider;
    use crate::storage::StorageHandle;
    use alloy_primitives::address;

    fn with_vec<T: Storable>(slot: u64, f: impl FnOnce(StorageVec<T>)) {
        let mut provider = HashMapStorageProvider::new(1);
        let addr = address!("0x0000000000000000000000000000000000001003");
        f(StorageVec::new(
            U256::from(slot),
            addr,
            StorageHandle::new(&mut provider),
        ));
    }

    #[test]
    fn test_empty() {
        with_vec::<u64>(0, |v| {
            assert_eq!(v.len().unwrap(), 0);
            assert!(v.is_empty().unwrap());
            assert_eq!(v.pop().unwrap(), None);
        });
    }

    #[test]
    fn test_push_get() {
        with_vec::<u64>(0, |v| {
            v.push(100).unwrap();
            v.push(200).unwrap();
            assert_eq!(v.len().unwrap(), 2);
            assert_eq!(v.get(0).unwrap(), Some(100));
            assert_eq!(v.get(1).unwrap(), Some(200));
            assert_eq!(v.get(2).unwrap(), None);
        });
    }

    #[test]
    fn test_pop() {
        with_vec::<u64>(0, |v| {
            v.push(10).unwrap();
            v.push(20).unwrap();
            assert_eq!(v.pop().unwrap(), Some(20));
            assert_eq!(v.pop().unwrap(), Some(10));
            assert_eq!(v.pop().unwrap(), None);
        });
    }

    #[test]
    fn test_set() {
        with_vec::<u64>(0, |v| {
            v.push(10).unwrap();
            v.set(0, 99).unwrap();
            assert_eq!(v.get(0).unwrap(), Some(99));
            assert!(v.set(5, 0).is_err());
        });
    }

    #[test]
    fn test_read_all() {
        with_vec::<u64>(0, |v| {
            v.push(1).unwrap();
            v.push(2).unwrap();
            v.push(3).unwrap();
            assert_eq!(v.read_all().unwrap(), vec![1, 2, 3]);
        });
    }

    #[test]
    fn test_clear() {
        with_vec::<u64>(0, |v| {
            v.push(1).unwrap();
            v.push(2).unwrap();
            v.clear().unwrap();
            assert!(v.is_empty().unwrap());
            v.push(3).unwrap();
            assert_eq!(v.get(0).unwrap(), Some(3));
        });
    }

    #[test]
    fn test_u256() {
        with_vec::<U256>(5, |v| {
            v.push(U256::MAX).unwrap();
            assert_eq!(v.get(0).unwrap(), Some(U256::MAX));
        });
    }
}
