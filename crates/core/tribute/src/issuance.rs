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

    /// Stores the encrypted record with an explicit league (`1..=4096`).
    pub fn issue_encrypted_with_league(
        &mut self,
        body: &outbe_primitives::tribute_encryption::EncryptedTributeV2,
        league: Option<u16>,
    ) -> Result<()> {
        crate::state_api::issue_encrypted_with_league(&self.storage_handle(), body, league)
    }

    /// Encrypts numerical fixtures before they enter the production storage path.
    #[cfg(any(test, feature = "test-enclave"))]
    pub fn issue(&mut self, tribute: &crate::TributeData) -> Result<()> {
        self.issue_with_league(tribute, None)
    }

    /// Encrypts numerical fixtures with an optional explicit league (`1..=4096`).
    #[cfg(any(test, feature = "test-enclave"))]
    pub fn issue_with_league(
        &mut self,
        tribute: &crate::TributeData,
        league: Option<u16>,
    ) -> Result<()> {
        crate::state::validate_tribute_for_issue(tribute)?;
        let body = crate::enclave_client::test_enclave::encrypted_fixture(
            tribute,
            self.storage_handle().chain_id()?,
        )?;
        self.issue_encrypted_with_league(&body, league)
    }
}
