use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolInterface};
use base64::Engine;
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{EntityId36, ExecutionScope, ParentBodySource};
use outbe_primitives::dispatch::{dispatch_call, metadata, preflight_dynamic_bytes_len, view};
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::error::Result;

use crate::api;
use crate::errors::NodError;
use crate::schema::{NodBucketState, NodCertifiedGenerationProjection, NodContract, NodItemState};

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/INod.sol"
);

/// Dispatches Nod calls through the block-scoped compressed-body lifecycle.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    preflight_entity_id(data)?;
    dispatch_call(data, INod::INodCalls::abi_decode, |call| {
        let nod = NodContract::new(storage.clone());
        use INod::INodCalls::*;
        match call {
            supportsInterface(c) => view(c, |c| {
                let id: [u8; 4] = c.interfaceId.0;
                Ok(id == ERC165_INTERFACE_ID)
            }),
            name(_) => metadata::<INod::nameCall>(|| Ok(NodContract::name().to_string())),
            symbol(_) => metadata::<INod::symbolCall>(|| Ok(NodContract::symbol().to_string())),
            totalSupply(_) => {
                metadata::<INod::totalSupplyCall>(|| nod.total_supply().map(U256::from))
            }
            balanceOf(c) => view(c, |c| {
                let count = api::list_by_owner(&storage, scope, parent, c.owner)?.len();
                Ok(U256::from(count))
            }),
            ownerOf(c) => view(c, |c| {
                let nod_id = parse_entity_id(&c.nodId)?;
                Ok(api::get_item(&storage, scope, parent, nod_id)?
                    .ok_or(NodError::NodNotFound)?
                    .owner)
            }),
            tokenURI(c) => view(c, |c| {
                let nod_id = parse_entity_id(&c.nodId)?;
                let item =
                    api::get_item(&storage, scope, parent, nod_id)?.ok_or(NodError::NodNotFound)?;
                let bucket_id = EntityId36::new(item.worldwide_day, item.bucket_key.0);
                let bucket = api::get_bucket(&storage, scope, parent, bucket_id)?
                    .ok_or(NodError::BucketNotFound)?;
                token_uri(&item, &bucket)
            }),
            tokenByIndex(c) => view(c, |c| {
                let idx = usize::try_from(c.index).map_err(|_| NodError::IndexOutOfBounds)?;
                api::list_all(&storage, scope, parent)?
                    .get(idx)
                    .map(|item| Bytes::copy_from_slice(item.nod_id.as_bytes()))
                    .ok_or_else(|| NodError::IndexOutOfBounds.into())
            }),
            tokenOfOwnerByIndex(c) => view(c, |c| {
                let idx = usize::try_from(c.index).map_err(|_| NodError::IndexOutOfBounds)?;
                api::list_by_owner(&storage, scope, parent, c.owner)?
                    .get(idx)
                    .map(|item| Bytes::copy_from_slice(item.nod_id.as_bytes()))
                    .ok_or_else(|| NodError::IndexOutOfBounds.into())
            }),
            nodData(c) => view(c, |c| {
                let nod_id = parse_entity_id(&c.nodId)?;
                let item =
                    api::get_item(&storage, scope, parent, nod_id)?.ok_or(NodError::NodNotFound)?;
                let bucket_id = EntityId36::new(item.worldwide_day, item.bucket_key.0);
                let bucket = api::get_bucket(&storage, scope, parent, bucket_id)?
                    .ok_or(NodError::BucketNotFound)?;
                Ok(to_abi_data(&item, &bucket))
            }),
            certifiedGeneration(c) => view(c, |c| {
                let worldwide_day = WorldwideDay::new(c.worldwideDay);
                Ok(to_abi_certified_generation(
                    worldwide_day,
                    nod.ocomp_certified_generation(worldwide_day)?,
                ))
            }),
        }
    })
}

