//! Historical state proof serving for enclave-initiated Tribute reads.

use super::*;
use outbe_primitives::{
    addresses::TRIBUTE_ADDRESS, storage::proof::AccountStorageProof, wwd_entity_id::WwdEntityId,
};
use outbe_tee::tribute_source::{TributeSourceSubject, TributeStateSource};
use outbe_tribute::state_proof::{expected_record_words, TributeStateProofSubject};
use reth_trie::TrieInput;

pub(super) fn serve<P>(
    provider: &P,
    chain_identity: Option<(u64, B256)>,
    subject: TributeSourceSubject,
    tribute_id: WwdEntityId,
) -> RpcResult<TributeStateSource>
where
    P: StateProviderFactory + HeaderProvider<Header = OutbeHeader> + BlockIdReader,
{
    if tribute_id.worldwide_day() != subject.worldwide_day {
        return Err(invalid_params("Tribute source day mismatch"));
    }
    let (chain_id, genesis_hash, state) = checkpoint_state(provider, chain_identity, subject)?;
    let node_subject = TributeStateProofSubject::from(subject);
    let reader = RethStateReader { state: &state };
    let mut readonly =
        ReadOnlyStorageProvider::new_with_chain_identity(reader, chain_id, genesis_hash);
    let (tribute, day_index, expected_words) = StorageHandle::enter(&mut readonly, |storage| {
        let record = outbe_tribute::state_api::get_retained_record(&storage, tribute_id)?
            .ok_or_else(|| {
                outbe_primitives::error::PrecompileError::storage("Tribute source is missing")
            })?;
        let tribute = record
            .encrypted()
            .ok_or_else(|| {
                outbe_primitives::error::PrecompileError::storage("Tribute source is not encrypted")
            })?
            .clone();
        let contract = outbe_tribute::TributeContract::new(storage);
        let position = contract.day_id_position.read(&tribute_id)?;
        if position == 0 || position > subject.tribute_count {
            return Err(outbe_primitives::error::PrecompileError::storage(
                "Tribute source position mismatch",
            ));
        }
        let day_index = position - 1;
        let words = expected_record_words(&contract, &node_subject, day_index, &tribute)?;
        Ok((tribute, day_index, words))
    })
    .map_err(internal_err)?;
    let slots = expected_words
        .iter()
        .map(|(slot, _)| *slot)
        .collect::<Vec<_>>();
    let proof = state
        .proof(TrieInput::default(), TRIBUTE_ADDRESS, &slots)
        .map_err(internal_err)?;
    let source = TributeStateSource {
        tribute,
        day_index,
        expected_words,
        proof: AccountStorageProof::try_from(&proof).map_err(internal_err)?,
    };
    source
        .verify(
            &subject,
            source.binding().map_err(internal_err)?,
            outbe_tee::tribute_source::proof_limits(),
        )
        .map_err(internal_err)?;
    Ok(source)
}

fn checkpoint_state<P>(
    provider: &P,
    chain_identity: Option<(u64, B256)>,
    subject: TributeSourceSubject,
) -> RpcResult<(u64, B256, StateProviderBox)>
where
    P: StateProviderFactory + HeaderProvider<Header = OutbeHeader> + BlockIdReader,
{
    let (chain_id, genesis_hash) = chain_identity
        .filter(|(chain_id, genesis_hash)| *chain_id != 0 && !genesis_hash.is_zero())
        .ok_or_else(|| internal_err("immutable chain identity is not configured"))?;
    if chain_id != subject.chain_id || subject.worldwide_day.value() == 0 {
        return Err(invalid_params("Tribute source chain or day mismatch"));
    }
    let checkpoint = subject.checkpoint;
    let finalized = provider
        .finalized_block_num_hash()
        .map_err(internal_err)?
        .ok_or_else(|| internal_err("finalized state unavailable"))?;
    if checkpoint.block_number == 0 || checkpoint.block_number > finalized.number {
        return Err(invalid_params("Tribute source checkpoint is not finalized"));
    }
    let header = provider
        .sealed_header(checkpoint.block_number)
        .map_err(internal_err)?
        .ok_or_else(|| internal_err("Tribute source header unavailable"))?;
    if header.hash() != checkpoint.block_hash || header.state_root() != checkpoint.state_root {
        return Err(invalid_params(
            "Tribute source checkpoint does not match the canonical header",
        ));
    }
    let state = provider
        .state_by_block_hash(checkpoint.block_hash)
        .map_err(internal_err)?;
    Ok((chain_id, genesis_hash, state))
}

pub(super) fn serve_day<P>(
    provider: &P,
    chain_identity: Option<(u64, B256)>,
    subject: TributeSourceSubject,
    day_index: Option<u32>,
) -> RpcResult<outbe_tribute::state_proof::TributeDayStateProof>
where
    P: StateProviderFactory + HeaderProvider<Header = OutbeHeader> + BlockIdReader,
{
    use outbe_tribute::state_proof::{
        TributeDayProofVerifier, TributeDayStateProof, TributeRecordStateProof,
    };
    let (chain_id, genesis_hash, state) = checkpoint_state(provider, chain_identity, subject)?;
    let reader = RethStateReader { state: &state };
    let mut readonly =
        ReadOnlyStorageProvider::new_with_chain_identity(reader, chain_id, genesis_hash);
    let (length_slot, tribute_id) = StorageHandle::enter(&mut readonly, |storage| {
        let contract = outbe_tribute::TributeContract::new(storage);
        let ids = contract.day_ids.list(&subject.worldwide_day);
        if ids.len()? != subject.tribute_count {
            return Err(outbe_primitives::error::PrecompileError::storage(
                "Tribute day count mismatch",
            ));
        }
        let id = match day_index {
            Some(index) if index < subject.tribute_count => {
                Some(ids.get(index)?.ok_or_else(|| {
                    outbe_primitives::error::PrecompileError::storage(
                        "Tribute day index is missing",
                    )
                })?)
            }
            Some(_) => {
                return Err(outbe_primitives::error::PrecompileError::storage(
                    "Tribute day index is outside the day",
                ))
            }
            None => None,
        };
        Ok((ids.base_slot(), id))
    })
    .map_err(internal_err)?;
    let proof = state
        .proof(
            TrieInput::default(),
            TRIBUTE_ADDRESS,
            &[B256::from(length_slot.to_be_bytes::<32>())],
        )
        .map_err(internal_err)?;
    let day_index_proof = AccountStorageProof::try_from(&proof).map_err(internal_err)?;
    let node_subject = TributeStateProofSubject::from(subject);
    TributeDayProofVerifier::new(
        node_subject,
        &day_index_proof,
        outbe_tee::tribute_source::proof_limits(),
    )
    .map_err(internal_err)?;
    // The identity is read from the fixed physical index, never from a caller list.
    let record = tribute_id
        .map(|id| {
            let source = serve(provider, chain_identity, subject, id)?;
            if Some(source.day_index) != day_index {
                return Err(invalid_params(
                    "Tribute reverse position does not match its physical index",
                ));
            }
            Ok(TributeRecordStateProof {
                day_index: source.day_index,
                tribute: source.tribute,
                account: source.proof,
            })
        })
        .transpose()?;
    Ok(TributeDayStateProof {
        day_index_proof,
        record,
    })
}
