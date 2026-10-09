//! Credis factory ABI: issue against a reservation's pledge and settle repayments.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolInterface};

use outbe_primitives::dispatch::{
    dispatch_call, mutate, mutate_payable, reject_value_unless_payable, view, PayableCallContext,
};
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[ICredisFactory::issueCredisCall::SELECTOR];

sol!("../../../contracts/precompiles/src/ICredisFactory.sol");

pub fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    // CredisFactory is a payable route, so the boundary credits value to this address
    // before dispatch. Refuse it for every selector except the published one.
    reject_value_unless_payable(data, PAYABLE_SELECTORS, &value)?;
    dispatch_call(
        data,
        ICredisFactory::ICredisFactoryCalls::abi_decode,
        |call| {
            use ICredisFactory::ICredisFactoryCalls::*;
            match call {
                issueCredis(c) => mutate_payable(
                    &storage,
                    c,
                    PayableCallContext {
                        selectors: PAYABLE_SELECTORS,
                        sender: caller,
                        value,
                    },
                    |sender, c, val| {
                        let (credis_id, principal_minor) = runtime::issue_credis(
                            storage.clone(),
                            sender,
                            c.reservationId,
                            c.referenceCurrency,
                            val,
                        )?;
                        Ok(ICredisFactory::issueCredisReturn {
                            credisId: credis_id,
                            principalMinor: principal_minor,
                        })
                    },
                ),
                settleCredis(c) => mutate(&storage, c, caller, |sender, c| {
                    let (principal, interest) =
                        runtime::settle(storage.clone(), sender, c.credisId, c.amountMinor)?;
                    Ok(ICredisFactory::settleCredisReturn {
                        principalPaidMinor: principal,
                        interestPaidMinor: interest,
                    })
                }),
                supportsInterface(c) => view(c, |c| {
                    let id: [u8; 4] = c.interfaceId.0;
                    Ok(id == ERC165_INTERFACE_ID)
                }),
            }
        },
    )
}
