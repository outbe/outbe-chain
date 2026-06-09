use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

fn escaped() -> StorageHandle<'static> {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::new(&mut provider)
}

fn main() {
    let _ = escaped();
}