fn preflight_entity_id(data: &[u8]) -> Result<()> {
    for selector in [
        INod::ownerOfCall::SELECTOR,
        INod::tokenURICall::SELECTOR,
        INod::nodDataCall::SELECTOR,
    ] {
        preflight_dynamic_bytes_len(data, selector, 0, 1, EntityId36::LEN)?;
    }
    Ok(())
}

fn token_uri(item: &NodItemState, bucket: &NodBucketState) -> Result<String> {
    let nod_id_str = NodContract::format_nod_id(item.nod_id);
    let json = format!(
        "{{\"name\":\"Nod #{}\",\"description\":\"{}\",\"image\":\"{}{}\",\"attributes\":[{{\"trait_type\":\"token_id\",\"value\":\"{}\"}},{{\"trait_type\":\"worldwide_day\",\"value\":{}}},{{\"trait_type\":\"league_id\",\"value\":{}}},{{\"trait_type\":\"floor_price_minor\",\"value\":\"{}\"}},{{\"trait_type\":\"gratis_load_minor\",\"value\":\"{}\"}},{{\"trait_type\":\"cost_of_gratis_minor\",\"value\":\"{}\"}},{{\"trait_type\":\"cost_amount_minor\",\"value\":\"{}\"}},{{\"trait_type\":\"is_qualified\",\"value\":{}}},{{\"trait_type\":\"is_settled\",\"value\":{}}},{{\"trait_type\":\"issued_at\",\"value\":{}}},{{\"trait_type\":\"reference_currency\",\"value\":{}}},{{\"trait_type\":\"issuance_currency\",\"value\":{}}}]}}",
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
        if item.is_settled { "true" } else { "false" },
        item.issued_at,
        item.reference_currency,
        item.issuance_currency,
    );
    let encoded = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
    Ok(format!("data:application/json;base64,{encoded}"))
}

fn to_abi_data(item: &NodItemState, bucket: &NodBucketState) -> INod::NodData {
    INod::NodData {
        nodId: Bytes::copy_from_slice(item.nod_id.as_bytes()),
        owner: item.owner,
        worldwideDay: item.worldwide_day.into(),
        leagueId: item.league_id,
        floorPriceMinor: item.floor_price_minor,
        gratisLoadMinor: item.gratis_load_minor,
        costOfGratisMinor: bucket.entry_price_minor,
        costAmountMinor: item.cost_amount_minor,
        isQualified: bucket.is_qualified,
        isSettled: item.is_settled,
        issuanceCurrency: item.issuance_currency,
        referenceCurrency: item.reference_currency,
        issuedAt: item.issued_at,
    }
}

fn to_abi_certified_generation(
    worldwide_day: WorldwideDay,
    generation: Option<NodCertifiedGenerationProjection>,
) -> INod::CertifiedGenerationData {
    match generation {
        Some(generation) => INod::CertifiedGenerationData {
            exists: true,
            worldwideDay: generation.worldwide_day.into(),
            generation: generation.generation,
            nodRoot: generation.nod_root,
            bucketRoot: generation.bucket_root,
            outputManifestRoot: generation.output_manifest_root,
            tributeCount: generation.tribute_count,
            nodCount: generation.nod_count,
            bucketCount: generation.bucket_count,
            nodAmountTotal: generation.nod_amount_total,
            nodGratisConsumed: generation.nod_gratis_consumed,
            issuedAt: generation.issued_at,
        },
        None => INod::CertifiedGenerationData {
            exists: false,
            worldwideDay: worldwide_day.into(),
            generation: 0,
            nodRoot: Default::default(),
            bucketRoot: Default::default(),
            outputManifestRoot: Default::default(),
            tributeCount: 0,
            nodCount: 0,
            bucketCount: 0,
            nodAmountTotal: U256::ZERO,
            nodGratisConsumed: U256::ZERO,
            issuedAt: 0,
        },
    }
}

fn parse_entity_id(bytes: &Bytes) -> Result<EntityId36> {
    EntityId36::try_from(bytes.as_ref())
        .map_err(|error| outbe_primitives::error::PrecompileError::Revert(error.to_string()))
}
