use alloy_primitives::{B256, U256};
use outbe_core_common::{
    tribute_encryption::EncryptedTributeV2,
    wwd_entity_id::{derive_poseidon_entity_id, WwdEntityId},
};
use outbe_primitives::storage::proof::FinalizedStateCheckpoint;
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::{
        proof::{AccountStorageProof, StateProofError, StateProofLimits},
        readonly::ReadOnlyStorageProvider,
        types::Storable,
        StorageHandle,
    },
};
use outbe_system_common::addresses::TRIBUTE_ADDRESS;
use outbe_system_common::time::WorldwideDay;
use serde::{Deserialize, Serialize};

use crate::{storage_records::StoredTributeEntryExt, TributeContract};

pub fn retained_source_binding(
    storage: &StorageHandle<'_>,
    subject: &TributeStateProofSubject,
    id: WwdEntityId,
) -> Result<B256> {
    let contract = TributeContract::new(storage.clone());
    let source = crate::state_api::get_retained_record(storage, id)?
        .ok_or(TributeStateProofError::RetainedSourceMissing)?;
    let tribute = source
        .encrypted()
        .ok_or(TributeStateProofError::SourceNotEncrypted)?;
    let position = contract.day_id_position.read(&id)?;
    if position == 0 || position > subject.tribute_count {
        return Err(TributeStateProofError::RetainedSourcePositionMismatch.into());
    }
    let index = position - 1;
    let words = expected_record_words(&contract, subject, index, tribute)?;
    for (slot, expected) in &words {
        if storage.sload(TRIBUTE_ADDRESS, U256::from_be_bytes(slot.0))? != *expected {
            return Err(TributeStateProofError::RetainedSourceIndexMismatch.into());
        }
    }
    Ok(crate::confidential::source_binding(tribute, index, &words)
        .map_err(TributeStateProofError::SourceBinding)?)
}

/// Encrypted tribute record, its day index, and expected `(slot, word)` storage pairs.
pub type TributeSourceWords = (EncryptedTributeV2, u32, Vec<(B256, U256)>);

/// Reads the encrypted tribute record, its day index, and expected storage words for `tribute_id`.
pub fn read_tribute_source_words(
    storage: StorageHandle<'_>,
    subject: &crate::confidential::TributeSourceSubject,
    tribute_id: WwdEntityId,
) -> Result<TributeSourceWords> {
    let record = crate::state_api::get_retained_record(&storage, tribute_id)?
        .ok_or(TributeStateProofError::SourceMissing)?;
    let tribute = record
        .encrypted()
        .ok_or(TributeStateProofError::SourceNotEncrypted)?
        .clone();
    let contract = TributeContract::new(storage);
    let position = contract.day_id_position.read(&tribute_id)?;
    if position == 0 || position > subject.tribute_count {
        return Err(TributeStateProofError::SourcePositionMismatch.into());
    }
    let day_index = position - 1;
    let node_subject = TributeStateProofSubject::from(*subject);
    let words = expected_record_words(&contract, &node_subject, day_index, &tribute)?;
    Ok((tribute, day_index, words))
}

/// The caller obtains this checkpoint from verified request finality.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TributeStateProofSubject {
    pub checkpoint: FinalizedStateCheckpoint,
    pub chain_id: u64,
    pub worldwide_day: WorldwideDay,
    pub tribute_count: u32,
}

impl From<crate::confidential::TributeSourceSubject> for TributeStateProofSubject {
    fn from(source: crate::confidential::TributeSourceSubject) -> Self {
        Self {
            checkpoint: FinalizedStateCheckpoint {
                block_number: source.checkpoint.block_number,
                block_hash: source.checkpoint.block_hash,
                state_root: source.checkpoint.state_root,
            },
            chain_id: source.chain_id,
            worldwide_day: source.worldwide_day,
            tribute_count: source.tribute_count,
        }
    }
}

impl TributeStateProofSubject {
    fn checkpoint_is_complete(&self) -> bool {
        self.checkpoint.block_number != 0
            && !self.checkpoint.block_hash.is_zero()
            && !self.checkpoint.state_root.is_zero()
    }

    fn is_complete(&self) -> bool {
        self.chain_id != 0 && self.worldwide_day.value() != 0 && self.checkpoint_is_complete()
    }

    fn validate(&self) -> Result<()> {
        if !self.is_complete() {
            return Err(TributeStateProofError::IncompleteSubject.into());
        }
        Ok(())
    }

