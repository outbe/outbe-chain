use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolInterface;
use outbe_core_common::erc::ERC165_INTERFACE_ID;
use outbe_core_common::wwd_entity_id::WwdEntityId;
use outbe_primitives::dispatch::{dispatch_call, metadata, mutate, view};
use outbe_primitives::error::Result;

use crate::schema::TributeContract;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

// Alloy 1.6 generates event constructors with the Solidity argument lists.
#[allow(clippy::too_many_arguments)]
mod abi {
    alloy_sol_types::sol!(
        #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
        "../../../contracts/precompiles/src/ITribute.sol"
    );
}
pub use abi::ITribute;

pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, ITribute::ITributeCalls::abi_decode, |call| {
        let mut tribute = TributeContract::new(storage.clone());
        use ITribute::ITributeCalls::*;
        match call {
            name(_) => metadata::<ITribute::nameCall>(|| Ok("tribute".to_string())),
            symbol(_) => metadata::<ITribute::symbolCall>(|| Ok("TRIBUTE".to_string())),
            balanceOf(c) => view(c, |c| {
                Ok(alloy_primitives::U256::from(tribute.balance_of(c.owner)?))
            }),
            ownerOf(c) => view(c, |c| tribute.owner_of(WwdEntityId::from(c.tributeId))),
            tokenURI(c) => view(c, |c| tribute.token_uri(WwdEntityId::from(c.tributeId))),
            getDayTotals(c) => view(c, |c| {
                let day =
                    outbe_emission::api::get_emission_day_record(storage.clone(), c.emissionDay)?
                        .map(|record| record.wwd)
                        .unwrap_or_else(|| outbe_system_common::time::WorldwideDay::new(0));
                let dt = tribute
                    .day_totals
                    .get(day)?
                    .unwrap_or_else(|| crate::day_schema::StoredDayTotals::with_key(day));
                let encrypted = if tribute.lysis_completed_at(day)? != 0 {
                    let frozen = tribute.encrypted_day_nominal(day, true)?;
                    if frozen.is_empty() {
                        tribute.encrypted_day_nominal(day, false)?
                    } else {
                        frozen
                    }
                } else {
                    tribute.encrypted_day_nominal(day, false)?
                };
                Ok((
                    dt.tribute_count,
                    alloy_primitives::Bytes::from(encrypted),
                    dt.is_sealed,
                )
                    .into())
            }),
            tokensByOwner(c) => view(c, |c| {
                Ok(tribute
                    .get_tribute_ids_by_owner(c.owner)?
                    .into_iter()
                    .map(|id| id.to_u256())
                    .collect::<Vec<_>>())
            }),
            getLeague(c) => view(c, |c| tribute.get_league(WwdEntityId::from(c.tributeId))),
            issueNod(c) => mutate(&storage, c, caller, |sender, c| {
                let nod_id = tribute.issue_nod(sender, WwdEntityId::from(c.tributeId))?;
                Ok(nod_id)
            }),
            clean(c) => mutate(&storage, c, caller, |_, c| {
                tribute.clean(WwdEntityId::from(c.tributeId))?;
                Ok(ITribute::cleanReturn {})
            }),
            supportsInterface(c) => view(c, |c| {
                let id: [u8; 4] = c.interfaceId.0;
                Ok(id == ERC165_INTERFACE_ID)
            }),
        }
    })
}
