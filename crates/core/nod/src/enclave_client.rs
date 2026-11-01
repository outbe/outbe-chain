//! Private arithmetic access to encrypted NOD amounts.
use alloy_primitives::U256;
use outbe_primitives::nod_encryption::EncryptedNodV1;
use outbe_tee::TransportError;

pub(crate) fn read_amount(record: &EncryptedNodV1) -> Result<U256, TransportError> {
    #[cfg(any(test, feature = "test-enclave"))]
    if test_enclave::enabled() {
        return outbe_tee_enclave::nod_encryption::decrypt_nod(
            &test_enclave::NETWORK_SECRET,
            record,
        )
        .map_err(crate::errors::enclave_transport_error);
    }
    outbe_tee::nod_mine::read_nod_amount(record)
}

/// In-process enclave stand-in for tests (this crate's tests and any downstream
/// crate that enables the `test-enclave` feature). The crate root rejects
/// `all(feature = "test-enclave", feature = "production-enclave")` at compile
/// time and CI's `no-test-deps` gate rejects `test-enclave` in the production
/// dependency graph, so this stand-in cannot reach a production build.
#[cfg(any(test, feature = "test-enclave"))]
pub mod test_enclave {
    use std::cell::Cell;
    thread_local! { static INSTALLED: Cell<bool> = const { Cell::new(false) }; }
    pub const NETWORK_SECRET: [u8; 32] = [0x5a; 32];
    pub struct Guard {
        previous: bool,
        _thread: std::marker::PhantomData<std::rc::Rc<()>>,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            INSTALLED.set(self.previous);
        }
    }
    pub fn scope() -> Guard {
        Guard {
            previous: INSTALLED.replace(true),
            _thread: std::marker::PhantomData,
        }
    }
    pub fn install() {
        INSTALLED.set(true);
    }
    pub fn uninstall() {
        INSTALLED.set(false);
    }
    pub(super) fn enabled() -> bool {
        INSTALLED.get()
    }
}
