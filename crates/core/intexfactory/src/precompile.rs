//! ABI dispatch for the IntexFactory precompile at `INTEX_FACTORY_ADDRESS`.
//!
//! Routing only: decode -> runtime -> encode. The settle calls / `minePromis` name the
//! owner they act for, so `caller = msg.sender` only binds the payment.
//! None accept value, except `distribute`, which credits auction proceeds.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolInterface};

use outbe_intex::SeriesId;
use outbe_primitives::dispatch::{
    dispatch_call, metadata, mutate, mutate_void, mutate_void_payable, reject_value_unless_payable,
    view,
};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[IIntexFactory::distributeCall::SELECTOR];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IIntexFactory.sol"
);

// Arming the proceeds fan-in is production work of the issuance leg, which a
// payout e2e never reaches: it runs no auction, so it issues nothing. This
// stages that one precondition and exists only in a throwaway build.
#[cfg(feature = "e2e-test")]
sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IIntexFactoryTestArming {
        function armProceedsForTest(uint32 worldwideDay, uint32[] chains, uint64 deadline) external;
        function seedDayVwapsForTest(uint16 isoCode, uint32 days, uint256 value) external;
        function issueForTest(
            bytes14[] seriesIds,
            uint16[] issuanceCurrencies,
            uint32 worldwideDay,
            uint32 issuedAt,
            uint32 issuedUnits,
            uint128 promisLoadMinor,
            uint256 entryPriceMinor,
            uint16 referenceCurrency,
            address[] recipients,
            uint256[] units,
            uint32[] recipientChains,
            uint32[] snapshotChains
        ) external;
        function closeCallNoticeForTest(uint16 isoCode, uint32 worldwideDay, uint64 deadline) external;
    }
}

/// Move a called group onto `deadline`, and with it into that deadline's bucket. The
/// sweep opens a bucket only once its hour has closed, so a notice that lapsed minutes
/// ago would otherwise idle out the rest of the hour.
#[cfg(feature = "e2e-test")]
fn requeue_called_group(
    storage: &StorageHandle<'_>,
    iso_code: u16,
    worldwide_day: u32,
    deadline: u64,
) -> Result<()> {
    use crate::schema::IntexFactoryContract;
    use outbe_primitives::storage::types::Storable;
    use outbe_primitives::time::WorldwideDay;

    let mut factory = IntexFactoryContract::new(storage.clone());
    let worldwide_day = WorldwideDay::from(worldwide_day);
    let key = IntexFactoryContract::scoped(iso_code, worldwide_day.value());
    let count = factory.called_group_count.read(&key)?;
    if count == 0 {
        return Err(outbe_primitives::error::PrecompileError::Revert(
            "closeCallNoticeForTest: no called group".into(),
        ));
    }
    let mut members = Vec::with_capacity(count as usize);
    for index in 0..count {
        let word = factory
            .called_group_members
            .read(&IntexFactoryContract::group_member_key(
                iso_code,
                worldwide_day,
                index,
            ))?;
        members.push(SeriesId::from_word(word));
    }
    factory.remove_called_group(iso_code, worldwide_day)?;
    factory.push_called_group(iso_code, worldwide_day, deadline, &members)
}

/// The e2e-only arming selectors. `None` leaves `data` to the published interface.
#[cfg(feature = "e2e-test")]
fn dispatch_test_arming(storage: &StorageHandle<'_>, data: &[u8]) -> Result<Option<Bytes>> {
    if let Ok(call) = IIntexFactoryTestArming::seedDayVwapsForTestCall::abi_decode(data) {
        seed_day_vwaps_for_test(storage, call)?;
        return Ok(Some(Bytes::new()));
    }
    if let Ok(call) = IIntexFactoryTestArming::issueForTestCall::abi_decode(data) {
        issue_for_test(storage, call)?;
        return Ok(Some(Bytes::new()));
    }
    if let Ok(call) = IIntexFactoryTestArming::closeCallNoticeForTestCall::abi_decode(data) {
        requeue_called_group(storage, call.isoCode, call.worldwideDay, call.deadline)?;
        return Ok(Some(Bytes::new()));
    }
    if let Ok(call) = IIntexFactoryTestArming::armProceedsForTestCall::abi_decode(data) {
        outbe_intex::api::arm_proceeds(
            storage,
            call.worldwideDay.into(),
            &call.chains,
            call.deadline,
        )?;
        return Ok(Some(Bytes::new()));
    }
    Ok(None)
}

