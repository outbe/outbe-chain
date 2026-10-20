//! Shared proposal operations over the existing, separately typed OIP/GIP storage.

use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::error::Result;
use outbe_primitives::storage::dsl::{Map, RecordEntry, StorageRecord, Value};
use outbe_primitives::storage::types::{Mapping, Slot, StorageSet};

use crate::errors::GovernanceError;
use crate::precompile::IGovernance;
use crate::runtime::validate_text;
use crate::schema::{Gip, GipEntryExt, GovernanceContract, Oip, OipEntryExt};
use crate::state::{author_index_key, page_bounds, ProposalMeta};
use crate::status;

/// The record-specific storage bindings and wire events, not proposal policy.
/// Field accessors keep metadata and status operations independent of text size.
pub(crate) trait ProposalRecord: StorageRecord<Key = U256> {
    fn store<'a, 's>(contract: &'a GovernanceContract<'s>) -> ProposalStore<'a, 's, Self>;
    fn new(meta: ProposalMeta, text: String) -> Self;
    fn author_status(&self) -> (Address, u8);
    fn replace_text(&mut self, text: String, hash: B256, block: u64);

    fn author<'s>(entry: &RecordEntry<'s, U256, Self>) -> Slot<'s, Address>;
    fn status<'s>(entry: &RecordEntry<'s, U256, Self>) -> Slot<'s, u8>;
    fn created_block<'s>(entry: &RecordEntry<'s, U256, Self>) -> Slot<'s, u64>;
    fn updated_block<'s>(entry: &RecordEntry<'s, U256, Self>) -> Slot<'s, u64>;
    fn text_hash<'s>(entry: &RecordEntry<'s, U256, Self>) -> Slot<'s, B256>;

    fn submitted(id: U256, author: Address, hash: B256) -> impl SolEvent;
    fn text_updated(id: U256, hash: B256) -> impl SolEvent;
    fn status_changed(id: U256, old: u8, new: u8) -> impl SolEvent;
}

/// Borrows one record map together with its matching counters and indexes.
/// Only the two record adapters construct these bindings.
pub(crate) struct ProposalStore<'a, 's, R: ProposalRecord> {
    contract: &'a GovernanceContract<'s>,
    records: &'a Map<'s, U256, R>,
    next_id: &'a Value<'s, u64>,
    author_count: &'a Map<'s, Address, u32>,
    author_ids: &'a Map<'s, B256, U256>,
    by_status: &'a Mapping<'s, u8, StorageSet<'s, U256>>,
}

impl<'s> GovernanceContract<'s> {
    pub(crate) fn proposals<R: ProposalRecord>(&self) -> ProposalStore<'_, 's, R> {
        R::store(self)
    }
}

impl<R: ProposalRecord> ProposalStore<'_, '_, R> {
    pub(crate) fn get(&self, id: U256) -> Result<Option<R>> {
        self.records.get(id)
    }

    pub(crate) fn count(&self) -> Result<u64> {
        self.next_id.read()
    }

    pub(crate) fn count_by_author(&self, author: Address) -> Result<u32> {
        self.author_count.read(&author)
    }

    pub(crate) fn count_by_status(&self, status: u8) -> Result<u32> {
        if !status::is_valid_status(status) {
            return Err(GovernanceError::InvalidStatus.into());
        }
        self.by_status.get_set(&status).len()
    }

    pub(crate) fn by_author(
        &self,
        author: Address,
        offset: U256,
        limit: U256,
    ) -> Result<Vec<ProposalMeta>> {
        let (start, end) = page_bounds(self.author_count.read(&author)?, offset, limit);
        let mut rows = Vec::with_capacity((end - start) as usize);
        for index in start..end {
            let id = self.author_ids.read(&author_index_key(author, index))?;
            let entry = self.records.entry(id);
            if entry.exists()? {
                rows.push(Self::meta(&entry)?);
            }
        }
        Ok(rows)
    }

    pub(crate) fn by_status(
        &self,
        status: u8,
        offset: U256,
        limit: U256,
    ) -> Result<Vec<ProposalMeta>> {
        if !status::is_valid_status(status) {
            return Err(GovernanceError::InvalidStatus.into());
        }
        let set = self.by_status.get_set(&status);
        let (start, end) = page_bounds(set.len()?, offset, limit);
        let mut rows = Vec::with_capacity((end - start) as usize);
        for index in start..end {
            if let Some(id) = set.at(index)? {
                rows.push(Self::meta(&self.records.entry(id))?);
            }
        }
        Ok(rows)
    }

