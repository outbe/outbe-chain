use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, metadata, view};
use outbe_primitives::erc::{
    ERC165_INTERFACE_ID, ERC4906_INTERFACE_ID, ERC721_INTERFACE_ID, ERC721_METADATA_INTERFACE_ID,
};
use outbe_primitives::error::Result;

use crate::api;
use crate::errors::GemError;
use crate::schema::{GemContract, GemView};

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

/// No global enumeration: gems are read by id and by owner only.
const SUPPORTED_INTERFACES: [[u8; 4]; 4] = [
    ERC165_INTERFACE_ID,
    ERC721_INTERFACE_ID,
    ERC721_METADATA_INTERFACE_ID,
    ERC4906_INTERFACE_ID,
];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IGem.sol"
);

/// Dispatches Gem calls against EVM state.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, IGem::IGemCalls::abi_decode, |call| {
        use IGem::IGemCalls::*;
        match call {
            call @ (supportsInterface(_) | name(_) | symbol(_) | totalSupply(_)) => {
                dispatch_metadata(&storage, call)
            }
            transferFrom(_)
            | safeTransferFrom_0(_)
            | safeTransferFrom_1(_)
            | approve(_)
            | setApprovalForAll(_) => Err(GemError::NonTransferable.into()),
            getApproved(c) => view(c, |_| Ok(Address::ZERO)),
            isApprovedForAll(c) => view(c, |_| Ok(false)),
            call @ (balanceOf(_)
            | ownerOf(_)
            | tokenURI(_)
            | tokenOfOwnerByIndex(_)
            | getGem(_)) => dispatch_entity(&storage, call),
        }
    })
}

fn dispatch_metadata(
    storage: &outbe_primitives::storage::StorageHandle<'_>,
    call: IGem::IGemCalls,
) -> Result<Bytes> {
    use IGem::IGemCalls::*;
    match call {
        supportsInterface(c) => view(c, |c| Ok(SUPPORTED_INTERFACES.contains(&c.interfaceId.0))),
        name(_) => metadata::<IGem::nameCall>(|| Ok(GemContract::name().to_string())),
        symbol(_) => metadata::<IGem::symbolCall>(|| Ok(GemContract::symbol().to_string())),
        totalSupply(_) => metadata::<IGem::totalSupplyCall>(|| {
            GemContract::new(storage.clone())
                .total_supply()
                .map(U256::from)
        }),
        _ => Err(crate::errors::GemFatalError::MetadataRouteEntityCall.into()),
    }
}

fn dispatch_entity(
    storage: &outbe_primitives::storage::StorageHandle<'_>,
    call: IGem::IGemCalls,
) -> Result<Bytes> {
    use IGem::IGemCalls::*;
    match call {
        balanceOf(c) => view(c, |c| api::balance_of(storage, c.owner).map(U256::from)),
        ownerOf(c) => view(c, |c| {
            Ok(api::get_item(storage, c.gemId)?
                .ok_or(GemError::GemNotFound)?
                .owner)
        }),
        tokenURI(c) => view(c, |c| {
            let gem = api::get_view(storage, c.gemId)?.ok_or(GemError::GemNotFound)?;
            let now = storage.timestamp()?.to::<u64>();
            Ok(crate::metadata::token_uri(&gem, now))
        }),
        tokenOfOwnerByIndex(c) => view(c, |c| {
            let idx = usize::try_from(c.index).map_err(|_| GemError::IndexOutOfBounds)?;
            api::list_by_owner(storage, c.owner)?
                .get(idx)
                .map(|item| item.gem_id.to_u256())
                .ok_or_else(|| GemError::IndexOutOfBounds.into())
        }),
        getGem(c) => view(c, |c| {
            let gem = api::get_view(storage, c.gemId)?.ok_or(GemError::GemNotFound)?;
            Ok(to_abi_data(&gem, storage.timestamp()?.to::<u64>()))
        }),
        _ => Err(crate::errors::GemFatalError::EntityRouteMetadataCall.into()),
    }
}

fn to_abi_data(gem: &GemView, now: u64) -> IGem::GemData {
    IGem::GemData {
        gemId: gem.item.gem_id.to_u256(),
        owner: gem.item.owner,
        gemType: gem.item.gem_type,
        state: gem.effective_state(now),
        promisLoadMinor: gem.item.promis_load_minor,
        entryPriceMinor: gem.bucket.entry_price_minor,
        issuanceCurrency: gem.item.issuance_currency,
        referenceCurrency: gem.item.reference_currency,
        issuedAt: gem.item.issued_at,
        callPriceMinor: gem.terms.call_price_minor,
        calledAt: gem.called_at(),
        callNoticePeriod: gem.terms.call_notice_period_seconds,
        callWindow: gem.terms.call_window_seconds,
        callThreshold: gem.terms.call_threshold_seconds,
        settlementDeadline: gem.settlement_deadline(),
    }
}
