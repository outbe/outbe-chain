//! Test-only adapter; production transport is `outbe_tee::pledge_ledger`.
#[cfg(any(test, feature = "test-enclave"))]
pub mod test_enclave {
    pub const DEV_CHAIN_ID: u64 = outbe_tee_enclave::dev::FIDELITY_CHAIN_ID;
    pub fn dev_chain() -> alloy_primitives::B256 {
        outbe_tee_enclave::dev::fidelity_chain()
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

    pub fn owner_envelope(
        storage: &outbe_primitives::storage::StorageHandle<'_>,
        account: alloy_primitives::Address,
        nonce: u64,
        action: outbe_tee::pledgenote::OwnerAction,
    ) -> Vec<u8> {
        use alloy_primitives::{B256, U256};
        use outbe_tee::pledgenote::*;
        let chain_id = B256::from(U256::from(storage.chain_id().unwrap()));
        let key = outbe_tee_enclave::gratis::derive_modify_key(&state_key(), account).unwrap();
        let mac = owner_mac(&key, chain_id, account, nonce, &action).unwrap();
        encrypt_request(
            outbe_tee_enclave::crypto::x25519_public(&outbe_tee_enclave::dev::PLEDGE_OFFER_SECRET),
            &PrivateRequest::Owner {
                chain_id,
                account,
                nonce,
                action,
                mac,
            },
        )
        .unwrap()
    }

    pub fn query(
        storage: &outbe_primitives::storage::StorageHandle<'_>,
        account: alloy_primitives::Address,
    ) -> outbe_tee::pledgenote::Receipt {
        let envelope = owner_envelope(
            storage,
            account,
            0,
            outbe_tee::pledgenote::OwnerAction::Query,
        );
        let bytes = crate::api::query(storage, envelope).unwrap();
        let view = outbe_tee_enclave::gratis::derive_view_key(&state_key(), account).unwrap();
        outbe_tee::pledgenote::decrypt_receipt(&view, &bytes).unwrap()
    }
}