#[cfg(feature = "e2e-test")]
fn seed_day_vwaps_for_test(
    storage: &StorageHandle<'_>,
    call: IIntexFactoryTestArming::seedDayVwapsForTestCall,
) -> Result<()> {
    // What `set_vwap` does in this module's own tests: the per-day value keyed by
    // the pair's registry index, and the watermark the begin-block hook would move.
    // This adds nothing to the Oracle crate. It only writes data for the days the crate serves.
    use outbe_oracle::schema::OracleContract;
    use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};

    let oracle = OracleContract::new(storage.clone());
    let pair = outbe_oracle::api::AddressPair::new_coen_to(call.isoCode);
    let pair_id = oracle.pair_index_of(pair)?;
    let mut day = previous_date_key(timestamp_to_date_key(storage.timestamp()?.to::<u64>()));
    for _ in 0..call.days {
        oracle.record_utc_day_vwap(day, pair_id, call.value)?;
        if oracle.utc_day_vwap_last_finalized.read()? < day {
            oracle.utc_day_vwap_last_finalized.write(day)?;
        }
        day = previous_date_key(day);
    }
    // The VWAP pusher sends the rewritten days again.
    let factory = crate::schema::IntexFactoryContract::new(storage.clone());
    if factory.vwap_sent_day.read()? > day {
        factory.vwap_sent_day.write(day)?;
    }
    Ok(())
}

#[cfg(feature = "e2e-test")]
fn issue_for_test(
    storage: &StorageHandle<'_>,
    call: IIntexFactoryTestArming::issueForTestCall,
) -> Result<()> {
    // Mirrors the clearing engine: issue every series first, then send the day's
    // legs once. Sending per series would declare a one-chunk day twice, and the
    // second delivery is dropped as a conflicting repeat rather than applied.
    if call.seriesIds.len() != call.issuanceCurrencies.len() {
        return Err(outbe_primitives::error::PrecompileError::Revert(
            "issueForTest: a currency per series".into(),
        ));
    }
    let mut legs = Vec::new();
    let ids = call.seriesIds.clone();
    for (series_id, issuance_currency) in call.seriesIds.into_iter().zip(call.issuanceCurrencies) {
        legs.extend(crate::api::issue(
            storage,
            crate::schema::IssuanceParams {
                series_id: SeriesId::from(series_id),
                worldwide_day: call.worldwideDay.into(),
                issued_units: call.issuedUnits,
                promis_load_minor: call.promisLoadMinor,
                entry_price_minor: call.entryPriceMinor,
                issuance_currency,
                reference_currency: call.referenceCurrency,
                recipients: call.recipients.clone(),
                units: call.units.clone(),
                recipient_chains: call.recipientChains.clone(),
                snapshot_chains: call.snapshotChains.clone(),
            },
        )?);
    }
    // The Called sweep counts breach days from `issued_at`, so a scenario that
    // seeds those days places issuance behind them, on every chain alike. Zero
    // keeps the stamp the engine wrote.
    if call.issuedAt != 0 {
        for series_id in ids {
            outbe_intex::api::set_issued_at(storage, SeriesId::from(series_id), call.issuedAt)?;
        }
        for leg in &mut legs {
            leg.payload.issuedAt = call.issuedAt;
        }
    }
    crate::api::send_issuance(storage, legs)
}

