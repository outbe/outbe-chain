use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, metadata, view};
use outbe_primitives::erc::{
    ERC165_INTERFACE_ID, ERC721_ENUMERABLE_INTERFACE_ID, ERC721_INTERFACE_ID,
    ERC721_METADATA_INTERFACE_ID,
};
use outbe_primitives::error::Result;

use crate::errors::NodError;
use crate::schema::{NodBucketState, NodContract, NodItemState};

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
        unlocksAt: item.unlocks_at,
        referenceCurrency: item.reference_currency,
        issuedAt: item.issued_at,
    }
}
