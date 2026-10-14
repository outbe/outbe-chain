use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolInterface;
use outbe_compressed_entities::{ExecutionReaders, ParentBodySource};
use outbe_intex::SeriesId;
use outbe_primitives::dispatch::{dispatch_call, metadata, mutate, mutate_void, view};
use outbe_primitives::error::Result;

use crate::errors::GemFactoryError;
use crate::runtime;
use crate::schema::GemFactoryContract;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

// Alloy 1.6 generates event constructors with the Solidity argument lists.
#[allow(clippy::too_many_arguments)]
mod abi {
    alloy_sol_types::sol!(
        #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
        "../../../contracts/precompiles/src/IGemFactory.sol"
    );
}
pub use abi::IGemFactory;

/// Dispatches Gem Factory calls through the block-scoped compressed-body lifecycle.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    readers: ExecutionReaders<'_, '_, impl ParentBodySource>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    let ExecutionReaders { scope, parent } = readers;
    #[cfg(feature = "e2e-test")]
    if let Some(answer) = crate::test_arming::dispatch(&storage, scope, parent, data) {
        return answer;
    }
    dispatch_call(data, IGemFactory::IGemFactoryCalls::abi_decode, |call| {
        use IGemFactory::IGemFactoryCalls::*;
        match call {
            issueGemPosition(c) => mutate(&storage, c, caller, |sender, c| {
                runtime::issue_gem_position(
                    &storage,
                    sender,
                    SeriesId::from(c.sourceIntexId),
                    c.units,
                )
            }),
            issueGem(c) => mutate(&storage, c, caller, |sender, c| {
                runtime::issue_merchant_gem(
                    &storage,
                    scope,
                    parent,
                    crate::schema::MerchantGemIssueParams {
                        caller: sender,
                        position_id: c.positionId,
                        owner: c.owner,
                        promis_load: c.promisLoadMinor,
                    },
                )
            }),
            settleGem(c) => mutate_void(&storage, c, caller, |sender, c| {
                runtime::settle_gem(
                    &storage,
                    scope,
                    parent,
                    crate::schema::GemSettlementParams {
                        caller: sender,
                        gem_id: c.gemId,
                        asset: c.asset,
                        snapshot_id: c.snapshotId,
                    },
                )
            }),
            minePromis(c) => mutate(&storage, c, caller, |_sender, c| {
                let auth = outbe_promisfactory::api::ModifyAuth {
                    mac: c.mac.0,
                    op_nonce: c.opNonce,
                };
                runtime::mine_promis(
                    &storage,
                    scope,
                    parent,
                    crate::schema::GemMiningProof {
                        gem_id: c.gemId,
                        nonce: c.nonce,
                    },
                    auth,
                )
            }),
            getStatistics(_) => metadata::<IGemFactory::getStatisticsCall>(|| {
                let factory = GemFactoryContract::new(storage.clone());
                Ok(IGemFactory::getStatisticsReturn {
                    totalGemsIssued: factory.total_gems_issued.read()?,
                    totalCapacityMinor: factory.total_capacity_minor.read()?,
                })
            }),

            quoteSettlement(c) => metadata::<IGemFactory::quoteSettlementCall>(|| {
                let (settlement_currency, amount, snapshot_id) =
                    runtime::quote_settlement(&storage, scope, parent, c.gemId, c.asset)?;
                Ok(IGemFactory::quoteSettlementReturn {
                    settlementCurrency: settlement_currency,
                    paymentMinor: amount,
                    snapshotId: snapshot_id,
                })
            }),

            getPosition(c) => view(c, |c| runtime::position_data(&storage, c.positionId)),

            balanceOf(c) => view(c, |c| {
                GemFactoryContract::new(storage.clone())
                    .balance_of(c.owner)
                    .map(U256::from)
            }),
            ownerOf(c) => view(c, |c| {
                GemFactoryContract::new(storage.clone()).owner_of(c.positionId)
            }),
            tokenURI(c) => view(c, |c| {
                GemFactoryContract::new(storage.clone()).token_uri(c.positionId)
            }),
            tokenOfOwnerByIndex(c) => view(c, |c| {
                let idx = u32::try_from(c.index).map_err(|_| GemFactoryError::IndexOutOfBounds)?;
                GemFactoryContract::new(storage.clone()).token_of_owner_by_index(c.owner, idx)
            }),
        }
    })
}
