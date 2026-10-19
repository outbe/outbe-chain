use super::dsl::{List, Map, StorageRecord};
use super::types::{Storable, StorageKey};
use crate::error::{PrecompileError, Result};

pub struct IndexedList<'storage, T> {
    values: List<'storage, T>,
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

impl<'storage, T: Storable + StorageKey + Copy + Eq> IndexedList<'storage, T> {
    pub fn new(values: List<'storage, T>, positions: Map<'storage, T, u32>) -> Self {
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

/// Creates `record` and lists its key in every index, all or nothing.
pub fn create_indexed<'storage, V, const N: usize>(
    records: &Map<'storage, V::Key, V>,
    record: &V,
    indexes: [IndexedList<'storage, V::Key>; N],
) -> Result<()>
where
    V: StorageRecord,
    V::Key: Storable + StorageKey + Copy + Eq,
{
    records.storage().with_checkpoint(|| {
        records.create(record)?;
        indexes
            .iter()
            .try_for_each(|index| index.insert(record.key()))
    })
}

/// Unlists an already-loaded record under `key` from `indexes` and deletes it, all or
/// nothing.
pub fn delete_loaded_indexed<'storage, V, const N: usize>(
    records: &Map<'storage, V::Key, V>,
    key: V::Key,
    indexes: [IndexedList<'storage, V::Key>; N],
) -> Result<()>
where
    V: StorageRecord,
    V::Key: Storable + StorageKey + Copy + Eq,
{
    records.storage().with_checkpoint(|| {
        indexes.iter().try_for_each(|index| index.remove(key))?;
        records.delete(key)
    })
}

/// Unlists the record under `key` from the indexes it selects and deletes it, all or
/// nothing.
pub fn delete_indexed<'storage, V, const N: usize>(
    records: &Map<'storage, V::Key, V>,
    key: V::Key,
    indexes: impl FnOnce(&V) -> [IndexedList<'storage, V::Key>; N],
) -> Result<()>
where
    V: StorageRecord,
    V::Key: Storable + StorageKey + Copy + Eq,
{
    records.storage().with_checkpoint(|| {
        let record = records
            .get(key)?
            .ok_or_else(|| PrecompileError::fatal("state record is absent from its index"))?;
        delete_loaded_indexed(records, key, indexes(&record))
    })
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
            let index = IndexedList::new(values, positions);
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
            let index = IndexedList::new(values, positions);
            index.insert(11u64).unwrap();
            index.insert(22u64).unwrap();
            index.positions.write(&22, 1).unwrap();
            assert!(index.remove(11).is_err());
            assert_eq!(index.values.read_all().unwrap(), vec![11, 22]);
            assert_eq!(index.positions.read(&11).unwrap(), 1);
        });
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct SampleRecord {
        id: u64,
        amount: u64,
    }

    impl StorageRecord for SampleRecord {
        type Key = u64;
        const SLOTS: u64 = 1;

        fn key(&self) -> Self::Key {
            self.id
        }

        fn exists(entry: &super::super::dsl::RecordEntry<'_, Self::Key, Self>) -> Result<bool> {
            Ok(super::super::types::Mapping::<u64, u64>::new(
                entry.base_slot(),
                entry.address(),
                entry.storage(),
            )
            .read(entry.key_ref())?
                != 0)
        }

        fn load(
            entry: &super::super::dsl::RecordEntry<'_, Self::Key, Self>,
        ) -> Result<Option<Self>> {
            let amount = super::super::types::Mapping::<u64, u64>::new(
                entry.base_slot(),
                entry.address(),
                entry.storage(),
            )
            .read(entry.key_ref())?;
            if amount == 0 {
                Ok(None)
            } else {
                Ok(Some(Self {
                    id: *entry.key_ref(),
                    amount,
                }))
            }
        }

        fn create(
            entry: &super::super::dsl::RecordEntry<'_, Self::Key, Self>,
            value: &Self,
        ) -> Result<()> {
            if Self::exists(entry)? {
                return Err(super::super::dsl::existing_record_err("SampleRecord"));
            }
            Self::update(entry, value)
        }

        fn update(
            entry: &super::super::dsl::RecordEntry<'_, Self::Key, Self>,
            value: &Self,
        ) -> Result<()> {
            super::super::types::Mapping::<u64, u64>::new(
                entry.base_slot(),
                entry.address(),
                entry.storage(),
            )
            .write(entry.key_ref(), value.amount)
        }

        fn delete(entry: &super::super::dsl::RecordEntry<'_, Self::Key, Self>) -> Result<()> {
            if !Self::exists(entry)? {
                return Err(super::super::dsl::missing_record_err("SampleRecord"));
            }
            super::super::types::Mapping::<u64, u64>::new(
                entry.base_slot(),
                entry.address(),
                entry.storage(),
            )
            .get(entry.key_ref())
            .delete()
        }
    }

    #[test]
    fn create_and_delete_indexed_keep_record_and_indexes_atomic() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let records =
                Map::<u64, SampleRecord>::new(U256::from(10), Address::ZERO, storage.clone());
            let make_index = || {
                IndexedList::new(
                    List::new(U256::from(20), Address::ZERO, storage.clone()),
                    Map::new(U256::from(21), Address::ZERO, storage.clone()),
                )
            };

            for rec in [
                SampleRecord { id: 1, amount: 100 },
                SampleRecord { id: 2, amount: 200 },
                SampleRecord { id: 3, amount: 300 },
            ] {
                create_indexed(&records, &rec, [make_index()]).unwrap();
            }

            // Duplicate creation fails without corrupting the index.
            assert!(create_indexed(
                &records,
                &SampleRecord { id: 2, amount: 999 },
                [make_index()]
            )
            .is_err());
            assert_eq!(
                records.get(2).unwrap(),
                Some(SampleRecord { id: 2, amount: 200 })
            );

            // Deleting the first record swaps the last key (3) into position 1.
            delete_indexed(&records, 1, |_| [make_index()]).unwrap();
            assert_eq!(records.get(1).unwrap(), None);
            let idx = make_index();
            assert_eq!(idx.values.read_all().unwrap(), vec![3, 2]);
            assert_eq!(idx.positions.read(&3).unwrap(), 1);
            assert_eq!(idx.positions.read(&2).unwrap(), 2);

            // Deleting a missing key fails.
            assert!(delete_indexed(&records, 1, |_| [make_index()]).is_err());
        });
    }
}