    pub(crate) fn submit(&self, author: Address, text: &str) -> Result<U256> {
        validate_text(text)?;
        // Refuse before any write so a full index cannot corrupt earlier entries.
        let author_count = self.author_count.read(&author)?;
        let next_author_count = author_count
            .checked_add(1)
            .ok_or(GovernanceError::AuthorIndexFull)?;
        let block = self.contract.storage.block_number()?;
        let next = self.next_id.read()? + 1;
        let id = U256::from(next);
        let hash = keccak256(text.as_bytes());
        let record = R::new(
            ProposalMeta {
                id,
                author,
                status: status::DRAFT,
                created_block: block,
                updated_block: block,
                text_hash: hash,
            },
            text.to_owned(),
        );
        self.records.create(&record)?;
        self.next_id.write(next)?;
        self.author_ids
            .write(&author_index_key(author, author_count), id)?;
        self.author_count.write(&author, next_author_count)?;
        self.by_status.get_set(&status::DRAFT).insert(id)?;
        self.emit(R::submitted(id, author, hash))?;
        Ok(id)
    }

    pub(crate) fn create_approved(&self, author: Address, text: &str) -> Result<U256> {
        let id = self.submit(author, text)?;
        self.apply_status(id, status::APPROVED)?;
        Ok(id)
    }

    pub(crate) fn update_text(&self, caller: Address, id: U256, text: &str) -> Result<()> {
        validate_text(text)?;
        let mut record = self
            .records
            .get(id)?
            .ok_or(GovernanceError::ProposalNotFound)?;
        let (author, current) = record.author_status();
        if author != caller {
            return Err(GovernanceError::NotAuthor.into());
        }
        if !status::text_editable(current) {
            return Err(GovernanceError::TextNotEditableInStatus.into());
        }
        let block = self.contract.storage.block_number()?;
        let hash = keccak256(text.as_bytes());
        record.replace_text(text.to_owned(), hash, block);
        // Preserve the record DSL's unchanged-text compare-skip behavior.
        self.records.update(&record)?;
        self.emit(R::text_updated(id, hash))
    }

    pub(crate) fn set_status(&self, caller: Address, id: U256, new_status: u8) -> Result<()> {
        let entry = self.records.entry(id);
        if !entry.exists()? {
            return Err(GovernanceError::ProposalNotFound.into());
        }
        let current = R::status(&entry).read()?;
        let author = R::author(&entry).read()?;
        status::validate_transition(current, new_status)?;
        let author_resubmit =
            current == status::REWORK && new_status == status::DRAFT && caller == author;
        if !author_resubmit && !self.contract.is_authority(caller)? {
            return Err(GovernanceError::NotAuthorized.into());
        }
        self.apply_status(id, new_status)
    }

    /// Called only after authorization, or from the approved Vote path.
    fn apply_status(&self, id: U256, new_status: u8) -> Result<()> {
        let entry = self.records.entry(id);
        if !entry.exists()? {
            return Err(GovernanceError::ProposalNotFound.into());
        }
        let current = R::status(&entry).read()?;
        status::validate_transition(current, new_status)?;
        let block = self.contract.storage.block_number()?;
        R::status(&entry).write(new_status)?;
        R::updated_block(&entry).write(block)?;
        self.by_status.get_set(&current).remove(&id)?;
        self.by_status.get_set(&new_status).insert(id)?;
        self.emit(R::status_changed(id, current, new_status))
    }

    fn meta(entry: &RecordEntry<'_, U256, R>) -> Result<ProposalMeta> {
        Ok(ProposalMeta {
            id: entry.key(),
            author: R::author(entry).read()?,
            status: R::status(entry).read()?,
            created_block: R::created_block(entry).read()?,
            updated_block: R::updated_block(entry).read()?,
            text_hash: R::text_hash(entry).read()?,
        })
    }

    fn emit(&self, event: impl SolEvent) -> Result<()> {
        self.contract
            .storage
            .emit_event(self.contract.address, event.encode_log_data())
    }
}

