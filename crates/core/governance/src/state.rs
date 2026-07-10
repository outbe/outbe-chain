use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_primitives::error::Result;

use crate::schema::{Gip, GipEntryExt, GovernanceContract, Oip, OipEntryExt};

/// Lightweight proposal projection for listings — every field except the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalMeta {
    pub id: U256,
    pub author: Address,
    pub status: u8,
    pub created_block: u64,
    pub updated_block: u64,
    pub text_hash: B256,
}

/// Storage key for the per-author id list: `keccak256(author ‖ index_be)`.
/// Shared by the writer (runtime submit) and the reader (below).
pub(crate) fn author_index_key(author: Address, index: u32) -> B256 {
    let mut buf = [0u8; 24];
    buf[..20].copy_from_slice(author.as_slice());
    buf[20..].copy_from_slice(&index.to_be_bytes());
    keccak256(buf)
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

    // --- proposal metadata projections (read only the fixed-width fields via the
    //     per-field accessors; never load the text data-run) ---

    fn oip_meta(&self, id: U256) -> Result<ProposalMeta> {
        let e = self.oips.entry(id);
        Ok(ProposalMeta {
            id,
            author: e.author().read()?,
            status: e.status().read()?,
            created_block: e.created_block().read()?,
            updated_block: e.updated_block().read()?,
            text_hash: e.text_hash().read()?,
        })
    }

    fn gip_meta(&self, id: U256) -> Result<ProposalMeta> {
        let e = self.gips.entry(id);
        Ok(ProposalMeta {
            id,
            author: e.author().read()?,
            status: e.status().read()?,
            created_block: e.created_block().read()?,
            updated_block: e.updated_block().read()?,
            text_hash: e.text_hash().read()?,
        })
    }

    // --- index-backed listings (read only the relevant bucket) ---

    pub fn oips_by_author(&self, author: Address) -> Result<Vec<ProposalMeta>> {
        let n = self.oip_author_count.read(&author)?;
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            let id = self.oip_author_ids.read(&author_index_key(author, i))?;
            out.push(self.oip_meta(id)?);
        }
        Ok(out)
    }

    pub fn gips_by_author(&self, author: Address) -> Result<Vec<ProposalMeta>> {
        let n = self.gip_author_count.read(&author)?;
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            let id = self.gip_author_ids.read(&author_index_key(author, i))?;
            out.push(self.gip_meta(id)?);
        }
        Ok(out)
    }

    /// OIPs that reached Approved or Implemented.
    pub fn accepted_oips(&self) -> Result<Vec<ProposalMeta>> {
        self.oip_accepted
            .read_all()?
            .into_iter()
            .map(|id| self.oip_meta(id))
            .collect()
    }

    /// GIPs that reached Approved or Implemented.
    pub fn accepted_gips(&self) -> Result<Vec<ProposalMeta>> {
        self.gip_accepted
            .read_all()?
            .into_iter()
            .map(|id| self.gip_meta(id))
            .collect()
    }

    pub fn rejected_oips(&self) -> Result<Vec<ProposalMeta>> {
        self.oip_rejected
            .read_all()?
            .into_iter()
            .map(|id| self.oip_meta(id))
            .collect()
    }

    pub fn rejected_gips(&self) -> Result<Vec<ProposalMeta>> {
        self.gip_rejected
            .read_all()?
            .into_iter()
            .map(|id| self.gip_meta(id))
            .collect()
    }
}
