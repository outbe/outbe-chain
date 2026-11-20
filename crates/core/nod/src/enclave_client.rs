//! Private arithmetic access to encrypted NOD amounts.
use alloy_primitives::{B256, U256};
use outbe_core_common::nod_encryption::{
    EncryptedNodGroupLoadV1, EncryptedNodV1, NodQuoteContextV1,
};
use outbe_tee::TransportError;

use crate::nod_encryption::NodGroupLoadOpRequestV1;

pub(crate) fn read_amount(record: &EncryptedNodV1) -> Result<U256, TransportError> {
    #[cfg(any(test, feature = "test-enclave"))]
    if test_enclave::enabled() {
        return crate::nod_encryption::decrypt_nod(&test_enclave::state_key(), record)
            .map_err(TransportError::enclave_error);
    }
    crate::nod_host::read_nod_amount(record)
}

pub(crate) fn apply_group_load_operation(
    request: NodGroupLoadOpRequestV1,
) -> Result<EncryptedNodGroupLoadV1, TransportError> {
    #[cfg(any(test, feature = "test-enclave"))]
    if test_enclave::enabled() {
        return crate::nod_encryption::apply_nod_group_load_operation(
            &test_enclave::state_key(),
            &request,
        )
        .map_err(TransportError::enclave_error);
    }
    crate::nod_host::apply_nod_group_load_operation(request)
}

pub(crate) fn read_group_load(record: &EncryptedNodGroupLoadV1) -> Result<U256, TransportError> {
    #[cfg(any(test, feature = "test-enclave"))]
    if test_enclave::enabled() {
        return crate::nod_encryption::read_nod_group_load(&test_enclave::state_key(), record)
            .map_err(TransportError::enclave_error);
    }
    crate::nod_host::read_nod_group_load(record)
}

pub(crate) fn seal_group_load(
    chain_id: u64,
    scope: B256,
    sequence: u64,
    inputs_hash: B256,
    load_minor: U256,
) -> Result<EncryptedNodGroupLoadV1, TransportError> {
    #[cfg(any(test, feature = "test-enclave"))]
    if test_enclave::enabled() {
        return crate::nod_encryption::seal_group_load(
            &test_enclave::state_key(),
            (chain_id, scope),
            (sequence, inputs_hash),
            load_minor,
        )
        .map_err(TransportError::enclave_error);
    }
    crate::nod_host::seal_group_load((chain_id, scope), (sequence, inputs_hash), load_minor)
}

pub(crate) fn encrypt_nod_quote(
    nod: &EncryptedNodV1,
    context: NodQuoteContextV1,
    payment_minor: U256,
) -> Result<Vec<u8>, TransportError> {
    #[cfg(any(test, feature = "test-enclave"))]
    if test_enclave::enabled() {
        return crate::nod_encryption::encrypt_nod_quote(
            &test_enclave::state_key(),
            nod,
            &context,
            payment_minor,
        )
        .map_err(TransportError::enclave_error);
    }
    crate::nod_host::encrypt_nod_quote(nod, context, payment_minor)
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
    /// Opaque fixture capability; construction requires `outbe-tee/test-utils`.
    pub fn state_key() -> outbe_tee::resident::StateKeyHandle {
        outbe_tee::resident::StateKeyHandle::for_test([0x5a; 32])
    }
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
