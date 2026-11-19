use super::*;

/// What the owner brings to one mining: the PoW nonce and the Promis modify-key authorization.
pub struct MiningProof {
    pub nonce: u64,
    pub auth: outbe_promisfactory::api::ModifyAuth,
}

/// Burn Settled Intex and mint Promis with PoW and the owner's modify-key authorization.
/// `owner` comes from calldata. Any sender can submit the authorized request.
pub fn mine_promis(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    owner: Address,
    units: U256,
    proof: MiningProof,
) -> Result<U256> {
    let MiningProof { nonce, auth } = proof;
    if owner.is_zero() {
        return Err(IntexFactoryError::ZeroAddress.into());
    }
    if units.is_zero() {
        return Err(IntexFactoryError::ZeroUnits.into());
    }

    let series = outbe_intex::api::get_series(storage, series_id)?;
    let settled = outbe_intex::api::owner_settled_units(storage, series_id, owner)?;
    if U256::from(settled) < units {
        return Err(IntexFactoryError::InsufficientSettled.into());
    }

    let promis_minor = series
        .promis_load_minor
        .checked_mul(units)
        .ok_or(IntexFactoryError::PromisOverflow)?;

    // PoW over the per-(series, owner) sequence. Bump it on success.
    let mut factory = IntexFactoryContract::new(storage.clone());
    let seq = factory.read_mine_seq(series_id, owner)?;
    validate_pow(
        owner,
        outbe_intex::intex_id(series_id),
        u64::from(seq),
        promis_minor,
        nonce,
    )?;
    let next_seq = seq
        .checked_add(1)
        .ok_or(IntexFactoryError::MiningSequenceOverflow)?;
    // The sequence moves only together with the burn and the mint.
    storage.clone().with_checkpoint(|| {
        factory.write_mine_seq(series_id, owner, next_seq)?;

        let mined = u32::try_from(units).map_err(|_| IntexFactoryError::MinedUnitsExceedSeries)?;
        outbe_intex::api::record_mined_units(storage, series_id, owner, mined)?;
        outbe_intex::api::release_if_spent(storage, series_id)?;

        // Promis is confidential: the mint runs inside the enclave, authorized by the
        // owner's Promis modify key (the `mac`/`opNonce` must bind `promis_minor`).
        outbe_promisfactory::api::mint_from_intex(
            storage.clone(),
            outbe_intex::intex_id(series_id),
            owner,
            promis_minor,
            auth,
        )?;

        Ok(promis_minor)
    })
}

/// The `minePromis` work check: the common mining preimage with the Intex tag and the
/// mined Promis amount.
pub(crate) fn validate_pow(
    owner: Address,
    intex_id: U256,
    mining_sequence: u64,
    promis_amount: U256,
    nonce: u64,
) -> Result<()> {
    let hash = outbe_core_common::pow::compute_intex_mining_pow_hash(
        intex_id,
        owner,
        mining_sequence,
        nonce,
        promis_amount,
    );
    outbe_core_common::pow::meets_difficulty(&hash).map_err(|e| IntexFactoryError::from(e).into())
}
