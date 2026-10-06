use super::*;

pub(super) fn read_historical_committee_snapshot(
    state: &dyn StateProvider,
    record: &CertifiedParentProofRecord,
    limits: &SchemaLimits,
) -> Result<CommitteeSnapshot, FinalizedIntentProofBuildError> {
    let key = committee_snapshot_key(record.finalized_epoch, record.committee_set_hash);
    let mapped = |base_slot: u64| key.mapping_slot(U256::from(base_slot));
    let word = |slot: U256| -> Result<U256, FinalizedIntentProofBuildError> {
        state
            .storage(VALIDATOR_SET_ADDRESS, B256::new(slot.to_be_bytes::<32>()))
            .map_err(|error| FinalizedIntentProofBuildError::State(error.to_string()))
            .map(Option::unwrap_or_default)
    };

    if word(mapped(31))? != U256::from(1) {
        return Err(FinalizedIntentProofBuildError::Finalization(
            "historical committee snapshot is absent",
        ));
    }
    let committee_len = usize::try_from(word(mapped(32))?).map_err(|_| {
        FinalizedIntentProofBuildError::Finalization("historical committee length exceeds usize")
    })?;
    if committee_len == 0
        || committee_len > limits.max_collection_items
        || committee_len != record.ordered_committee.len()
    {
        return Err(FinalizedIntentProofBuildError::Finalization(
            "historical committee length is outside the exact persisted shape",
        ));
    }
    let mut committee = Vec::with_capacity(committee_len);
    for index in 0..committee_len {
        committee.push(read_committee_entry(&word, key, index as u64)?);
    }
    let (vrf_material_version, vrf_group_public_key_bytes) = read_vrf_key(&word, key, limits)?;
    let snapshot = CommitteeSnapshot {
        committee,
        vrf_material_version,
        vrf_group_public_key_bytes,
        vrf_public_polynomial_hash: B256::new(word(mapped(47))?.to_be_bytes::<32>()),
    };
    validate_persisted_snapshot(&snapshot, record)?;
    Ok(snapshot)
}

fn read_committee_entry(
    word: &impl Fn(U256) -> Result<U256, FinalizedIntentProofBuildError>,
    key: B256,
    index: u64,
) -> Result<CommitteeEntry, FinalizedIntentProofBuildError> {
    let nested =
        |base_slot: u64, index: u64| index.mapping_slot(key.mapping_slot(U256::from(base_slot)));
    let address_word = word(nested(33, index))?.to_be_bytes::<32>();
    if address_word[..12].iter().any(|byte| *byte != 0) {
        return Err(FinalizedIntentProofBuildError::Finalization(
            "historical committee address has non-zero high bytes",
        ));
    }
    let low = word(nested(34, index))?.to_be_bytes::<32>();
    let high = word(nested(35, index))?.to_be_bytes::<32>();
    if high[16..].iter().any(|byte| *byte != 0) {
        return Err(FinalizedIntentProofBuildError::Finalization(
            "historical committee BLS suffix is not right padded",
        ));
    }
    let mut consensus_pubkey = [0_u8; 48];
    consensus_pubkey[..32].copy_from_slice(&low);
    consensus_pubkey[32..].copy_from_slice(&high[..16]);
    Ok(CommitteeEntry {
        address: Address::from_slice(&address_word[12..]),
        consensus_pubkey,
    })
}

fn read_vrf_key(
    word: &impl Fn(U256) -> Result<U256, FinalizedIntentProofBuildError>,
    key: B256,
    limits: &SchemaLimits,
) -> Result<(u64, Vec<u8>), FinalizedIntentProofBuildError> {
    let mapped = |base_slot: u64| key.mapping_slot(U256::from(base_slot));
    let nested = |base_slot: u64, index: u64| index.mapping_slot(mapped(base_slot));
    let vrf_material_version = u64::try_from(word(mapped(36))?).map_err(|_| {
        FinalizedIntentProofBuildError::Finalization("VRF material version exceeds u64")
    })?;
    let vrf_key_len = usize::try_from(word(mapped(38))?).map_err(|_| {
        FinalizedIntentProofBuildError::Finalization("VRF group key length exceeds usize")
    })?;
    if vrf_key_len == 0 || vrf_key_len > limits.max_proof_bytes {
        return Err(FinalizedIntentProofBuildError::Finalization(
            "VRF group key length is outside proof bounds",
        ));
    }
    let mut vrf_group_public_key_bytes = Vec::with_capacity(vrf_key_len);
    for index in 0..vrf_key_len.div_ceil(32) {
        let chunk = word(nested(39, index as u64))?.to_be_bytes::<32>();
        vrf_group_public_key_bytes.extend_from_slice(&chunk);
    }
    vrf_group_public_key_bytes.truncate(vrf_key_len);
    if word(mapped(37))? != U256::from_be_bytes(keccak256(&vrf_group_public_key_bytes).0) {
        return Err(FinalizedIntentProofBuildError::Finalization(
            "VRF group key hash does not match snapshot bytes",
        ));
    }
    Ok((vrf_material_version, vrf_group_public_key_bytes))
}

fn validate_persisted_snapshot(
    snapshot: &CommitteeSnapshot,
    record: &CertifiedParentProofRecord,
) -> Result<(), FinalizedIntentProofBuildError> {
    let committee_mismatch = snapshot.committee_set_hash_v2(record.finalized_epoch)
        != record.committee_set_hash
        || snapshot
            .committee
            .iter()
            .map(|entry| entry.address)
            .ne(record.ordered_committee.iter().copied());
    if committee_mismatch
        || snapshot.vrf_material_version != record.vrf_material_version
        || keccak256(&snapshot.vrf_group_public_key_bytes) != record.vrf_group_public_key_hash
    {
        return Err(FinalizedIntentProofBuildError::Finalization(
            "historical committee snapshot does not match persisted finalization metadata",
        ));
    }
    Ok(())
}
