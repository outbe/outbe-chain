use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolInterface};
use outbe_common::erc::ERC165_INTERFACE_ID;
use outbe_primitives::dispatch::{dispatch_call, metadata, view};
use outbe_primitives::error::Result;

use crate::errors::PromisError;
use crate::schema::Promis;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IPromis.sol"
);

const fn compute_ipromis_interface_id() -> [u8; 4] {
    let selectors: [[u8; 4]; 9] = [
        IPromis::nameCall::SELECTOR,
        IPromis::symbolCall::SELECTOR,
        IPromis::decimalsCall::SELECTOR,
        IPromis::balanceOfCall::SELECTOR,
        IPromis::opNonceOfCall::SELECTOR,
        IPromis::allowanceCall::SELECTOR,
        IPromis::approveCall::SELECTOR,
        IPromis::transferCall::SELECTOR,
        IPromis::transferFromCall::SELECTOR,
    ];
    let mut acc = [0u8; 4];
    let mut i = 0;
    while i < selectors.len() {
        acc[0] ^= selectors[i][0];
        acc[1] ^= selectors[i][1];
        acc[2] ^= selectors[i][2];
        acc[3] ^= selectors[i][3];
        i += 1;
    }
    acc
}

/// `IPromis` interface ID (XOR of non-ERC-165 selectors in IPromis).
pub(crate) const IPROMIS_INTERFACE_ID: [u8; 4] = compute_ipromis_interface_id();

/// Dispatches an ABI-encoded call to the Promis precompile.
///
/// This surface is **read-only + the non-transferable ERC-20 stubs**. Balances
/// are confidential: `balanceOf` returns the account's ciphertext blob
/// (`version || AEAD-ct`) for the caller to decrypt with its view key. All state
/// changes go through the enclave-backed [`crate::api`] (called cross-crate by
/// the factories), never this ABI.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, IPromis::IPromisCalls::abi_decode, |call| {
        let promis = Promis::new(storage);
        use IPromis::IPromisCalls::*;
        match call {
            name(_) => metadata::<IPromis::nameCall>(|| Ok(promis.name().to_string())),
            symbol(_) => metadata::<IPromis::symbolCall>(|| Ok(promis.symbol().to_string())),
            decimals(_) => metadata::<IPromis::decimalsCall>(|| Ok(promis.decimals())),

            // Confidential read: return ciphertext. The client decrypts it.
            balanceOf(c) => view(c, |c| promis.balance_ct_of(c.account).map(Bytes::from)),
            opNonceOf(c) => view(c, |c| promis.op_nonce_of(c.account)),

            // Non-transferable surface.
            allowance(c) => view(c, |_c| Ok(U256::ZERO)),
            approve(_) | transfer(_) | transferFrom(_) => {
                Err(PromisError::TransferNotAllowed.into())
            }

            supportsInterface(c) => view(c, |c| {
                let id: [u8; 4] = c.interfaceId.0;
                Ok(id == ERC165_INTERFACE_ID || id == IPROMIS_INTERFACE_ID)
            }),
        }
    })
}
