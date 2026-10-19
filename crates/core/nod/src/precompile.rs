use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, metadata, view};
use outbe_primitives::erc::{
    ERC165_INTERFACE_ID, ERC4906_INTERFACE_ID, ERC721_ENUMERABLE_INTERFACE_ID, ERC721_INTERFACE_ID,
    ERC721_METADATA_INTERFACE_ID,
};
use outbe_primitives::error::Result;

use crate::api;
use crate::errors::NodError;
use crate::schema::{NodContract, NodGroupState, NodItemState};
use outbe_primitives::wwd_entity_id::WwdEntityId;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

const SUPPORTED_INTERFACES: [[u8; 4]; 5] = [
    ERC165_INTERFACE_ID,
    ERC721_INTERFACE_ID,
    ERC721_METADATA_INTERFACE_ID,
    ERC721_ENUMERABLE_INTERFACE_ID,
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
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, INod::INodCalls::abi_decode, |call| {
        let nod = NodContract::new(storage.clone());
        use INod::INodCalls::*;
        match call {
            call @ (supportsInterface(_) | name(_) | symbol(_) | totalSupply(_)) => {
                dispatch_metadata(&nod, call)
            }
            transferFrom(_)
            | safeTransferFrom_0(_)
            | safeTransferFrom_1(_)
            | approve(_)
            | setApprovalForAll(_) => Err(NodError::NonTransferable.into()),
            getApproved(c) => view(c, |_| Ok(Address::ZERO)),
            isApprovedForAll(c) => view(c, |_| Ok(false)),
            call @ (balanceOf(_)
            | ownerOf(_)
            | tokenURI(_)
            | tokenByIndex(_)
            | tokenOfOwnerByIndex(_)
            | getNod(_)) => dispatch_entity(storage, &nod, call),
        }
    })
}

fn dispatch_metadata(nod: &NodContract<'_>, call: INod::INodCalls) -> Result<Bytes> {
    use INod::INodCalls::*;
    match call {
        supportsInterface(c) => view(c, |c| Ok(SUPPORTED_INTERFACES.contains(&c.interfaceId.0))),
        name(_) => metadata::<INod::nameCall>(|| Ok(NodContract::name().to_string())),
        symbol(_) => metadata::<INod::symbolCall>(|| Ok(NodContract::symbol().to_string())),
        totalSupply(_) => metadata::<INod::totalSupplyCall>(|| nod.total_supply().map(U256::from)),
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
            let nod_id = WwdEntityId::from(c.nodId);
            Ok(api::get_item(&storage, nod_id)?
                .ok_or(NodError::NodNotFound)?
                .owner)
        }),
        tokenURI(c) => view(c, |c| {
            let nod_id = WwdEntityId::from(c.nodId);
            let item = api::get_item(&storage, nod_id)?.ok_or(NodError::NodNotFound)?;
            let group = api::get_group(&storage, item.group_key)?.ok_or(NodError::GroupNotFound)?;
            crate::metadata::token_uri(nod, &item, &group, storage.timestamp()?)
        }),
        tokenByIndex(c) => view(c, |c| {
            let idx = usize::try_from(c.index).map_err(|_| NodError::IndexOutOfBounds)?;
            api::list_all(&storage)?
                .get(idx)
                .map(|item| item.nod_id.to_u256())
                .ok_or_else(|| NodError::IndexOutOfBounds.into())
        }),
        tokenOfOwnerByIndex(c) => view(c, |c| {
            let idx = usize::try_from(c.index).map_err(|_| NodError::IndexOutOfBounds)?;
            api::list_by_owner(&storage, c.owner)?
                .get(idx)
                .map(|item| item.nod_id.to_u256())
                .ok_or_else(|| NodError::IndexOutOfBounds.into())
        }),
        getNod(c) => view(c, |c| {
            let nod_id = WwdEntityId::from(c.nodId);
            let item = api::get_item(&storage, nod_id)?.ok_or(NodError::NodNotFound)?;
            let group = api::get_group(&storage, item.group_key)?.ok_or(NodError::GroupNotFound)?;
            to_abi_data(&storage, &item, &group)
        }),
        _ => unreachable!("entity route requires an entity call"),
    }
}

fn to_abi_data(
    storage: &outbe_primitives::storage::StorageHandle<'_>,
    item: &NodItemState,
    group: &NodGroupState,
) -> Result<INod::NodData> {
    let nod = NodContract::new(storage.clone());
    let called_at = nod.group_called_at.read(&item.group_key)?;
    let terms = nod.read_call_terms(item.group_key)?;
    let deadline = if called_at == 0 {
        0
    } else {
        api::settlement_deadline_of(called_at, terms.call_notice_period_seconds)
    };
    let state = api::effective_state(item, called_at, deadline, storage.timestamp()?);
    Ok(INod::NodData {
        nodId: item.nod_id.to_u256(),
        owner: item.owner,
        worldwideDay: item.worldwide_day.into(),
        leagueId: item.league_id,
        encryptedGratisAmount: item.encrypted.encrypted_gratis_amount.clone().into(),
        encryptedCreatorPublicKey: item.encrypted.encrypted_creator_public_key.clone().into(),
        encryptionBinding: item.encrypted.encryption_binding,
        chainId: item.encrypted.terms.chain_id,
        entryPriceMinor: group.entry_price_minor,
        settlementCostMinor: api::settlement_cost_minor(
            group.entry_price_minor,
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
    })
}