pub fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    // IntexFactory is a payable route, so the boundary credits value to this
    // address. Every selector the module has not published refuses it here.
    reject_value_unless_payable(data, PAYABLE_SELECTORS, &value)?;
    #[cfg(feature = "e2e-test")]
    if let Some(out) = dispatch_test_arming(&storage, data)? {
        return Ok(out);
    }
    dispatch_call(
        data,
        IIntexFactory::IIntexFactoryCalls::abi_decode,
        |call| {
            use IIntexFactory::IIntexFactoryCalls::*;
            match call {
                settleIntex(c) => mutate_void(c, caller, |sender, c| {
                    runtime::settle_intex(
                        &storage,
                        SeriesId::from(c.seriesId),
                        c.owner,
                        sender,
                        c.units,
                        c.asset,
                        c.snapshotId,
                    )
                }),
                quoteSettlement(c) => metadata::<IIntexFactory::quoteSettlementCall>(|| {
                    let (settlement_currency, payment_minor, snapshot_id) =
                        runtime::quote_settlement(
                            &storage,
                            SeriesId::from(c.seriesId),
                            c.asset,
                            c.units,
                        )?;
                    Ok(IIntexFactory::quoteSettlementReturn {
                        settlementCurrency: settlement_currency,
                        paymentMinor: payment_minor,
                        snapshotId: snapshot_id,
                    })
                }),
                isSeriesQualified(c) => metadata::<IIntexFactory::isSeriesQualifiedCall>(|| {
                    runtime::is_series_qualified(&storage, SeriesId::from(c.seriesId))
                }),
                maxUtcDayVwapSince(c) => view(c, |c| {
                    outbe_oracle::api::max_utc_day_vwap_since(
                        storage.clone(),
                        c.isoCode,
                        c.fromUtcDay,
                    )
                }),
                // Off-chain the owner brute-forces `nonce` so the work hash
                // SHA256(owner ++ promisAmount_be32 ++ seriesId ++ seq_be4 ++ nonce_be8)
                // has the protocol's leading zero bytes. `seq` is the on-chain
                // per-(series, owner) counter.
                minePromis(c) => mutate(c, caller, |_sender, c| {
                    let auth = outbe_promisfactory::api::ModifyAuth {
                        mac: c.mac.0,
                        op_nonce: c.opNonce,
                    };
                    runtime::mine_promis(
                        &storage,
                        SeriesId::from(c.seriesId),
                        c.owner,
                        c.units,
                        c.nonce,
                        auth,
                    )
                }),
                // The only payable selector: credits auction proceeds (msg.value)
                // from the source chain into the day's pot.
                distribute(c) => {
                    mutate_void_payable(c, PAYABLE_SELECTORS, caller, value, |sender, c, val| {
                        runtime::distribute(
                            &storage,
                            sender,
                            c.worldwideDay.into(),
                            c.srcChainId,
                            val,
                        )
                    })
                }
                // Permissionless: the merkle proof is the authorization, so the
                // sender is irrelevant to the outcome.
                payContributorBatch(c) => mutate_void(c, caller, |_sender, c| {
                    runtime::pay_contributor_batch(
                        &storage,
                        c.worldwideDay,
                        c.startIndex,
                        &c.leaves,
                        &c.proof,
                    )
                }),
                contributorPayoutRound(c) => view(c, |c| {
                    runtime::contributor_payout_round(&storage, c.worldwideDay)
                }),
                contributorPaidWord(c) => view(c, |c| {
                    outbe_intex::api::paid_leaves_word(&storage, c.worldwideDay, c.wordIndex)
                }),
                seriesUnitCounts(c) => view(c, |c| {
                    runtime::series_unit_counts(&storage, c.seriesId.into())
                }),
                ownerBalances(c) => view(c, |c| {
                    runtime::owner_balances(&storage, c.seriesId.into(), c.owner)
                }),
            }
        },
    )
}
