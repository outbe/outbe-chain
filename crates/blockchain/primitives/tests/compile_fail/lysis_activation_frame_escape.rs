use outbe_primitives::{
    error::Result,
    storage::{hashmap::HashMapStorageProvider, CertifiedLysisActivation, StorageHandle},
};

fn main() {
    let mut provider = HashMapStorageProvider::new(1);
    let handle = StorageHandle::new(&mut provider);
    let _: Result<&'static mut CertifiedLysisActivation<'static>> =
        handle.with_lysis_activation_frame(Default::default(), |capability| Ok(capability));
}