    fn matches_context(&self, day_index: u32, tribute: &EncryptedTributeV2) -> bool {
        let context = &tribute.context;
        day_index < self.tribute_count
            && tribute.has_valid_encoding()
            && (context.chain_id, context.worldwide_day) == (self.chain_id, self.worldwide_day)
            && !context.owner.is_zero()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TributeRecordStateProof {
    pub day_index: u32,
    pub tribute: EncryptedTributeV2,
    pub account: AccountStorageProof,
}

/// Derives every expected word from the module-owned storage DSL.
pub fn expected_record_words(
    contract: &TributeContract<'_>,
    subject: &TributeStateProofSubject,
    day_index: u32,
    tribute: &EncryptedTributeV2,
) -> Result<Vec<(B256, U256)>> {
    subject.validate()?;
    let context = &tribute.context;
    let expected_id = derive_poseidon_entity_id(context.owner, context.worldwide_day);
    if !subject.matches_context(day_index, tribute) || expected_id != context.tribute_id {
        return Err(TributeStateProofError::SourceIdentityMismatch.into());
    }
    let position = day_index
        .checked_add(1)
        .ok_or(TributeStateProofError::SourceIndexOverflow)?;
    let entry = contract.records.entry(context.tribute_id);
    let day_ids = contract.day_ids.list(&subject.worldwide_day);
    let mut words = vec![
        (day_ids.base_slot(), U256::from(subject.tribute_count)),
        (day_ids.data_slot(day_index), context.tribute_id.to_word()),
        (
            contract.day_id_position.slot(&context.tribute_id).slot(),
            U256::from(position),
        ),
        (entry.owner().slot(), context.owner.to_word()),
        (entry.chain_id().slot(), context.chain_id.to_word()),
        (
            entry.worldwide_day().slot(),
            context.worldwide_day.to_word(),
        ),
        (
            entry.issuance_currency().slot(),
            context.issuance_currency.to_word(),
        ),
        (
            entry.reference_currency().slot(),
            context.reference_currency.to_word(),
        ),
        (
            entry.tribute_price_minor().slot(),
            context.tribute_price_minor,
        ),
        (
            entry.offer_input_hash().slot(),
            context.offer_input_hash.to_word(),
        ),
    ];
    words.extend(
        entry
            .encrypted_amounts()
            .encoded_words(&tribute.encrypted_amounts),
    );
    words.sort_unstable_by_key(|(slot, _)| *slot);
    if words.windows(2).any(|words| words[0].0 == words[1].0) {
        return Err(TributeStateProofError::StorageSlotsOverlap.into());
    }
    Ok(words
        .into_iter()
        .map(|(slot, word)| (B256::from(slot.to_be_bytes::<32>()), word))
        .collect())
}

impl TributeRecordStateProof {
    pub fn source_opening(
        &self,
        contract: &TributeContract<'_>,
        subject: &TributeStateProofSubject,
    ) -> Result<crate::confidential::TributeStateSource> {
        Ok(crate::confidential::TributeStateSource {
            tribute: self.tribute.clone(),
            day_index: self.day_index,
            expected_words: expected_record_words(
                contract,
                subject,
                self.day_index,
                &self.tribute,
            )?,
            proof: self.account.clone(),
        })
    }

    pub fn verify(
        &self,
        subject: &TributeStateProofSubject,
        limits: StateProofLimits,
    ) -> Result<()> {
        subject.validate()?;
        let verified = self
            .account
            .verify(TRIBUTE_ADDRESS, subject.checkpoint.state_root, limits)
            .map_err(TributeStateProofError::AccountProof)?;
        let mut provider = ReadOnlyStorageProvider::new_with_chain_identity(
            verified,
            subject.chain_id,
            B256::ZERO,
        );
        StorageHandle::enter(&mut provider, |storage| {
            let contract = TributeContract::new(storage.clone());
            let expected =
                expected_record_words(&contract, subject, self.day_index, &self.tribute)?;
            if expected.len() != self.account.slots.len()
                || expected
                    .iter()
                    .zip(&self.account.slots)
                    .any(|((slot, _), opening)| *slot != opening.slot)
            {
                return Err(TributeStateProofError::ProofSlotMismatch.into());
            }
            for (slot, word) in expected {
                if storage.sload(TRIBUTE_ADDRESS, U256::from_be_bytes(slot.0))? != word {
                    return Err(TributeStateProofError::ProofWordMismatch.into());
                }
            }
            Ok(())
        })
    }
}

#[derive(Debug, thiserror::Error)]
enum TributeStateProofError {
    #[error("retained Tribute source is missing")]
    RetainedSourceMissing,
    #[error("Tribute source is missing")]
    SourceMissing,
    #[error("Tribute source is not encrypted")]
    SourceNotEncrypted,
    #[error("retained Tribute source position mismatch")]
    RetainedSourcePositionMismatch,
    #[error("Tribute source position mismatch")]
    SourcePositionMismatch,
    #[error("retained Tribute source does not match its index")]
    RetainedSourceIndexMismatch,
    #[error("incomplete finalized Tribute proof subject")]
    IncompleteSubject,
    #[error("Tribute source identity does not match its subject")]
    SourceIdentityMismatch,
    #[error("Tribute source index overflow")]
    SourceIndexOverflow,
    #[error("Tribute source storage slots overlap")]
    StorageSlotsOverlap,
    #[error("Tribute proof does not open the exact source slots")]
    ProofSlotMismatch,
    #[error("Tribute proof word does not match its source")]
    ProofWordMismatch,
    #[error("{0}")]
    SourceBinding(#[source] outbe_tee::TransportError),
    #[error("{0}")]
    AccountProof(#[source] StateProofError),
}

impl From<TributeStateProofError> for PrecompileError {
    fn from(error: TributeStateProofError) -> Self {
        PrecompileError::storage(error.to_string())
    }
}

#[cfg(test)]
mod tests;
