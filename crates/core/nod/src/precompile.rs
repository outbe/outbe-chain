use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use base64::Engine;
use outbe_primitives::dispatch::{dispatch_call, metadata, view};
use outbe_primitives::erc::{
    ERC165_INTERFACE_ID, ERC721_ENUMERABLE_INTERFACE_ID, ERC721_INTERFACE_ID,
    ERC721_METADATA_INTERFACE_ID,
};
use outbe_primitives::error::Result;

use crate::errors::NodError;
use crate::schema::{NodBucketState, NodContract, NodItemState};
use crate::{api, NodRepositoryReader};

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/INod.sol"
);

/// Dispatches an ABI-encoded call to the Nod precompile.
///
/// Nod owns ERC-721 reads, `nodData`, and `tokens`. Issuance (cross-module
/// from Lysis) and `mineGratis` (user-triggered) live on the NodFactory
/// precompile at `NOD_FACTORY_ADDRESS`.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    #[cfg(not(any(test, feature = "test-utils")))]
    {
        let _ = (storage, data, value);
        Err(outbe_primitives::error::PrecompileError::Fatal(
            "Nod execution read authority was not supplied".into(),
        ))
    }

    #[cfg(any(test, feature = "test-utils"))]
    legacy_dispatch(storage, data, value)
}

/// Dispatches Nod calls with the least-authority off-chain body reader.
pub fn dispatch_with_reader(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    _caller: Address,
    value: U256,
    reader: &NodRepositoryReader,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, INod::INodCalls::abi_decode, |call| {
        let nod = NodContract::new(storage.clone());
        use INod::INodCalls::*;
        match call {
            supportsInterface(c) => view(c, |c| {
                let id: [u8; 4] = c.interfaceId.0;
                Ok(id == ERC165_INTERFACE_ID
                    || id == ERC721_INTERFACE_ID
                    || id == ERC721_METADATA_INTERFACE_ID
                    || id == ERC721_ENUMERABLE_INTERFACE_ID)
            }),
            name(_) => metadata::<INod::nameCall>(|| Ok(NodContract::name().to_string())),
            symbol(_) => metadata::<INod::symbolCall>(|| Ok(NodContract::symbol().to_string())),
            totalSupply(_) => {
                metadata::<INod::totalSupplyCall>(|| nod.total_supply().map(U256::from))
            }
            balanceOf(c) => view(c, |c| {
                let count = api::list_by_owner(reader, c.owner)?.len();
                Ok(U256::from(count))
            }),
            ownerOf(c) => view(c, |c| {
                Ok(api::get_item(reader, c.nodId)?
                    .ok_or(NodError::NodNotFound)?
                    .owner)
            }),
            tokenURI(c) => view(c, |c| {
                let item = api::get_item(reader, c.nodId)?.ok_or(NodError::NodNotFound)?;
                let bucket =
                    api::get_bucket(reader, item.bucket_key)?.ok_or(NodError::BucketNotFound)?;
                token_uri(&item, &bucket)
            }),
            tokens(c) => view(c, |c| {
                Ok(api::list_by_owner(reader, c.owner)?
                    .into_iter()
                    .map(|item| item.nod_id)
                    .collect())
            }),
            tokenByIndex(c) => view(c, |c| {
                let idx = usize::try_from(c.index).map_err(|_| NodError::IndexOutOfBounds)?;
                api::list_all(reader)?
                    .get(idx)
                    .map(|item| item.nod_id)
                    .ok_or_else(|| NodError::IndexOutOfBounds.into())
            }),
            tokenOfOwnerByIndex(c) => view(c, |c| {
                let idx = usize::try_from(c.index).map_err(|_| NodError::IndexOutOfBounds)?;
                api::list_by_owner(reader, c.owner)?
                    .get(idx)
                    .map(|item| item.nod_id)
                    .ok_or_else(|| NodError::IndexOutOfBounds.into())
            }),
            nodData(c) => view(c, |c| {
                let item = api::get_item(reader, c.nodId)?.ok_or(NodError::NodNotFound)?;
                let bucket =
                    api::get_bucket(reader, item.bucket_key)?.ok_or(NodError::BucketNotFound)?;
                Ok(to_abi_data(&item, &bucket))
            }),
        }
    })
}