impl ProposalRecord for Oip {
    fn store<'a, 's>(contract: &'a GovernanceContract<'s>) -> ProposalStore<'a, 's, Self> {
        ProposalStore {
            contract,
            records: &contract.oips,
            next_id: &contract.next_oip_id,
            author_count: &contract.oip_author_count,
            author_ids: &contract.oip_author_ids,
            by_status: &contract.oip_by_status,
        }
    }

    fn new(meta: ProposalMeta, text: String) -> Self {
        Self {
            id: meta.id,
            author: meta.author,
            status: meta.status,
            created_block: meta.created_block,
            updated_block: meta.updated_block,
            text_hash: meta.text_hash,
            text,
        }
    }

    fn author_status(&self) -> (Address, u8) {
        (self.author, self.status)
    }

    fn replace_text(&mut self, text: String, hash: B256, block: u64) {
        self.text = text;
        self.text_hash = hash;
        self.updated_block = block;
    }

    fn author<'s>(entry: &RecordEntry<'s, U256, Self>) -> Slot<'s, Address> {
        entry.author()
    }

    fn status<'s>(entry: &RecordEntry<'s, U256, Self>) -> Slot<'s, u8> {
        entry.status()
    }

    fn created_block<'s>(entry: &RecordEntry<'s, U256, Self>) -> Slot<'s, u64> {
        entry.created_block()
    }

    fn updated_block<'s>(entry: &RecordEntry<'s, U256, Self>) -> Slot<'s, u64> {
        entry.updated_block()
    }

    fn text_hash<'s>(entry: &RecordEntry<'s, U256, Self>) -> Slot<'s, B256> {
        entry.text_hash()
    }

    fn submitted(id: U256, author: Address, hash: B256) -> impl SolEvent {
        IGovernance::OipSubmitted {
            id,
            author,
            textHash: hash,
        }
    }

    fn text_updated(id: U256, hash: B256) -> impl SolEvent {
        IGovernance::OipTextUpdated { id, textHash: hash }
    }

    fn status_changed(id: U256, old: u8, new: u8) -> impl SolEvent {
        IGovernance::OipStatusChanged {
            id,
            oldStatus: old,
            newStatus: new,
        }
    }
}

impl ProposalRecord for Gip {
    fn store<'a, 's>(contract: &'a GovernanceContract<'s>) -> ProposalStore<'a, 's, Self> {
        ProposalStore {
            contract,
            records: &contract.gips,
            next_id: &contract.next_gip_id,
            author_count: &contract.gip_author_count,
            author_ids: &contract.gip_author_ids,
            by_status: &contract.gip_by_status,
        }
    }

    fn new(meta: ProposalMeta, text: String) -> Self {
        Self {
            id: meta.id,
            author: meta.author,
            status: meta.status,
            created_block: meta.created_block,
            updated_block: meta.updated_block,
            text_hash: meta.text_hash,
            text,
        }
    }

    fn author_status(&self) -> (Address, u8) {
        (self.author, self.status)
    }

    fn replace_text(&mut self, text: String, hash: B256, block: u64) {
        self.text = text;
        self.text_hash = hash;
        self.updated_block = block;
    }

    fn author<'s>(entry: &RecordEntry<'s, U256, Self>) -> Slot<'s, Address> {
        entry.author()
    }

    fn status<'s>(entry: &RecordEntry<'s, U256, Self>) -> Slot<'s, u8> {
        entry.status()
    }

    fn created_block<'s>(entry: &RecordEntry<'s, U256, Self>) -> Slot<'s, u64> {
        entry.created_block()
    }

    fn updated_block<'s>(entry: &RecordEntry<'s, U256, Self>) -> Slot<'s, u64> {
        entry.updated_block()
    }

    fn text_hash<'s>(entry: &RecordEntry<'s, U256, Self>) -> Slot<'s, B256> {
        entry.text_hash()
    }

    fn submitted(id: U256, author: Address, hash: B256) -> impl SolEvent {
        IGovernance::GipSubmitted {
            id,
            author,
            textHash: hash,
        }
    }

    fn text_updated(id: U256, hash: B256) -> impl SolEvent {
        IGovernance::GipTextUpdated { id, textHash: hash }
    }

    fn status_changed(id: U256, old: u8, new: u8) -> impl SolEvent {
        IGovernance::GipStatusChanged {
            id,
            oldStatus: old,
            newStatus: new,
        }
    }
}
