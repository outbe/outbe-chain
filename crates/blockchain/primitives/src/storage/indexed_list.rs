use super::dsl::{List, Map};
use super::types::{Storable, StorageKey};
use crate::error::{PrecompileError, Result};

pub struct IndexedList<'values, 'storage, T> {
    values: &'values List<'storage, T>,
    positions: Map<'storage, T, u32>,
}

#[derive(Debug, thiserror::Error)]
enum IndexError {
    #[error("state index already contains the identity")]
    AlreadyPresent,
    #[error("state index does not contain the identity")]
    Missing,
    #[error("state index position differs from its list entry")]
    PositionMismatch,
    #[error("state index length exceeds u32")]
    LengthOverflow,
}

impl From<IndexError> for PrecompileError {
    fn from(error: IndexError) -> Self {
        Self::fatal(error.to_string())
    }
}

impl<'values, 'storage, T: Storable + StorageKey + Copy + Eq> IndexedList<'values, 'storage, T> {
    pub fn new(values: &'values List<'storage, T>, positions: Map<'storage, T, u32>) -> Self {
        Self { values, positions }
    }

    pub fn insert(&self, value: T) -> Result<()> {
        self.positions.storage().with_checkpoint(|| {
            if self.positions.read(&value)? != 0 {
                return Err(IndexError::AlreadyPresent.into());
            }
            let next = self
                .values
                .len()?
                .checked_add(1)
                .ok_or(IndexError::LengthOverflow)?;
            self.values.push(value)?;
            self.positions.write(&value, next)
        })
    }

    pub fn remove(&self, value: T) -> Result<()> {
        self.positions.storage().with_checkpoint(|| {
            let index = self
                .positions
                .read(&value)?
                .checked_sub(1)
                .ok_or(IndexError::Missing)?;
            if self.values.get(index)? != Some(value) {
                return Err(IndexError::PositionMismatch.into());
            }
            let len = self.values.len()?;
            let last = len.checked_sub(1).ok_or(IndexError::Missing)?;
            let moved = self.values.get(last)?.ok_or(IndexError::PositionMismatch)?;
            if self.positions.read(&moved)? != len {
                return Err(IndexError::PositionMismatch.into());
            }
            if index != last {
                self.values.set(index, moved)?;
                self.positions.write(&moved, index + 1)?;
            }
            if self.values.pop()? != Some(moved) {
                return Err(IndexError::PositionMismatch.into());
            }
            self.positions.clear(&value)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{hashmap::HashMapStorageProvider, StorageHandle};
    use alloy_primitives::{Address, U256};

    #[test]
    fn middle_removal_preserves_positions_and_permits_reinsertion() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let values = List::new(U256::ZERO, Address::ZERO, storage.clone());
            let positions = Map::new(U256::from(1), Address::ZERO, storage);
            let index = IndexedList::new(&values, positions);
            for value in [11u64, 22, 33] {
                index.insert(value).unwrap();
            }
            assert!(index.insert(22).is_err());
            index.remove(22).unwrap();
            assert_eq!(index.values.read_all().unwrap(), vec![11, 33]);
            assert_eq!(index.positions.read(&33).unwrap(), 2);
            assert_eq!(index.positions.read(&22).unwrap(), 0);
            assert!(index.remove(22).is_err());
            index.insert(22).unwrap();
            assert_eq!(index.values.read_all().unwrap(), vec![11, 33, 22]);
            for value in [11, 33, 22] {
                index.remove(value).unwrap();
            }
            assert!(index.values.is_empty().unwrap());
        });
    }

    #[test]
    fn inconsistent_reverse_index_does_not_change_the_list() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let values = List::new(U256::ZERO, Address::ZERO, storage.clone());
            let positions = Map::new(U256::from(1), Address::ZERO, storage);
            let index = IndexedList::new(&values, positions);
            index.insert(11u64).unwrap();
            index.insert(22u64).unwrap();
            index.positions.write(&22, 1).unwrap();
            assert!(index.remove(11).is_err());
            assert_eq!(index.values.read_all().unwrap(), vec![11, 22]);
            assert_eq!(index.positions.read(&11).unwrap(), 1);
        });
    }
}
