use crate::storage::{hashmap::HashMapStorageProvider, StorageHandle};

pub(super) fn with_storage<F: FnOnce(StorageHandle)>(f: F) {
    let mut provider = HashMapStorageProvider::new(1);
    let storage = StorageHandle::new(&mut provider);
    f(storage);
}
