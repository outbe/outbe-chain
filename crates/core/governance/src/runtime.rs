use alloy_primitives::{keccak256, Address, U256};
use outbe_primitives::error::Result;

use crate::errors::GovernanceError;
use crate::precompile::IGovernance;
use crate::schema::{Gip, GovernanceContract, Oip};

/// Maximum size of any single normative / proposal text. Every write path
/// enforces it. This limit keeps a full-overwrite comfortably inside one block
/// under the permissioned gas model (flat 5000/SSTORE).
pub const MAX_TEXT_BYTES: usize = 128 * 1024;

pub(crate) fn validate_text(text: &str) -> Result<()> {
    if text.is_empty() {
        return Err(GovernanceError::EmptyText.into());
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err(GovernanceError::TextTooLarge.into());
    }
    Ok(())
}

impl GovernanceContract<'_> {
    // --- authority gate (live ValidatorSet) ---

    fn ensure_authority(&self, caller: Address) -> Result<()> {
        if self.is_authority(caller)? {
            Ok(())
        } else {
            Err(GovernanceError::NotAuthorized.into())
        }
    }

    // --- meta-canon / canon writes (authorities-gated, full overwrite) ---

    pub fn update_meta_canon(&mut self, caller: Address, text: &str) -> Result<u64> {
        self.ensure_authority(caller)?;
        validate_text(text)?;
        let version = self.meta_canon_version.read()? + 1;
        let hash = keccak256(text.as_bytes());
        self.meta_canon.write(text.as_bytes())?;
        self.meta_canon_version.write(version)?;
        self.meta_canon_hash.write(hash)?;
        self.meta_canon_revisions.write(&version, hash)?;
        self.emit(IGovernance::MetaCanonUpdated { version, hash })?;
        Ok(version)
    }

    pub fn update_canon(&mut self, caller: Address, text: &str) -> Result<u64> {
        self.ensure_authority(caller)?;
        validate_text(text)?;
        let version = self.canon_version.read()? + 1;
        let hash = keccak256(text.as_bytes());
        self.canon.write(text.as_bytes())?;
        self.canon_version.write(version)?;
        self.canon_hash.write(hash)?;
        self.canon_revisions.write(&version, hash)?;
        self.emit(IGovernance::CanonUpdated { version, hash })?;
        Ok(version)
    }

    // --- OIP / GIP operations ---

    /// Submits an OIP in Draft, open to any caller.
    pub fn submit_oip(&mut self, author: Address, text: &str) -> Result<U256> {
        self.proposals::<Oip>().submit(author, text)
    }

    /// Submits a GIP in Draft, open to any caller.
    pub fn submit_gip(&mut self, author: Address, text: &str) -> Result<U256> {
        self.proposals::<Gip>().submit(author, text)
    }

    /// Materializes an approved Vote decision without the direct-call authority gate.
    pub fn create_approved_oip(&mut self, author: Address, text: &str) -> Result<U256> {
        self.proposals::<Oip>().create_approved(author, text)
    }

    /// Materializes an approved Vote decision without the direct-call authority gate.
    pub fn create_approved_gip(&mut self, author: Address, text: &str) -> Result<U256> {
        self.proposals::<Gip>().create_approved(author, text)
    }

    /// Replaces text only for the author of a Draft or Rework proposal.
    pub fn update_oip_text(&mut self, caller: Address, id: U256, text: &str) -> Result<()> {
        self.proposals::<Oip>().update_text(caller, id, text)
    }

    /// Replaces text only for the author of a Draft or Rework proposal.
    pub fn update_gip_text(&mut self, caller: Address, id: U256, text: &str) -> Result<()> {
        self.proposals::<Gip>().update_text(caller, id, text)
    }

    /// Authority-gated, except an author may resubmit Rework as Draft.
    /// Status transitions never read or rewrite the proposal text.
    pub fn set_oip_status(&mut self, caller: Address, id: U256, new_status: u8) -> Result<()> {
        self.proposals::<Oip>().set_status(caller, id, new_status)
    }

    /// Authority-gated, except an author may resubmit Rework as Draft.
    /// Status transitions never read or rewrite the proposal text.
    pub fn set_gip_status(&mut self, caller: Address, id: U256, new_status: u8) -> Result<()> {
        self.proposals::<Gip>().set_status(caller, id, new_status)
    }
}
