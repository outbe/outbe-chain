use alloy_primitives::{keccak256, Address, U256};

use crate::error::Result;
use crate::storage::StorageHandle;

/// EVM-compatible dynamic bytes/string storage (Solidity layout).
///
/// - Short (<=31 bytes): data + length inline in one slot
/// - Long (>=32 bytes): length in base slot, data at keccak256(base_slot)
pub struct StorageBytes<'storage> {
    base_slot: U256,
    address: Address,
    storage: StorageHandle<'storage>,
}

impl<'storage> StorageBytes<'storage> {
    pub fn new(base_slot: U256, address: Address, storage: StorageHandle<'storage>) -> Self {
        Self {
            base_slot,
            address,
            storage,
        }
    }

    pub fn len(&self) -> Result<usize> {
        let raw = self.storage.sload(self.address, self.base_slot)?;
        Ok(decode_length(raw))
    }

    pub fn is_empty(&self) -> Result<bool> {
        self.len().map(|l| l == 0)
    }

    pub fn read(&self) -> Result<Vec<u8>> {
        let raw = self.storage.sload(self.address, self.base_slot)?;
        let length = decode_length(raw);
        if length == 0 {
            return Ok(Vec::new());
        }
        if is_short(raw) {
            let bytes = raw.to_be_bytes::<32>();
            Ok(bytes[..length].to_vec())
        } else {
            let data_start = data_slot(self.base_slot);
            let mut result = Vec::with_capacity(length);
            for i in 0..length.div_ceil(32) {
                let word = self
                    .storage
                    .sload(self.address, data_start + U256::from(i))?;
                let bytes = word.to_be_bytes::<32>();
                let take = (length - result.len()).min(32);
                result.extend_from_slice(&bytes[..take]);
            }
            Ok(result)
        }
    }

    pub fn write(&self, data: &[u8]) -> Result<()> {
        let length = data.len();
        if length <= 31 {
            let mut word = [0u8; 32];
            word[..length].copy_from_slice(data);
            word[31] = (length * 2) as u8;
            self.storage
                .sstore(self.address, self.base_slot, U256::from_be_bytes(word))
        } else {
            self.storage
                .sstore(self.address, self.base_slot, U256::from(length * 2 + 1))?;
            let data_start = data_slot(self.base_slot);
            for i in 0..length.div_ceil(32) {
                let start = i * 32;
                let end = (start + 32).min(length);
                let mut word = [0u8; 32];
                word[..end - start].copy_from_slice(&data[start..end]);
                self.storage.sstore(
                    self.address,
                    data_start + U256::from(i),
                    U256::from_be_bytes(word),
                )?;
            }
            Ok(())
        }
    }

    pub fn read_string(&self) -> Result<String> {
        self.read()
            .map(|b| String::from_utf8(b).unwrap_or_default())
    }

    pub fn write_string(&self, s: &str) -> Result<()> {
        self.write(s.as_bytes())
    }

    pub fn clear(&self) -> Result<()> {
        self.storage
            .sstore(self.address, self.base_slot, U256::ZERO)
    }
}

fn decode_length(raw: U256) -> usize {
    if raw.is_zero() {
        return 0;
    }
    if is_short(raw) {
        (raw.to_be_bytes::<32>()[31] / 2) as usize
    } else {
        let len_u256: U256 = (raw - U256::from(1u64)) >> 1;
        len_u256.to::<usize>()
    }
}

fn is_short(raw: U256) -> bool {
    (raw & U256::from(1u64)).is_zero()
}

fn data_slot(base_slot: U256) -> U256 {
    U256::from_be_bytes(keccak256(base_slot.to_be_bytes::<32>()).0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::hashmap::HashMapStorageProvider;
    use crate::storage::StorageHandle;
    use alloy_primitives::address;

    fn with_storage<F: FnOnce(StorageHandle)>(f: F) {
        let mut provider = HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        f(storage);
    }

    const ADDR: Address = address!("0x0000000000000000000000000000000000001003");

    #[test]
    fn test_empty() {
        with_storage(|storage| {
            let sb = StorageBytes::new(U256::ZERO, ADDR, storage);
            assert_eq!(sb.len().unwrap(), 0);
            assert!(sb.is_empty().unwrap());
        });
    }

    #[test]
    fn test_short_string() {
        with_storage(|storage| {
            let sb = StorageBytes::new(U256::ZERO, ADDR, storage);
            sb.write_string("hello").unwrap();
            assert_eq!(sb.len().unwrap(), 5);
            assert_eq!(sb.read_string().unwrap(), "hello");
        });
    }

    #[test]
    fn test_short_31_bytes() {
        with_storage(|storage| {
            let sb = StorageBytes::new(U256::ZERO, ADDR, storage);
            let data = vec![0xABu8; 31];
            sb.write(&data).unwrap();
            assert_eq!(sb.len().unwrap(), 31);
            assert_eq!(sb.read().unwrap(), data);
        });
    }

    #[test]
    fn test_long_32_bytes() {
        with_storage(|storage| {
            let sb = StorageBytes::new(U256::ZERO, ADDR, storage);
            let data = vec![0xCDu8; 32];
            sb.write(&data).unwrap();
            assert_eq!(sb.len().unwrap(), 32);
            assert_eq!(sb.read().unwrap(), data);
        });
    }

    #[test]
    fn test_long_100_bytes() {
        with_storage(|storage| {
            let sb = StorageBytes::new(U256::ZERO, ADDR, storage);
            let data: Vec<u8> = (0..100).map(|i| i as u8).collect();
            sb.write(&data).unwrap();
            assert_eq!(sb.len().unwrap(), 100);
            assert_eq!(sb.read().unwrap(), data);
        });
    }

    #[test]
    fn test_clear() {
        with_storage(|storage| {
            let sb = StorageBytes::new(U256::ZERO, ADDR, storage);
            sb.write_string("hello").unwrap();
            sb.clear().unwrap();
            assert!(sb.is_empty().unwrap());
        });
    }
}
