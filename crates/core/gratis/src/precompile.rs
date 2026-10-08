use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, metadata, view};
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::error::{PrecompileError, Result};

use crate::schema::Gratis;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!("../../../contracts/precompiles/src/IGratis.sol");

const TRANSFER_NOT_ALLOWED: &str = "gratis token transfers are not allowed";

/// Dispatches an ABI-encoded call to the Gratis precompile.
///
/// This surface is **read-only + the non-transferable ERC-20 stubs**. Balances
/// are confidential: `balanceOf` returns the account's ciphertext blob
/// (`version || AEAD-ct`) for the caller to decrypt with its view key. All state
/// changes go through the enclave-backed [`crate::api`] (the factories call it
/// cross-crate), never through this ABI.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, IGratis::IGratisCalls::abi_decode, |call| {
        let gratis = Gratis::new(storage);
        use IGratis::IGratisCalls::*;
        match call {
            name(_) => metadata::<IGratis::nameCall>(|| Ok(crate::metadata::NAME.to_string())),
            symbol(_) => {
                metadata::<IGratis::symbolCall>(|| Ok(crate::metadata::SYMBOL.to_string()))
            }
            decimals(_) => metadata::<IGratis::decimalsCall>(|| Ok(crate::metadata::DECIMALS)),
            pledgedTotalSupply(_) => metadata::<IGratis::pledgedTotalSupplyCall>(|| {
                crate::state::pledged_total_supply(&gratis)
            }),

            // Confidential reads return ciphertext. Decrypt it client-side.
            balanceOf(c) => view(c, |c| {
                crate::state::account(&gratis, c.account)
                    .balance_ct()
                    .map(Bytes::from)
            }),
            pledgedOf(c) => view(c, |c| {
                crate::state::account(&gratis, c.account)
                    .pledged_ct()
                    .map(Bytes::from)
            }),
            opNonceOf(c) => view(c, |c| crate::state::account(&gratis, c.account).op_nonce()),

            // Non-transferable surface.
            allowance(c) => view(c, |_c| Ok(U256::ZERO)),
            approve(_) | transfer(_) | transferFrom(_) => {
                Err(PrecompileError::Revert(TRANSFER_NOT_ALLOWED.into()))
            }

            supportsInterface(c) => view(c, |c| {
                let id: [u8; 4] = c.interfaceId.0;
                Ok(id == ERC165_INTERFACE_ID)
            }),
        }
    })
}
