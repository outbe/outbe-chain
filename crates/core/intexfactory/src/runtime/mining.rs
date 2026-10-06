use super::*;

/// minePromis: PoW-gated burn of Settled then mint of Promis. `owner` is the
/// caller.
pub fn mine_promis(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    owner: Address,
    units: U256,
    nonce: u64,
    auth: outbe_promisfactory::api::ModifyAuth,
) -> Result<U256> {
    if owner.is_zero() {
        return Err(IntexFactoryError::ZeroAddress.into());
    }
    if units.is_zero() {
        return Err(IntexFactoryError::ZeroUnits.into());
    }

    let series = outbe_intex::api::read_series(storage, series_id)?;
    let settled = nft_balance_of(storage, owner, settled_token_id(series_id))?;
    if settled < units {
        return Err(IntexFactoryError::InsufficientSettled.into());
    }

    let promis_minor = series
        .promis_load_minor
        .checked_mul(units)
        .ok_or_else(|| PrecompileError::Revert("promis overflow".into()))?;

    // PoW over the per-(series, owner) sequence. Bump it on success.
    let mut factory = IntexFactoryContract::new(storage.clone());
    let seq = factory.read_mine_seq(series_id, owner)?;
    validate_pow(owner, promis_minor, series_id, seq, nonce)?;
    let next_seq = seq
        .checked_add(1)
        .ok_or_else(|| PrecompileError::Revert("mining sequence overflow".into()))?;
    // The sequence moves only together with the burn and the mint.
    storage.clone().with_checkpoint(|| {
        factory.write_mine_seq(series_id, owner, next_seq)?;

        // Burn Settled from owner on the NFT.
        storage.call(
            INTEX_NFT1155_ADDRESS,
            U256::ZERO,
            IIntexNFT1155::burnSettledCall {
                owner,
                seriesId: series_id.into(),
                units,
            }
            .abi_encode()
            .into(),
        )?;

        let exercised = u32::try_from(units)
            .map_err(|_| PrecompileError::Revert("exercised units exceed the series".into()))?;
        outbe_intex::api::record_exercised_units(storage, series_id, owner, exercised)?;

        // Promis is confidential: the mint runs inside the enclave, authorized by the
        // owner's Promis modify key (the `mac`/`opNonce` must bind `promis_minor`).
        outbe_promisfactory::api::mint(storage.clone(), owner, promis_minor, auth)?;

        emit_event(
            storage,
            crate::precompile::IIntexFactory::PromisMined {
                seriesId: series_id.into(),
                owner,
                units,
                promisMinor: promis_minor,
            },
        )?;
        Ok(promis_minor)
    })
}

/// Issued token id = `uint256(seriesId)`. Mirrors `IntexNFT1155._issuedTokenId`.
pub(crate) fn issued_token_id(series_id: SeriesId) -> U256 {
    U256::from_be_slice(series_id.as_bytes())
}

/// Settled token id = the series id with `SETTLED_TAG` set. A series id is 14 bytes, so the issued
/// space ends at 2**112 and the bit above it distinguishes the classes without a hash. Mirrors
/// `IntexNFT1155._settledTokenId`. The two derivations must stay identical.
pub(crate) fn settled_token_id(series_id: SeriesId) -> U256 {
    issued_token_id(series_id) | SETTLED_TAG
}

/// PoW hash: `SHA256(owner ++ promisAmount_be32 ++ seriesId ++ seq_be4 ++ nonce_be8)`.
///
/// `owner` and `seq` earn their place here, unlike in the shared scheme: the
/// owner arrives as a call argument, and the sequence rises with every
/// successful partial mining, so one solved nonce cannot serve the next.
pub(crate) fn compute_pow_hash(
    owner: Address,
    promis_amount: U256,
    series_id: SeriesId,
    seq: u32,
    nonce: u64,
) -> [u8; 32] {
    let mut data = Vec::with_capacity(20 + 32 + SERIES_ID_LEN + 4 + 8);
    data.extend_from_slice(owner.as_slice());
    data.extend_from_slice(&promis_amount.to_be_bytes::<32>());
    data.extend_from_slice(series_id.as_bytes());
    data.extend_from_slice(&seq.to_be_bytes());
    data.extend_from_slice(&nonce.to_be_bytes());

    let digest = ring::digest::digest(&ring::digest::SHA256, &data);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

/// The preimage is Intex's own. The difficulty it must clear is the protocol's.
pub(crate) fn validate_pow(
    owner: Address,
    promis_amount: U256,
    series_id: SeriesId,
    seq: u32,
    nonce: u64,
) -> Result<()> {
    let hash = compute_pow_hash(owner, promis_amount, series_id, seq, nonce);
    outbe_common::pow::meets_difficulty(&hash).map_err(|e| IntexFactoryError::from(e).into())
}