#[cfg(any(test, feature = "test-utils"))]
fn legacy_dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, INod::INodCalls::abi_decode, |call| {
        let nod = NodContract::new(storage.clone());
        use INod::INodCalls::*;
        match call {
            supportsInterface(c) => view(c, |c| {
                let id: [u8; 4] = c.interfaceId.0;
                Ok(id == ERC165_INTERFACE_ID
                    || id == ERC721_INTERFACE_ID
                    || id == ERC721_METADATA_INTERFACE_ID
                    || id == ERC721_ENUMERABLE_INTERFACE_ID)
            }),
            name(_) => metadata::<INod::nameCall>(|| Ok(NodContract::name().to_string())),
            symbol(_) => metadata::<INod::symbolCall>(|| Ok(NodContract::symbol().to_string())),
            totalSupply(_) => {
                metadata::<INod::totalSupplyCall>(|| nod.total_supply().map(U256::from))
            }
            balanceOf(c) => view(c, |c| nod.get_nods_count_by_owner(c.owner).map(U256::from)),
            ownerOf(c) => view(c, |c| nod.owner_of(c.nodId)),
            tokenURI(c) => view(c, |c| nod.token_uri(c.nodId)),
            tokens(c) => view(c, |c| nod.get_nods_by_owner(c.owner)),
            tokenByIndex(c) => view(c, |c| {
                let idx = u32::try_from(c.index).map_err(|_| NodError::IndexOutOfBounds)?;
                nod.global_nod_ids
                    .get(idx)?
                    .ok_or_else(|| NodError::IndexOutOfBounds.into())
            }),
            tokenOfOwnerByIndex(c) => view(c, |c| {
                let idx = u32::try_from(c.index).map_err(|_| NodError::IndexOutOfBounds)?;
                nod.get_nod_by_owner_idx(c.owner, idx)
            }),
            nodData(c) => view(c, |c| {
                let (item, bucket) = nod.get_nod_data(c.nodId)?;
                Ok(to_abi_data(&item, &bucket))
            }),
        }
    })
}

fn token_uri(item: &NodItemState, bucket: &NodBucketState) -> Result<String> {
    let nod_id_str = NodContract::format_nod_id(item.nod_id);
    let json = format!(
        "{{\"name\":\"Nod #{}\",\"description\":\"{}\",\"image\":\"{}{}\",\"attributes\":[{{\"trait_type\":\"token_id\",\"value\":\"{}\"}},{{\"trait_type\":\"worldwide_day\",\"value\":{}}},{{\"trait_type\":\"league_id\",\"value\":{}}},{{\"trait_type\":\"floor_price_minor\",\"value\":\"{}\"}},{{\"trait_type\":\"gratis_load_minor\",\"value\":\"{}\"}},{{\"trait_type\":\"cost_of_gratis_minor\",\"value\":\"{}\"}},{{\"trait_type\":\"cost_amount_minor\",\"value\":\"{}\"}},{{\"trait_type\":\"is_qualified\",\"value\":{}}},{{\"trait_type\":\"issued_at\",\"value\":{}}},{{\"trait_type\":\"reference_currency\",\"value\":{}}},{{\"trait_type\":\"issuance_currency\",\"value\":{}}}]}}",
        &nod_id_str[..8],
        crate::constants::TOKEN_DESCRIPTION,
        crate::constants::TOKEN_IMAGE_BASE,
        nod_id_str,
        item.nod_id,
        item.worldwide_day,
        item.league_id,
        item.floor_price_minor,
        item.gratis_load_minor,
        bucket.entry_price_minor,
        item.cost_amount_minor,
        if bucket.is_qualified { "true" } else { "false" },
        item.issued_at,
        item.reference_currency,
        item.issuance_currency,
    );
    let encoded = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
    Ok(format!("data:application/json;base64,{encoded}"))
}

fn to_abi_data(item: &NodItemState, bucket: &NodBucketState) -> INod::NodData {
    INod::NodData {
        nodId: item.nod_id,
        owner: item.owner,
        worldwideDay: item.worldwide_day.into(),
        leagueId: item.league_id,
        floorPriceMinor: item.floor_price_minor,
        gratisLoadMinor: item.gratis_load_minor,
        costOfGratisMinor: bucket.entry_price_minor,
        costAmountMinor: item.cost_amount_minor,
        isQualified: bucket.is_qualified,
        issuanceCurrency: item.issuance_currency,
        referenceCurrency: item.reference_currency,
        issuedAt: item.issued_at,
    }
}
