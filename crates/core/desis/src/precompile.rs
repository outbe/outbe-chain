//! ABI dispatch for the Desis precompile at `DESIS_ADDRESS`.
//!
//! Routes bid ingestion and clearing calls from OriginRouter to the
//! runtime. Encoding only; all logic lives in `runtime.rs`.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, mutate_void, mutate_void_payable, view};
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;
use crate::schema::BidData;

/// Interface ID probed by `OriginRouter.wire` — `type(IDesis).interfaceId` of the
/// router-facing interface in contracts/intex/src/origin/interfaces/IDesis.sol
/// (XOR of its 5 function selectors).
pub(crate) const IDESIS_INTERFACE_ID: [u8; 4] = [0xde, 0xc7, 0x95, 0xe6];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IDesis.sol"
);

pub fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    dispatch_call(data, IDesis::IDesisCalls::abi_decode, |call| {
        use IDesis::IDesisCalls::*;
        match call {
            processBidsBatch(c) => mutate_void(c, caller, |sender, c| {
                let bids = bids_from_sol_arrays(
                    &c.bidderAddresses,
                    &c.intexQuantities,
                    &c.intexBidRates,
                    &c.timestamps,
                )?;
                runtime::process_bids_batch(
                    storage.clone(),
                    sender,
                    c.worldwideDay,
                    c.srcChainId,
                    c.relayGeneration,
                    c.batchIndex,
                    c.totalBatches,
                    bids,
                )
            }),
            processBidsDone(c) => mutate_void(c, caller, |sender, c| {
                runtime::process_bids_done(
                    storage.clone(),
                    sender,
                    c.worldwideDay,
                    c.srcChainId,
                    c.relayGeneration,
                    c.totalBatches,
                    c.totalBids,
                )
            }),
            clearAuction(c) => mutate_void_payable(c, caller, value, |sender, c, _val| {
                runtime::clear_auction(storage.clone(), sender, c.worldwideDay).map(|_| ())
            }),
            getAuctionStage(c) => view(c, |c| {
                use crate::schema::DesisContract;
                let contract = storage.contract::<DesisContract>();
                let stage = contract.read_stage(c.worldwideDay)?;
                Ok(IDesis::AuctionStage::try_from(stage as u8)
                    .unwrap_or(IDesis::AuctionStage::None))
            }),
            getBidsCount(c) => view(c, |c| {
                use crate::schema::DesisContract;
                let contract = storage.contract::<DesisContract>();
                let count = contract.day_bid_count.read(&c.worldwideDay)?;
                Ok(U256::from(count))
            }),
            getChainBidsCount(c) => view(c, |c| {
                use crate::schema::DesisContract;
                let contract = storage.contract::<DesisContract>();
                let key = DesisContract::chain_key(c.worldwideDay, c.srcChainId);
                let count = contract.chain_bid_count.read(&key)?;
                Ok(U256::from(count))
            }),
            isChainDone(c) => view(c, |c| {
                use crate::schema::DesisContract;
                let contract = storage.contract::<DesisContract>();
                let key = DesisContract::chain_key(c.worldwideDay, c.srcChainId);
                Ok(contract.chain_done.read(&key)? != 0)
            }),
            supportsInterface(c) => view(c, |c| {
                let id: [u8; 4] = c.interfaceId.0;
                Ok(id == ERC165_INTERFACE_ID || id == IDESIS_INTERFACE_ID)
            }),
        }
    })
}

fn bids_from_sol_arrays(
    bidders: &[Address],
    quantities: &[u16],
    rates: &[u32],
    timestamps: &[u32],
) -> Result<Vec<BidData>> {
    let len = bidders.len();
    if len != quantities.len() || len != rates.len() || len != timestamps.len() {
        return Err(outbe_primitives::error::PrecompileError::Revert(
            "processBidsBatch: array length mismatch".into(),
        ));
    }
    Ok((0..len)
        .map(|i| BidData {
            bidder_address: bidders[i],
            intex_quantity: quantities[i],
            intex_bid_rate: rates[i],
            timestamp: timestamps[i],
        })
        .collect())
}
