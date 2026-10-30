use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolInterface;
use outbe_primitives::dispatch::{dispatch_call, mutate, view};
use outbe_primitives::error::Result;
use outbe_vaultrouter::settlement::SettlementPayment;

use crate::runtime;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

// Alloy 1.6 generates event constructors with the Solidity argument lists.
#[allow(clippy::too_many_arguments)]
mod abi {
    alloy_sol_types::sol!(
        #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
        "../../../contracts/precompiles/src/INodFactory.sol"
    );
}
pub use abi::INodFactory;

/// Dispatches NodFactory calls against EVM state.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    #[cfg(feature = "e2e-test")]
    if let Some(answer) = crate::test_arming_e2e::dispatch(&storage, data) {
        return answer;
    }
    dispatch_call(data, INodFactory::INodFactoryCalls::abi_decode, |call| {
        use INodFactory::INodFactoryCalls::*;
        match call {
            settleNod(c) => mutate(&storage, c, caller, |sender, c| {
                runtime::settle_nod(
                    &storage,
                    c.nodId,
                    SettlementPayment {
                        payer: sender,
                        asset: c.asset,
                        snapshot_id: c.snapshotId,
                    },
                )?;
                Ok(INodFactory::settleNodReturn {})
            }),
            quoteSettlement(c) => view(c, |c| {
                let (settlement_currency, amount, snapshot_id) =
                    runtime::quote_settlement(&storage, c.nodId, c.asset)?;
                Ok(INodFactory::quoteSettlementReturn {
                    settlementCurrency: settlement_currency,
                    paymentMinor: amount,
                    snapshotId: snapshot_id,
                })
            }),
            mineGratis(c) => mutate(&storage, c, caller, |sender, c| {
                let auth = outbe_gratisfactory::api::ModifyAuth {
                    mac: c.mac.0,
                    op_nonce: c.opNonce,
                };
                let encrypted_balance = runtime::mine_gratis(
                    &storage,
                    runtime::MineGratisRequest {
                        caller: sender,
                        nod_id: c.nodId,
                        nonce: c.nonce,
                        auth,
                    },
                )?;
                Ok(encrypted_balance)
            }),
        }
    })
}
