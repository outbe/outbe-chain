use alloy_primitives::{B256, U256};
use outbe_primitives::storage::proof::FinalizedStateCheckpoint;
use outbe_primitives::{
    addresses::TRIBUTE_ADDRESS,
    error::{PrecompileError, Result},
    storage::{
        proof::{AccountStorageProof, StateProofLimits},
        readonly::ReadOnlyStorageProvider,
        types::Storable,
        StorageHandle,
    },
    time::WorldwideDay,
    tribute_encryption::EncryptedTributeV2,
    wwd_entity_id::derive_poseidon_entity_id,
    wwd_entity_id::WwdEntityId,
};
use serde::{Deserialize, Serialize};

use crate::{storage_records::StoredTributeEntryExt, TributeContract};

pub fn retained_source_binding(
    storage: &StorageHandle<'_>,
    subject: &TributeStateProofSubject,
    id: WwdEntityId,
) -> Result<B256> {
    let contract = TributeContract::new(storage.clone());
    let source = crate::state_api::get_retained_record(storage, id)?
        .ok_or_else(|| invalid("retained Tribute source is missing"))?;
    let tribute = source
        .encrypted()
        .ok_or_else(|| invalid("Tribute source is not encrypted"))?;
    let position = contract.day_id_position.read(&id)?;
    if position == 0 || position > subject.tribute_count {
        return Err(invalid("retained Tribute source position mismatch"));
    }
    let index = position - 1;
    let words = expected_record_words(&contract, subject, index, tribute)?;
    for (slot, expected) in &words {
        if storage.sload(TRIBUTE_ADDRESS, U256::from_be_bytes(slot.0))? != *expected {
            return Err(invalid("retained Tribute source does not match its index"));
        }
    }
    outbe_tee::tribute_source::source_binding(tribute, index, &words)
        .map_err(|error| invalid(error.to_string()))
}

/// The caller obtains this checkpoint from verified request finality.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TributeStateProofSubject {
    pub checkpoint: FinalizedStateCheckpoint,
    pub chain_id: u64,
    pub worldwide_day: WorldwideDay,
    pub tribute_count: u32,
}

impl From<outbe_tee::tribute_source::TributeSourceSubject> for TributeStateProofSubject {
    fn from(source: outbe_tee::tribute_source::TributeSourceSubject) -> Self {
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
            return Err(invalid("incomplete finalized Tribute proof subject"));
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
        return Err(invalid(
            "Tribute source identity does not match its subject",
        ));
    }
    let position = day_index
        .checked_add(1)
        .ok_or_else(|| invalid("Tribute source index overflow"))?;
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
        return Err(invalid("Tribute source storage slots overlap"));
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
    ) -> Result<outbe_tee::tribute_source::TributeStateSource> {
        Ok(outbe_tee::tribute_source::TributeStateSource {
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
            .map_err(|error| invalid(error.to_string()))?;
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
                return Err(invalid(
                    "Tribute proof does not open the exact source slots",
                ));
            }
            for (slot, word) in expected {
                if storage.sload(TRIBUTE_ADDRESS, U256::from_be_bytes(slot.0))? != word {
                    return Err(invalid("Tribute proof word does not match its source"));
                }
            }
            Ok(())
        })
    }
}

fn invalid(message: impl Into<String>) -> PrecompileError {
    PrecompileError::storage(message)
}

#[cfg(test)]
mod tests;
