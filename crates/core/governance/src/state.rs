use alloy_primitives::{Address, B256, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::keys::address_u32_key;
use outbe_validatorset::contract::ValidatorSet;

use crate::schema::{Gip, GovernanceContract, Oip};

/// Lightweight proposal projection for listings - every field except the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalMeta {
    pub id: U256,
    pub author: Address,
    pub status: u8,
    pub created_block: u64,
    pub updated_block: u64,
    pub text_hash: B256,
}

/// Storage key for the per-author id list: `keccak256(author || index_be)`.
/// Shared by the writer (runtime submit) and the reader (below).
pub(crate) fn author_index_key(author: Address, index: u32) -> B256 {
    address_u32_key(author, index)
}

/// Hard cap on how many items a single paginated read returns, so a caller
/// cannot ask for an unbounded page.
pub const MAX_PAGE: u32 = 1000;

/// Clamp an `(offset, limit)` request against a bucket of `total` items,
/// returning the half-open `[start, end)` index range to read.
pub(crate) fn page_bounds(total: u32, offset: U256, limit: U256) -> (u32, u32) {
    let start = offset.min(U256::from(total)).to::<u32>();
    let take = limit.min(U256::from(MAX_PAGE)).to::<u32>();
    let end = start.saturating_add(take).min(total);
    (start, end)
}

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
        self.proposals::<Oip>().get(id)
    }

    pub fn get_gip(&self, id: U256) -> Result<Option<Gip>> {
        self.proposals::<Gip>().get(id)
    }

    /// Number of OIPs ever submitted (ids run `1..=oip_count`).
    pub fn oip_count(&self) -> Result<u64> {
        self.proposals::<Oip>().count()
    }

    /// Number of GIPs ever submitted (ids run `1..=gip_count`).
    pub fn gip_count(&self) -> Result<u64> {
        self.proposals::<Gip>().count()
    }

    // --- authority ---

    /// `true` when `who` is an ACTIVE validator in the current `ValidatorSet`.
    /// Exited, jailed and pending validators are not authorities.
    pub fn is_authority(&self, who: Address) -> Result<bool> {
        Ok(ValidatorSet::new(self.storage.clone())
            .validator_lifecycle(who)?
            .is_active_status())
    }

    // --- index-backed listings (paginated: read only the [offset, offset+limit)
    //     slice of the relevant bucket. The counts let callers size the pages) ---

    pub fn oip_count_by_author(&self, author: Address) -> Result<u32> {
        self.proposals::<Oip>().count_by_author(author)
    }

    pub fn gip_count_by_author(&self, author: Address) -> Result<u32> {
        self.proposals::<Gip>().count_by_author(author)
    }

    pub fn oips_by_author(
        &self,
        author: Address,
        offset: U256,
        limit: U256,
    ) -> Result<Vec<ProposalMeta>> {
        self.proposals::<Oip>().by_author(author, offset, limit)
    }

    pub fn gips_by_author(
        &self,
        author: Address,
        offset: U256,
        limit: U256,
    ) -> Result<Vec<ProposalMeta>> {
        self.proposals::<Gip>().by_author(author, offset, limit)
    }

    /// Number of OIPs currently in `status` (0..=4).
    pub fn oip_count_by_status(&self, status: u8) -> Result<u32> {
        self.proposals::<Oip>().count_by_status(status)
    }

    /// Number of GIPs currently in `status` (0..=4).
    pub fn gip_count_by_status(&self, status: u8) -> Result<u32> {
        self.proposals::<Gip>().count_by_status(status)
    }

    /// OIPs currently in `status` (paginated).
    pub fn oips_by_status(
        &self,
        status: u8,
        offset: U256,
        limit: U256,
    ) -> Result<Vec<ProposalMeta>> {
        self.proposals::<Oip>().by_status(status, offset, limit)
    }

    /// GIPs currently in `status` (paginated).
    pub fn gips_by_status(
        &self,
        status: u8,
        offset: U256,
        limit: U256,
    ) -> Result<Vec<ProposalMeta>> {
        self.proposals::<Gip>().by_status(status, offset, limit)
    }
}
