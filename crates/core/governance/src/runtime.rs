use alloy_primitives::{keccak256, Address, U256};
use outbe_primitives::error::Result;

use crate::state::{author_index_key, page_bounds, ProposalMeta};

use crate::errors::GovernanceError;
use crate::precompile::IGovernance;
// Per-kind event types. This file imports them by name so that the macro can name
// them with a transparent `:ident` fragment. A `:path` fragment cannot sit in
// struct-literal position.
use crate::precompile::IGovernance::{
    GipStatusChanged, GipSubmitted, GipTextUpdated, OipStatusChanged, OipSubmitted, OipTextUpdated,
};
use crate::schema::{Gip, GipEntryExt, GovernanceContract, Oip, OipEntryExt};
use crate::status;

/// Maximum size of the canon and meta-canon texts. Only `update_canon` and
/// `update_meta_canon` use this bound. Both paths are authority-gated, so this
/// bound depends on the permissioned premise. At `SSTORE_RESET` (5000 gas) a
/// full overwrite writes 4096 words = 20.48M gas, about 68% of a 30M block.
pub const MAX_CANON_TEXT_BYTES: usize = 128 * 1024;

/// Maximum size of an OIP / GIP proposal text. The submit and text-update
/// paths use this bound. These paths are open to any caller, so this bound
/// does not depend on the permissioned premise. At `SSTORE_RESET` (5000 gas)
/// one write is 512 words = 2.56M gas, under 9% of a 30M block. The value
/// matches the zero-fee calldata cap `FREE_TX_DAILY_CALLDATA_BYTES` (16 KiB).
pub const MAX_PROPOSAL_TEXT_BYTES: usize = 16 * 1024;

fn validate_text(text: &str, max: usize) -> Result<()> {
    if text.is_empty() {
        return Err(GovernanceError::EmptyText.into());
    }
    if text.len() > max {
        return Err(GovernanceError::TextTooLarge {
            len: text.len(),
            max,
        }
        .into());
    }
    Ok(())
}

