use alloy_primitives::{Address, B256, U256};
use outbe_primitives::error::Result;

use crate::schema::{Gip, GovernanceContract, Oip};

impl GovernanceContract<'_> {
    // --- meta-canon / canon reads ---

    /// Returns `(text, version, hash)` for the meta-canon.
    pub fn get_meta_canon(&self) -> Result<(String, u64, B256)> {
        Ok((
            self.meta_canon.read_string()?,
            self.meta_canon_version.read()?,
            self.meta_canon_hash.read()?,
        ))
    }

    /// Returns `(text, version, hash)` for the canon.
    pub fn get_canon(&self) -> Result<(String, u64, B256)> {
        Ok((
            self.canon.read_string()?,
            self.canon_version.read()?,
            self.canon_hash.read()?,
        ))
    }

    /// Hash of a specific meta-canon revision (`0` if that version never existed).
    pub fn meta_canon_revision_hash(&self, version: u64) -> Result<B256> {
        self.meta_canon_revisions.read(&version)
    }

    /// Hash of a specific canon revision (`0` if that version never existed).
    pub fn canon_revision_hash(&self, version: u64) -> Result<B256> {
        self.canon_revisions.read(&version)
    }

    // --- proposal reads ---

    pub fn get_oip(&self, id: U256) -> Result<Option<Oip>> {
        self.oips.get(id)
    }

    pub fn get_gip(&self, id: U256) -> Result<Option<Gip>> {
        self.gips.get(id)
    }

    /// Number of OIPs ever submitted (ids run `1..=oip_count`).
    pub fn oip_count(&self) -> Result<u64> {
        self.next_oip_id.read()
    }

    /// Number of GIPs ever submitted (ids run `1..=gip_count`).
    pub fn gip_count(&self) -> Result<u64> {
        self.next_gip_id.read()
    }

    // --- authorities ---

    pub fn is_authority(&self, who: Address) -> Result<bool> {
        self.authorities.read(&who)
    }
}
