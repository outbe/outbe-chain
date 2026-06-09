use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

fn main() {
    let mut provider = HashMapStorageProvider::new(1);
    let handle = StorageHandle::new(&mut provider);

    std::thread::spawn(move || {
        let _ = handle.chain_id();
    });
}
