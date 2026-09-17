//! Test-only adapter; production transport is `outbe_tee::pledge_ledger`.
#[cfg(any(test, feature = "test-enclave"))]
pub mod test_enclave {
    pub const DEV_CHAIN_ID: u64 = outbe_tee_enclave::dev::PLEDGE_CHAIN_ID;
    pub fn dev_chain() -> alloy_primitives::B256 {
        outbe_tee_enclave::dev::pledge_chain()
    }
    pub fn state_key() -> [u8; 32] {
        outbe_tee_enclave::dev::pledge_state_key().expect("dev ledger key")
    }
    pub fn install() {
        let mut ledger = outbe_tee_enclave::pledgenote::Ledger::default();
        outbe_tee::pledge_ledger::test_backend::install(Box::new(move |request| {
            outbe_tee_enclave::dev::pledge_request(&mut ledger, request)
        }));
    }
    pub fn uninstall() {
        outbe_tee::pledge_ledger::test_backend::uninstall();
    }
}