/// Generates proposal writes and author listings for one proposal kind.
/// OIP and GIP share this logic over separate typed maps and id counters.
/// Only the storage target, metadata projection and per-kind events differ.
macro_rules! impl_proposal_ops {
    (
        submit = $submit:ident,
        create_approved = $create_approved:ident,
        update = $update:ident,
        set_status = $set_status:ident,
        apply_status = $apply_status:ident,
        list_by_author = $list_by_author:ident,
        meta = $meta:ident,
        record = $record:ident,
        map = $map:ident,
        counter = $counter:ident,
        submitted_event = $submitted_event:ident,
        text_event = $text_event:ident,
        status_event = $status_event:ident,
        by_status = $by_status:ident,
        author_count = $author_count:ident,
        author_ids = $author_ids:ident $(,)?
    ) => {
        /// Submits a new proposal in `Draft`, open to any caller.
        pub fn $submit(&mut self, author: Address, text: &str) -> Result<U256> {
            validate_text(text, MAX_PROPOSAL_TEXT_BYTES)?;
            // Refuse the submit before any write when the author index is full.
            // A wrapped counter would overwrite index slot 0.
            let acount = self.$author_count.read(&author)?;
            let next_acount = acount
                .checked_add(1)
                .ok_or(GovernanceError::AuthorIndexFull)?;
            let block = self.storage.block_number()?;
            let next = self
                .$counter
                .read()?
                .checked_add(1)
                .ok_or(GovernanceError::ProposalIdExhausted)?;
            let id = U256::from(next);
            let text_hash = keccak256(text.as_bytes());
            let record = $record {
                id,
                author,
                status: status::DRAFT,
                created_block: block,
                updated_block: block,
                text_hash,
                text: text.to_string(),
            };
            self.$map.create(&record)?;
            self.$counter.write(next)?;
            // author index (append-only): append id to the author's list.
            self.$author_ids
                .write(&author_index_key(author, acount), id)?;
            self.$author_count.write(&author, next_acount)?;
            // status index: a new proposal joins the Draft bucket.
            self.$by_status.get_set(&status::DRAFT).insert(id)?;
            self.emit($submitted_event {
                id,
                author,
                textHash: text_hash,
            })?;
            Ok(id)
        }

        /// Lists an author-index page, omitting entries whose records are missing.
        pub fn $list_by_author(
            &self,
            author: Address,
            offset: U256,
            limit: U256,
        ) -> Result<Vec<ProposalMeta>> {
            let (start, end) = page_bounds(self.$author_count.read(&author)?, offset, limit);
            let mut out = Vec::with_capacity((end - start) as usize);
            for i in start..end {
                let id = self.$author_ids.read(&author_index_key(author, i))?;
                if self.$map.entry(id).exists()? {
                    out.push(self.$meta(id)?);
                }
            }
            Ok(out)
        }

        /// Creates a proposal already in `Approved`.
        ///
        /// The vote path ([`crate::vote_target::GovernanceVoteTarget`]) uses
        /// this after quorum. The public ABI submit path does not expose it.
        ///
        /// It submits the proposal (Draft), then moves it Draft -> Approved.
        /// This is the same status write as set-status, without the ABI
        /// authority gate.
        pub fn $create_approved(&mut self, author: Address, text: &str) -> Result<U256> {
            let id = self.$submit(author, text)?;
            self.$apply_status(id, status::APPROVED)?;
            Ok(id)
        }

        /// Replaces a proposal's text. Author-only, and only while the proposal
        /// is `Draft` or `Rework`.
        pub fn $update(&mut self, caller: Address, id: U256, text: &str) -> Result<()> {
            validate_text(text, MAX_PROPOSAL_TEXT_BYTES)?;
            let mut record = self
                .$map
                .get(id)?
                .ok_or(GovernanceError::ProposalNotFound)?;
            if record.author != caller {
                return Err(GovernanceError::NotAuthor.into());
            }
            if !status::text_editable(record.status) {
                return Err(GovernanceError::TextNotEditableInStatus.into());
            }
            let block = self.storage.block_number()?;
            let text_hash = keccak256(text.as_bytes());
            record.text = text.to_string();
            record.text_hash = text_hash;
            record.updated_block = block;
            // Full-record update. The text slot compare-skips when unchanged.
            self.$map.update(&record)?;
            self.emit($text_event {
                id,
                textHash: text_hash,
            })?;
            Ok(())
        }

        /// Transitions a proposal's status. Authorities-gated, with one
        /// exception: the author may perform `Rework -> Draft` (resubmission).
        /// It touches only the status/updated-block slots, never the text.
        pub fn $set_status(&mut self, caller: Address, id: U256, new_status: u8) -> Result<()> {
            let entry = self.$map.entry(id);
            if !entry.exists()? {
                return Err(GovernanceError::ProposalNotFound.into());
            }
            let current = entry.status().read()?;
            let author = entry.author().read()?;
            status::validate_transition(current, new_status)?;

            let author_resubmit =
                current == status::REWORK && new_status == status::DRAFT && caller == author;
            if !author_resubmit && !self.is_authority(caller)? {
                return Err(GovernanceError::NotAuthorized.into());
            }

            self.$apply_status(id, new_status)
        }

        /// Status write + index move + event. The caller must gate
        /// authorization before this call (or be the vote path).
        fn $apply_status(&mut self, id: U256, new_status: u8) -> Result<()> {
            let entry = self.$map.entry(id);
            if !entry.exists()? {
                return Err(GovernanceError::ProposalNotFound.into());
            }
            let current = entry.status().read()?;
            status::validate_transition(current, new_status)?;

            let block = self.storage.block_number()?;
            let entry = self.$map.entry(id);
            entry.status().write(new_status)?;
            entry.updated_block().write(block)?;
            // status index: move the id from its old status bucket to the new one.
            self.$by_status.get_set(&current).remove(&id)?;
            self.$by_status.get_set(&new_status).insert(id)?;
            self.emit($status_event {
                id,
                oldStatus: current,
                newStatus: new_status,
            })?;
            Ok(())
        }
    };
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
        validate_text(text, MAX_CANON_TEXT_BYTES)?;
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
        validate_text(text, MAX_CANON_TEXT_BYTES)?;
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

    impl_proposal_ops!(
        submit = submit_oip,
        create_approved = create_approved_oip,
        update = update_oip_text,
        set_status = set_oip_status,
        apply_status = apply_oip_status,
        list_by_author = oips_by_author,
        meta = oip_meta,
        record = Oip,
        map = oips,
        counter = next_oip_id,
        submitted_event = OipSubmitted,
        text_event = OipTextUpdated,
        status_event = OipStatusChanged,
        by_status = oip_by_status,
        author_count = oip_author_count,
        author_ids = oip_author_ids,
    );

    impl_proposal_ops!(
        submit = submit_gip,
        create_approved = create_approved_gip,
        update = update_gip_text,
        set_status = set_gip_status,
        apply_status = apply_gip_status,
        list_by_author = gips_by_author,
        meta = gip_meta,
        record = Gip,
        map = gips,
        counter = next_gip_id,
        submitted_event = GipSubmitted,
        text_event = GipTextUpdated,
        status_event = GipStatusChanged,
        by_status = gip_by_status,
        author_count = gip_author_count,
        author_ids = gip_author_ids,
    );
}
