use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, metadata, mutate, view};
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::error::Result;

use crate::schema::Promis;

/// `IPromis` interface ID (XOR of non-ERC-165 selectors in IPromis).
pub(crate) const IPROMIS_INTERFACE_ID: [u8; 4] = [0xa8, 0xb3, 0x81, 0x25];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IPromis.sol"
);

/// Dispatches an ABI-encoded call to the Promis precompile.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, IPromis::IPromisCalls::abi_decode, |call| {
        let mut promis = Promis::new(storage);
        use IPromis::IPromisCalls::*;
        match call {
            name(_) => metadata::<IPromis::nameCall>(|| Ok(promis.name().to_string())),
            symbol(_) => metadata::<IPromis::symbolCall>(|| Ok(promis.symbol().to_string())),
            decimals(_) => metadata::<IPromis::decimalsCall>(|| Ok(promis.decimals())),
            totalSupply(_) => metadata::<IPromis::totalSupplyCall>(|| promis.total_supply()),
            balanceOf(c) => view(c, |c| promis.balance_of(c.account)),
            mineCoen(c) => mutate(c, caller, |sender, c| {
                // A-32: mine_coen burns synthetic promis and returns amount.
                // We must mint native tokens to the caller.
                let amount = promis.mine_coen(sender, c.amount)?;
                if !amount.is_zero() {
                    promis.storage.increase_balance(sender, amount)?;
                }
                Ok(amount)
            }),
            supportsInterface(c) => view(c, |c| {
                let id: [u8; 4] = c.interfaceId.0;
                Ok(id == ERC165_INTERFACE_ID || id == IPROMIS_INTERFACE_ID)
            }),
        }
    })
}
