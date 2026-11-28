//! Credis factory ABI: issue against a reservation's pledge, settle repayments and
//! let the owner forfeit.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolInterface;

use outbe_core_common::erc::ERC165_INTERFACE_ID;
use outbe_primitives::dispatch::{dispatch_call, mutate, view};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

#[allow(clippy::too_many_arguments)]
mod abi {
    alloy_sol_types::sol!("../../../contracts/precompiles/src/ICredisFactory.sol");
}
pub use abi::ICredisFactory;

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
                issueCredis(c) => mutate(&storage, c, caller, |sender, c| {
                    let (credis_id, amount_minor) = runtime::issue_credis(
                        storage.clone(),
                        sender,
                        c.credisReservationId,
                        &c.pledge,
                    )?;
                    Ok(ICredisFactory::issueCredisReturn {
                        credisId: credis_id,
                        amountMinor: amount_minor,
                    })
                }),
                settleCredis(c) => mutate(&storage, c, caller, |sender, c| {
                    let (amount, fee) = runtime::settle_credis(
                        storage.clone(),
                        sender,
                        c.credisId,
                        c.asset,
                        c.amountCapMinor,
                    )?;
                    Ok(ICredisFactory::settleCredisReturn {
                        settledAmountMinor: amount,
                        settledFeeMinor: fee,
                    })
                }),
                quoteSettlement(c) => view(c, |c| {
                    let quote = runtime::quote_settlement(&storage, c.credisId)?;
                    Ok(ICredisFactory::quoteSettlementReturn {
                        payoffMinor: quote.payoff_minor,
                        feeMinor: quote.fee_minor,
                    })
                }),
                getCallState(c) => view(c, |c| crate::call_state::call_state(&storage, c.credisId)),
                forfeitCredis(c) => mutate(&storage, c, caller, |sender, c| {
                    let forfeit = runtime::forfeit_by_owner(storage.clone(), sender, c.credisId)?;
                    Ok(ICredisFactory::forfeitCredisReturn {
                        forfeitedGratisMinor: forfeit.forfeited_gratis_minor,
                        forfeitedAmountMinor: forfeit.forfeited_amount_minor,
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
