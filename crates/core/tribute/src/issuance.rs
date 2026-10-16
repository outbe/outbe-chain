//! Atomic storage of enclave-produced encrypted Tribute records.

use outbe_primitives::error::Result;

use crate::TributeContract;

impl TributeContract<'_> {
    /// Stores the exact encrypted record returned by the enclave.
    pub fn issue_encrypted(
        &mut self,
        body: &outbe_primitives::tribute_encryption::EncryptedTributeV2,
    ) -> Result<()> {
        crate::state_api::issue_encrypted(&self.storage_handle(), body)
    }

    /// Encrypts numerical fixtures before they enter the production storage path.
    #[cfg(any(test, feature = "test-enclave"))]
    pub fn issue(&mut self, tribute: &crate::TributeData) -> Result<()> {
        crate::state::validate_tribute_for_issue(tribute)?;
        let body = crate::enclave_client::test_enclave::encrypted_fixture(
            tribute,
            self.storage_handle().chain_id()?,
        )?;
        self.issue_encrypted(&body)
    }
}
