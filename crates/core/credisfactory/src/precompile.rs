//! ABI dispatch for the credisfactory precompile at `CREDIS_FACTORY_ADDRESS`.
//!
//! `requestCredis` consumes a confidential Gratis pledge (pledge handle + spend
//! authorization) and opens a credis position bound to `bundleAccount`.
//! `anadosis` advances the schedule and releases 1/N of the pledged collateral
//! back to the original pledger's encrypted Gratis balance.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};

use outbe_primitives::dispatch::{dispatch_call, mutate, mutate_void, view};
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;

sol!("../../../contracts/precompiles/src/ICredisFactory.sol");

pub fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(
        data,
        ICredisFactory::ICredisFactoryCalls::abi_decode,
        |call| {
            use ICredisFactory::ICredisFactoryCalls::*;
            match call {
                requestCredis(c) => mutate(c, caller, |sender, c| {
                    let (position_id, amount_stables) = runtime::request_credis(
                        storage.clone(),
                        sender,
                        c.asset,
                        c.bundleAccount,
                        c.eoaAccount,
                        c.pledgeHandle,
                        c.spendAuth.0,
                    )?;
                    Ok(ICredisFactory::requestCredisReturn {
                        positionId: position_id,
                        amountStables: amount_stables,
                    })
                }),
                anadosis(c) => mutate_void(c, caller, |sender, c| {
                    runtime::pay_anadosis(storage.clone(), sender, c.positionId)?;
                    Ok(())
                }),
                supportsInterface(c) => view(c, |c| {
                    let id: [u8; 4] = c.interfaceId.0;
                    Ok(id == ERC165_INTERFACE_ID)
                }),
            }
        },
    )
}
