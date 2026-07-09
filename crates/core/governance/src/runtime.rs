use alloy_primitives::{keccak256, Address, U256};
use outbe_primitives::error::Result;

use crate::errors::GovernanceError;
use crate::precompile::IGovernance;
use crate::schema::{Gip, GipEntryExt, GovernanceContract, Oip, OipEntryExt};
use crate::status;

/// Maximum size of any single normative / proposal text, enforced on every
/// write path. Keeps a full-overwrite comfortably inside one block under the
/// permissioned gas model (flat 5000/SSTORE).
pub const MAX_TEXT_BYTES: usize = 128 * 1024;

fn validate_text(text: &str) -> Result<()> {
    if text.is_empty() {
        return Err(GovernanceError::EmptyText.into());
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err(GovernanceError::TextTooLarge.into());
    }
    Ok(())
}

/// Generates the submit / update-text / set-status methods for one proposal
/// kind. OIP and GIP share this exact logic over separate typed maps and id
/// counters; only the storage target and the event `kind` tag differ.
macro_rules! impl_proposal_ops {
    (
        submit = $submit:ident,
        update = $update:ident,
        set_status = $set_status:ident,
        record = $record:ident,
        map = $map:ident,
        counter = $counter:ident,
        kind = $kind:expr $(,)?
    ) => {
        /// Submits a new proposal in `Draft`, open to any caller.
        pub fn $submit(&mut self, author: Address, text: &str) -> Result<U256> {
            validate_text(text)?;
            let block = self.storage.block_number()?;
            let next = self.$counter.read()? + 1;
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
            self.emit(IGovernance::ProposalSubmitted {
                id,
                kind: $kind,
                author,
                textHash: text_hash,
            })?;
            Ok(id)
        }

        /// Replaces a proposal's text. Author-only, and only while the proposal
        /// is `Draft` or `Rework`.
        pub fn $update(&mut self, caller: Address, id: U256, text: &str) -> Result<()> {
            validate_text(text)?;
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
            // Full-record update; the text slot compare-skips when unchanged.
            self.$map.update(&record)?;
            self.emit(IGovernance::ProposalTextUpdated {
                id,
                kind: $kind,
                textHash: text_hash,
            })?;
            Ok(())
        }

        /// Transitions a proposal's status. Authorities-gated, with one
        /// exception: the author may perform `Rework -> Draft` (resubmission).
        /// Touches only the status/updated-block slots — never the text.
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

            let block = self.storage.block_number()?;
            let entry = self.$map.entry(id);
            entry.status().write(new_status)?;
            entry.updated_block().write(block)?;
            self.emit(IGovernance::ProposalStatusChanged {
                id,
                kind: $kind,
                oldStatus: current,
                newStatus: new_status,
            })?;
            Ok(())
        }
    };
}

impl GovernanceContract<'_> {
    // --- authorities gate ---

    fn ensure_authority(&self, caller: Address) -> Result<()> {
        if self.is_authority(caller)? {
            Ok(())
        } else {
            Err(GovernanceError::NotAuthorized.into())
        }
    }

    /// Adds an authority. Only an existing authority may add another (used by
    /// tests and genesis-adjacent tooling; the genesis seed writes the initial
    /// set directly).
    pub fn add_authority(&mut self, caller: Address, who: Address) -> Result<()> {
        self.ensure_authority(caller)?;
        self.authorities.write(&who, true)
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

    impl_proposal_ops!(
        submit = submit_oip,
        update = update_oip_text,
        set_status = set_oip_status,
        record = Oip,
        map = oips,
        counter = next_oip_id,
        kind = 0u8,
    );

    impl_proposal_ops!(
        submit = submit_gip,
        update = update_gip_text,
        set_status = set_gip_status,
        record = Gip,
        map = gips,
        counter = next_gip_id,
        kind = 1u8,
    );
}
