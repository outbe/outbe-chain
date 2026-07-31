use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolInterface};
use outbe_compressed_entities::{EntityId36, ExecutionScope, ParentBodySource};
use outbe_primitives::dispatch::{dispatch_call, metadata, preflight_dynamic_bytes_len, view};
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::error::Result;

use crate::schema::TributeContract;

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/ITribute.sol"
);

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
    dispatch_call(data, ITribute::ITributeCalls::abi_decode, |call| {
        let tribute = TributeContract::new(storage);
        use ITribute::ITributeCalls::*;
        match call {
            name(_) => metadata::<ITribute::nameCall>(|| Ok("tribute".to_string())),
            symbol(_) => metadata::<ITribute::symbolCall>(|| Ok("TRIBUTE".to_string())),
            totalSupply(_) => metadata::<ITribute::totalSupplyCall>(|| {
                Ok(alloy_primitives::U256::from(tribute.total_supply()?))
            }),
            balanceOf(c) => view(c, |c| {
                Ok(alloy_primitives::U256::from(
                    tribute.balance_of(scope, parent, c.owner)?,
                ))
            }),
            ownerOf(c) => view(c, |c| {
                tribute.owner_of(scope, parent, parse_entity_id(&c.tributeId)?)
            }),
            tokenURI(c) => view(c, |c| {
                tribute.token_uri(scope, parent, parse_entity_id(&c.tributeId)?)
            }),
            getDayTotals(c) => view(c, |c| {
                let dt = tribute.get_day_totals(c.worldwideDay.into())?;
                Ok((dt.tribute_count, dt.tribute_nominal_amount, dt.is_sealed).into())
            }),
            getTributesByOwner(c) => view(c, |c| {
                Ok(tribute
                    .get_tribute_ids_by_owner(scope, parent, c.owner)?
                    .into_iter()
                    .map(|id| Bytes::copy_from_slice(id.as_bytes()))
                    .collect::<Vec<_>>())
            }),
            getTributesByDay(c) => view(c, |c| {
                Ok(tribute
                    .get_tribute_ids_by_day(scope, parent, c.worldwideDay.into())?
                    .into_iter()
                    .map(|id| Bytes::copy_from_slice(id.as_bytes()))
                    .collect::<Vec<_>>())
            }),
            supportsInterface(c) => view(c, |c| {
                let id: [u8; 4] = c.interfaceId.0;
                Ok(id == ERC165_INTERFACE_ID)
            }),
        }
    })
}

fn preflight_entity_id(data: &[u8]) -> Result<()> {
    for selector in [
        ITribute::ownerOfCall::SELECTOR,
        ITribute::tokenURICall::SELECTOR,
    ] {
        preflight_dynamic_bytes_len(data, selector, 0, 1, EntityId36::LEN)?;
    }
    Ok(())
}

fn parse_entity_id(bytes: &Bytes) -> Result<EntityId36> {
    EntityId36::try_from(bytes.as_ref())
        .map_err(|error| outbe_primitives::error::PrecompileError::Revert(error.to_string()))
}
