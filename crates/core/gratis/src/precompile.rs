use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, metadata, mutate, view};
use outbe_primitives::erc::{ERC165_INTERFACE_ID, ERC20_INTERFACE_ID};
use outbe_primitives::error::{PrecompileError, Result};

use crate::schema::Gratis;

sol!("../../../contracts/precompiles/src/IGratis.sol");

const TRANSFER_NOT_ALLOWED: &str = "gratis token transfers are not allowed";

/// Dispatches an ABI-encoded call to the Gratis precompile.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, IGratis::IGratisCalls::abi_decode, |call| {
        let mut gratis = Gratis::new(storage);
        use IGratis::IGratisCalls::*;
        match call {
            name(_) => metadata::<IGratis::nameCall>(|| Ok(gratis.name().to_string())),
            symbol(_) => metadata::<IGratis::symbolCall>(|| Ok(gratis.symbol().to_string())),
            decimals(_) => metadata::<IGratis::decimalsCall>(|| Ok(gratis.decimals())),
            totalSupply(_) => metadata::<IGratis::totalSupplyCall>(|| gratis.total_supply()),
            pledgedTotalSupply(_) => {
                metadata::<IGratis::pledgedTotalSupplyCall>(|| gratis.pledged_total_supply())
            }
            balanceOf(c) => view(c, |c| gratis.balance_of(c.account)),

            pledgedOf(c) => view(c, |c| gratis.pledged_of(c.account)),

            // Non-transferable surface.
            allowance(c) => view(c, |_c| Ok(U256::ZERO)),
            approve(_) | transfer(_) | transferFrom(_) => {
                Err(PrecompileError::Revert(TRANSFER_NOT_ALLOWED.into()))
            }

            mineCoen(c) => mutate(c, caller, |sender, c| {
                // A-32: mine_coen burns synthetic gratis and returns amount.
                // We must mint native tokens to the caller.
                let amount = gratis.mine_coen(sender, c.amount)?;
                if !amount.is_zero() {
                    gratis.storage.increase_balance(sender, amount)?;
                }
                Ok(amount)
            }),
            supportsInterface(c) => view(c, |c| {
                let id: [u8; 4] = c.interfaceId.0;
                Ok(id == ERC165_INTERFACE_ID || id == ERC20_INTERFACE_ID)
            }),
        }
    })
}
