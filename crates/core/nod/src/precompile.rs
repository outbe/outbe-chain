use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, metadata, mutate, view};
use outbe_primitives::erc::{
    ERC165_INTERFACE_ID, ERC4906_INTERFACE_ID, ERC721_INTERFACE_ID, ERC721_METADATA_INTERFACE_ID,
};
use outbe_primitives::error::Result;

use crate::api;
use crate::errors::NodError;
use crate::schema::{NodContract, NodItemState};

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

const SUPPORTED_INTERFACES: [[u8; 4]; 4] = [
    ERC165_INTERFACE_ID,
    ERC721_INTERFACE_ID,
    ERC721_METADATA_INTERFACE_ID,
    ERC4906_INTERFACE_ID,
];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/INod.sol"
);

/// Dispatches Nod calls against EVM state.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, INod::INodCalls::abi_decode, |call| {
        let nod = NodContract::new(storage.clone());
        use INod::INodCalls::*;
        match call {
            call @ (supportsInterface(_) | name(_) | symbol(_)) => dispatch_metadata(call),
            transferFrom(_)
            | safeTransferFrom_0(_)
            | safeTransferFrom_1(_)
            | approve(_)
            | setApprovalForAll(_) => Err(NodError::NonTransferable.into()),
            getApproved(c) => view(c, |_| Ok(Address::ZERO)),
            isApprovedForAll(c) => view(c, |_| Ok(false)),
            clean(c) => mutate(&storage, c, caller, |_, c| {
                api::clean(&storage, c.nodId)?;
                Ok(INod::cleanReturn {})
            }),
            call @ (balanceOf(_) | ownerOf(_) | tokenURI(_) | tokensByOwner(_) | getNod(_)) => {
                dispatch_entity(storage, &nod, call)
            }
        }
    })
}

fn dispatch_metadata(call: INod::INodCalls) -> Result<Bytes> {
    use INod::INodCalls::*;
    match call {
        supportsInterface(c) => view(c, |c| Ok(SUPPORTED_INTERFACES.contains(&c.interfaceId.0))),
        name(_) => metadata::<INod::nameCall>(|| Ok(NodContract::name().to_string())),
        symbol(_) => metadata::<INod::symbolCall>(|| Ok(NodContract::symbol().to_string())),
        _ => unreachable!("metadata route requires a metadata call"),
    }
}

fn dispatch_entity(
    storage: outbe_primitives::storage::StorageHandle<'_>,
    nod: &NodContract<'_>,
    call: INod::INodCalls,
) -> Result<Bytes> {
    use INod::INodCalls::*;
    match call {
        balanceOf(c) => view(c, |c| api::balance_of(&storage, c.owner).map(U256::from)),
        ownerOf(c) => view(c, |c| {
            let nod_id = c.nodId;
            Ok(api::get_item(&storage, nod_id)?
                .ok_or(NodError::NodNotFound)?
                .owner)
        }),
        tokenURI(c) => view(c, |c| {
            let nod_id = c.nodId;
            let item = api::get_item(&storage, nod_id)?.ok_or(NodError::NodNotFound)?;
            crate::metadata::token_uri(nod, &item, storage.timestamp()?)
        }),
        tokensByOwner(c) => view(c, |c| api::tokens_by_owner(&storage, c.owner)),
        getNod(c) => view(c, |c| {
            let nod_id = c.nodId;
            let item = api::get_item(&storage, nod_id)?.ok_or(NodError::NodNotFound)?;
            to_abi_data(&storage, &item)
        }),
        _ => unreachable!("entity route requires an entity call"),
    }
}

fn to_abi_data(
    storage: &outbe_primitives::storage::StorageHandle<'_>,
    item: &NodItemState,
) -> Result<INod::NodData> {
    let nod = NodContract::new(storage.clone());
    let called_at = nod.group_called_at.read(&item.group_key)?;
    let terms = nod.read_call_terms(item.group_key)?;
    let deadline = if called_at == 0 {
        0
    } else {
        api::settlement_deadline_of(called_at, terms.call_notice_period_seconds)
    };
    let state = api::state_of(storage, item)?;
    Ok(INod::NodData {
        nodId: item.nod_id,
        owner: item.owner,
        seriesId: item.series_id.into(),
        leagueId: item.league_id,
        encryptedGratisAmount: item.encrypted.encrypted_gratis_amount.clone().into(),
        encryptionBinding: item.encrypted.encryption_binding,
        chainId: item.encrypted.terms.chain_id,
        entryPriceMinor: item.entry_price_minor(),
        settlementCostMinor: api::settlement_cost_minor(
            item.entry_price_minor(),
            api::calculation_amount(item)?,
        )?,
        issuanceCurrency: item.issuance_currency,
        referenceCurrency: item.reference_currency,
        issuedAt: item.issued_at,
        calledAt: called_at,
        isSettled: item.is_settled,
        effectiveState: state as u8,
        callPriceMinor: terms.call_price_minor,
        callRate: terms.call_rate,
        callWindow: terms.call_window_seconds,
        callThreshold: terms.call_threshold_seconds,
        callNoticePeriod: terms.call_notice_period_seconds,
        settlementDeadline: deadline,
        groupKey: item.group_key,
    })
}
