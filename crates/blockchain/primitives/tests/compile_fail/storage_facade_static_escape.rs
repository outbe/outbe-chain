use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

struct Facade<'storage> {
    _storage: StorageHandle<'storage>,
}

fn takes_static(_: Facade<'static>) {}

fn main() {
    let mut provider = HashMapStorageProvider::new(1);
    let handle = StorageHandle::new(&mut provider);
    takes_static(Facade { _storage: handle });
}
