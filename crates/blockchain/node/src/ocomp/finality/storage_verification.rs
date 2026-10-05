use super::*;

struct StorageOpening {
    storage: StorageWitness,
    expected_slots: Vec<(U256, U256)>,
}

pub(super) fn verify(
    proof: &FinalizedIntentProofV1,
    intent: &JobIntentV1,
    binding: IntentStorageBinding,
    limits: &SchemaLimits,
) -> Result<(), FinalizedIntentVerifierError> {
    let account_root = verify_account(proof, limits, binding.state_root)?;
    let opening = decode_storage_opening(proof, intent, binding.storage_key, limits)?;
    // Recompute the binding instead of trusting the caller's already-derived
    // value at the fixed Metadosis path.
    let recomputed = outbe_ocomp_protocol::intent::intent_storage_key(binding.intent_id)
        .map_err(|error| FinalizedIntentVerifierError::JobRecordEncoding(error.to_string()))?;
    if recomputed != binding.storage_key {
        return Err(FinalizedIntentVerifierError::WitnessMalformed {
            kind: "storage",
            reason: "intent storage key binding",
        });
    }

    verify_storage_nodes(opening, account_root)
}

fn verify_account(
    proof: &FinalizedIntentProofV1,
    limits: &SchemaLimits,
    request_state_root: B256,
) -> Result<B256, FinalizedIntentVerifierError> {
    ensure_witness_cap(
        "account",
        proof.intent_account_proof.0.len(),
        limits.max_proof_bytes,
    )?;
    ensure_witness_cap(
        "storage",
        proof.intent_storage_proof.0.len(),
        limits.max_proof_bytes,
    )?;

    let account = AccountWitness::decode(&proof.intent_account_proof.0, limits)?;
    verify_account_witness(account, METADOSIS_ADDRESS, request_state_root)
}

fn decode_storage_opening(
    proof: &FinalizedIntentProofV1,
    intent: &JobIntentV1,
    storage_key: B256,
    limits: &SchemaLimits,
) -> Result<StorageOpening, FinalizedIntentVerifierError> {
    let record = OcompJobRecordV1 {
        intent: intent.clone(),
        intent_height: intent.logical_evaluation_height,
        status: OcompJobStatus::AwaitingFinality,
        finalized: None,
        terminal: None,
    };
    let encoded_record = record
        .encode_canonical(limits)
        .map_err(|error| FinalizedIntentVerifierError::JobRecordEncoding(error.to_string()))?;
    let expected_slots = storage_bytes_slots(storage_key, &encoded_record);
    let storage =
        StorageWitness::decode(&proof.intent_storage_proof.0, limits, expected_slots.len())?;
    if storage.proofs.len() != expected_slots.len() {
        return Err(FinalizedIntentVerifierError::StorageProofCount {
            expected: expected_slots.len(),
            actual: storage.proofs.len(),
        });
    }

    Ok(StorageOpening {
        storage,
        expected_slots,
    })
}

fn verify_storage_nodes(
    opening: StorageOpening,
    account_root: B256,
) -> Result<(), FinalizedIntentVerifierError> {
    let StorageOpening {
        storage,
        expected_slots,
    } = opening;
    for (index, ((slot, word), nodes)) in expected_slots
        .into_iter()
        .zip(storage.proofs.iter())
        .enumerate()
    {
        let storage_proof = StorageProof {
            key: B256::new(slot.to_be_bytes::<32>()),
            value: word,
            ..StorageProof::new(B256::new(slot.to_be_bytes::<32>()))
        }
        .with_proof(nodes.clone());
        storage_proof.verify(account_root).map_err(|error| {
            FinalizedIntentVerifierError::StorageProof {
                index,
                reason: error.to_string(),
            }
        })?;
    }
    Ok(())
}
