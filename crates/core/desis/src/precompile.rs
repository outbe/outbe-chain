//! ABI dispatch for the Desis precompile at `DESIS_ADDRESS`.
//!
//! Routes bid ingestion and clearing calls from OriginMessenger to the
//! runtime. Encoding only; all logic lives in `runtime.rs`.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, mutate_void, mutate_void_payable, view};
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;
use crate::schema::BidData;

/// `IDesis` interface ID (XOR of non-ERC-165 selectors in IDesis).
pub(crate) const IDESIS_INTERFACE_ID: [u8; 4] = [0x7b, 0x57, 0x26, 0x34];

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
            processBidsBatch(c) => mutate_void(c, caller, |_sender, c| {
                let bids = bids_from_sol_arrays(
                    &c.bidderAddresses,
                    &c.intexQuantities,
                    &c.intexBidPrices,
                    &c.timestamps,
                )?;
                runtime::process_bids_batch(
                    storage.clone(),
                    c.seriesId,
                    c.srcEid,
                    c.isLast,
                    c.relayGeneration,
                    bids,
                )
            }),
            clearAuction(c) => mutate_void_payable(c, caller, value, |_sender, c, _val| {
                runtime::clear_auction(storage.clone(), c.seriesId).map(|_| ())
            }),
            getAuctionStage(c) => view(c, |c| {
                use crate::schema::DesisContract;
                let contract = storage.contract::<DesisContract>();
                let stage = contract.read_stage(c.seriesId)?;
                Ok(IDesis::AuctionStage::try_from(stage as u8)
                    .unwrap_or(IDesis::AuctionStage::None))
            }),
            getBidsCount(c) => view(c, |c| {
                use crate::schema::DesisContract;
                let contract = storage.contract::<DesisContract>();
                let count = contract.read_bid_count(c.seriesId)?;
                Ok(U256::from(count))
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
    prices: &[u64],
    timestamps: &[u32],
) -> Result<Vec<BidData>> {
    let len = bidders.len();
    if len != quantities.len() || len != prices.len() || len != timestamps.len() {
        return Err(outbe_primitives::error::PrecompileError::Revert(
            "processBidsBatch: array length mismatch".into(),
        )
        .into());
    }
    Ok((0..len)
        .map(|i| BidData {
            bidder_address: bidders[i],
            intex_quantity: quantities[i],
            intex_bid_price: prices[i],
            timestamp: timestamps[i],
        })
        .collect())
}
