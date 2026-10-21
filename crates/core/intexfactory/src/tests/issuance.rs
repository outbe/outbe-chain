use super::*;
use outbe_primitives::time::SECONDS_PER_DAY_U32;
use outbe_vaultrouter::settlement::SettlementPayment;

#[test]
fn issue_distinguishes_timestamp_narrowing_from_protocol_width() {
    with_factory(|storage| {
        for (timestamp, expected) in [
            (
                U256::from(u32::MAX) + U256::ONE,
                outbe_primitives::error::PrecompileError::from(
                    crate::errors::IntexFactoryError::BlockTimestampExceedsU32,
                ),
            ),
            (
                U256::from(u64::MAX),
                outbe_primitives::error::PrecompileError::from(
                    crate::errors::IntexFactoryError::BlockTimestampExceedsU32,
                ),
            ),
            (
                U256::from(u64::MAX) + U256::ONE,
                outbe_primitives::error::PrecompileError::Revert("timestamp exceeds u64".into()),
            ),
            (
                U256::MAX,
                outbe_primitives::error::PrecompileError::Revert("timestamp exceeds u64".into()),
            ),
        ] {
            storage.set_block_timestamp(timestamp).unwrap();
            assert_eq!(
                runtime::issue_intex(&storage, sample(7))
                    .err()
                    .unwrap()
                    .to_string(),
                expected.to_string()
            );
            assert!(!outbe_intex::api::series_exists(&storage, sid(7)).unwrap());
        }
        storage.set_block_timestamp(U256::from(u32::MAX)).unwrap();
        runtime::issue_intex(&storage, sample(7)).unwrap();
        assert_eq!(
            outbe_intex::api::read_series(&storage, sid(7))
                .unwrap()
                .issued_at,
            u32::MAX
        );
    });
}

#[test]
fn issue_creates_series_in_registry() {
    with_factory(|s| {
        runtime::issue_intex(&s, sample(7)).unwrap();

        // The series is captured in Intex with the issuance identity.
        let r = outbe_intex::api::read_series(&s, sid(7)).unwrap();
        assert_eq!(r.series_id, sid(7));
        assert_eq!(r.promis_load_minor, U256::from(PROMIS_LOAD_MINOR));
        assert_eq!(r.entry_price_minor, U256::from(ENTRY_PRICE));
        // The trigger is derived from the clearing price at issuance.
        assert_eq!(r.issued_units, 100);
        assert_eq!(r.call_notice_period_seconds, CALL_NOTICE_PERIOD);
        // Window/threshold/call-period are IntexFactory protocol constants now.
        assert_eq!(r.call_price_minor, U256::from(EXPECTED_TRIGGER));
        assert_eq!(
            r.call_trigger(),
            outbe_intex::IntexCallTrigger {
                call_window_seconds: 28 * SECONDS_PER_DAY_U32,
                call_threshold_seconds: 21 * SECONDS_PER_DAY_U32,
                call_notice_period_seconds: CALL_NOTICE_PERIOD,
            }
        );
        // Born Issued. `issued_at` is the block timestamp.
        assert_eq!(
            series_state(&s, r.series_id),
            outbe_intex::IntexState::Issued
        );
        assert_eq!(r.issued_at, ISSUED_AT);
        assert_eq!(outbe_intex::api::call_stamp(&s, &r).unwrap().called_at, 0);
    });
}

#[test]
fn call_terms_mark_up_the_entry_with_the_live_profile() {
    with_factory(|s| {
        let terms = crate::api::call_terms(&s).unwrap();
        assert_eq!(
            terms.call_price(U256::from(ENTRY_PRICE)).unwrap(),
            U256::from(EXPECTED_TRIGGER)
        );
        assert_eq!(terms.call_trigger, prod_trigger());

        select_dev_profile(&s);
        let dev = outbe_intex::config::IntexParams::DEV;
        let terms = crate::api::call_terms(&s).unwrap();
        assert_eq!(
            terms.call_price(U256::from(ENTRY_PRICE)).unwrap(),
            runtime::marked_up(U256::from(ENTRY_PRICE), dev.call_rate).unwrap()
        );
        assert_eq!(
            terms.call_trigger,
            outbe_intex::IntexCallTrigger {
                call_window_seconds: dev.call_window_seconds,
                call_threshold_seconds: dev.call_threshold_seconds,
                call_notice_period_seconds: dev.call_notice_period_seconds,
            }
        );
    });
}

fn select_dev_profile(s: &StorageHandle<'_>) {
    outbe_intex::IntexContract::new(s.clone())
        .config_profile
        .write(outbe_intex::config::PROFILE_DEV)
        .unwrap();
}

/// The auction fixed the conditions at its start. A profile changed since then does
/// not reach the series or the legs.
#[test]
fn issuance_seals_the_given_conditions_not_the_live_profile() {
    with_factory(|s| {
        select_dev_profile(&s);
        let params = sample(7);
        let legs = runtime::issue_intex(&s, params.clone()).unwrap();

        let r = outbe_intex::api::read_series(&s, sid(7)).unwrap();
        assert_eq!(r.promis_load_minor, U256::from(params.promis_load_minor));
        assert_eq!(r.entry_price_minor, params.entry_price_minor);
        assert_eq!(r.call_price_minor, params.call_price_minor);
        assert_eq!(r.call_trigger(), params.call_trigger);
        assert_eq!(r.issued_at, ISSUED_AT);

        assert_eq!(legs.len(), 1);
        let leg = &legs[0].payload;
        assert_eq!(leg.promisLoadMinor, params.promis_load_minor);
        assert_eq!(U256::from(leg.entryPriceMinor), params.entry_price_minor);
        assert_eq!(U256::from(leg.callPriceMinor), params.call_price_minor);
        assert_eq!(leg.callWindow, params.call_trigger.call_window_seconds);
        assert_eq!(
            leg.callThreshold,
            params.call_trigger.call_threshold_seconds
        );
        assert_eq!(
            leg.callNoticePeriod,
            params.call_trigger.call_notice_period_seconds
        );
        assert_eq!(leg.issuedAt, ISSUED_AT);
    });
}

#[test]
fn issue_rejects_duplicate_series() {
    with_factory(|s| {
        runtime::issue_intex(&s, sample(7)).unwrap();
        // The registry record-create rejects a duplicate series id.
        assert!(runtime::issue_intex(&s, sample(7)).is_err());
    });
}

#[test]
fn issue_zero_winners_leaves_the_day_untouched() {
    with_factory(|s| {
        let mut p = sample(7);
        p.issued_units = 0;
        runtime::issue_intex(&s, p).unwrap();

        assert!(!outbe_intex::api::series_exists(&s, sid(7)).unwrap());
    });
}

#[test]
fn issue_sends_every_snapshot_chain_the_series() {
    with_factory(|s| {
        let mut p = sample(7);
        p.snapshot_chains = vec![10, 20, 30];
        let legs = runtime::issue_intex(&s, p).unwrap();

        let chains: Vec<u32> = legs.iter().map(|leg| leg.chain_id).collect();
        assert_eq!(chains, vec![10, 20, 30]);
        for leg in &legs {
            assert_eq!(SeriesId::from(leg.payload.seriesId), sid(7));
            assert_eq!(leg.payload.issuedUnits, 100);
        }
    });
}

#[test]
fn issue_enrolls_series_in_dense_enumeration() {
    with_factory(|s| {
        runtime::issue_intex(&s, sample(11)).unwrap();
        runtime::issue_intex(&s, sample(22)).unwrap();
        assert_eq!(outbe_intex::api::total_series(&s).unwrap(), 2);
        assert_eq!(outbe_intex::api::series_id_at(&s, 0).unwrap(), sid(11));
        assert_eq!(outbe_intex::api::series_id_at(&s, 1).unwrap(), sid(22));
    });
}

#[test]
fn call_derivation() {
    let call = outbe_primitives::markup::marked_up(U256::from(ENTRY_PRICE), CALL_RATE_PCT).unwrap();
    assert_eq!(call, U256::from(EXPECTED_TRIGGER));

    let one = U256::from(1_000_000u64);
    assert_eq!(
        outbe_primitives::markup::marked_up(one, CALL_RATE_PCT).unwrap(),
        U256::from(2_280_000u64)
    );
}

#[test]
fn coen_iso_one_maps_to_the_center_price_bin_at_six_decimals() {
    assert_eq!(
        outbe_primitives::call_bins::price_to_bin(U256::from(1_000_000u64)).unwrap(),
        REAL_ID_SHIFT as u32
    );
}

#[test]
fn coen_iso_wire_price_preserves_the_six_decimal_integer() {
    assert_eq!(
        runtime::to_wire_price(U256::from(1_234_567u64)).unwrap(),
        1_234_567
    );
    assert_eq!(
        runtime::to_wire_price(U256::from(u64::MAX)).unwrap(),
        u64::MAX
    );
    assert!(runtime::to_wire_price(U256::from(u64::MAX) + U256::ONE).is_err());
}

fn leg(chain_id: u32, day: u32, series: u32) -> runtime::IssuanceLeg {
    let payload = crate::sol_ext::IOriginRouter::IssuanceInstructionsParams {
        seriesId: sid(series).into(),
        worldwideDay: day,
        issuedAt: ISSUED_AT,
        issuedUnits: 1,
        promisLoadMinor: PROMIS_LOAD_MINOR,
        entryPriceMinor: 0,
        callNoticePeriod: 0,
        issuanceCurrency: 840,
        referenceCurrency: 840,
        callWindow: 0,
        callThreshold: 0,
        callPriceMinor: 0,
    };
    runtime::IssuanceLeg { chain_id, payload }
}

fn shape(
    messages: &[(
        u32,
        Vec<crate::sol_ext::IOriginRouter::IssuanceInstructionsParams>,
    )],
) -> Vec<(u32, usize)> {
    messages
        .iter()
        .map(|(chain, series)| (*chain, series.len()))
        .collect()
}

#[test]
fn a_chains_series_travel_together_up_to_the_message_cap() {
    // One series past the cap on one chain: the series cap splits them.
    let over = MAX_SERIES_PER_MESSAGE as u32 + 1;
    let legs: Vec<_> = (1..=over).map(|s| leg(10, 1, s)).collect();
    assert_eq!(
        shape(&runtime::pack_issuance_messages(legs)),
        vec![(10, MAX_SERIES_PER_MESSAGE), (10, 1)]
    );
}

#[test]
fn the_message_caps_match_the_codec() {
    use crate::constants::MAX_SERIES_PER_MARK;
    use outbe_primitives::test_utils::sol_interface::sol_constant;
    const CODEC: &str =
        include_str!("../../../../../contracts/intex/src/shared/libs/BridgeMsgCodec.sol");
    assert_eq!(
        sol_constant(CODEC, "MAX_SERIES_PER_ISSUANCE"),
        Some(MAX_SERIES_PER_MESSAGE)
    );
    assert_eq!(
        sol_constant(CODEC, "MAX_SERIES_PER_MARK"),
        Some(MAX_SERIES_PER_MARK)
    );
}

#[test]
fn a_chains_series_batch_even_when_another_chain_comes_between_them() {
    // A day emits its legs series by series, so one chain's legs are never adjacent.
    // Both of chain 10's series still travel together, or the batching would do
    // nothing precisely when a day has several currency pairs.
    let legs = vec![leg(10, 1, 1), leg(20, 1, 1), leg(10, 1, 2)];
    assert_eq!(
        shape(&runtime::pack_issuance_messages(legs)),
        vec![(10, 2), (20, 1)]
    );
}

fn run_shape(runs: &[runtime::IssuanceRun]) -> Vec<((u32, u32), Vec<usize>)> {
    runs.iter()
        .map(|(key, messages)| (*key, messages.iter().map(Vec::len).collect()))
        .collect()
}

#[test]
fn a_chains_chunks_form_one_run_even_when_another_chain_interleaves() {
    // One day, one chain, series spanning two messages, with another chain's message in between.
    let full = MAX_SERIES_PER_MESSAGE as u32;
    let mut legs: Vec<_> = (1..=full).map(|s| leg(10, 1, s)).collect();
    legs.push(leg(20, 1, 1));
    legs.push(leg(10, 1, full + 1));
    let runs = runtime::chunk_issuance_messages(runtime::pack_issuance_messages(legs));
    assert_eq!(
        run_shape(&runs),
        vec![
            ((10, 1), vec![MAX_SERIES_PER_MESSAGE, 1]),
            ((20, 1), vec![1])
        ]
    );
}

#[test]
fn a_chains_days_are_numbered_as_separate_runs() {
    let packed = vec![
        (10, vec![leg(10, 1, 1).payload]),
        (10, vec![leg(10, 2, 2).payload]),
    ];
    let runs = runtime::chunk_issuance_messages(packed);
    assert_eq!(
        run_shape(&runs),
        vec![((10, 1), vec![1]), ((10, 2), vec![1])]
    );
}

#[test]
fn a_single_message_day_is_chunk_zero_of_one() {
    let runs =
        runtime::chunk_issuance_messages(runtime::pack_issuance_messages(vec![leg(7, 1, 3)]));
    assert_eq!(run_shape(&runs), vec![((7, 1), vec![1])]);
}

/// Issue a series whose issuance currency differs from its reference, with a
/// payment token reporting `iso` and 18 decimals and a registered vault.
fn with_dual_currency_series<R>(iso: u64, f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = dual_currency_series(iso);
    StorageHandle::enter(&mut storage, f)
}

/// A payment token that accepts `transferFrom` but never credits the factory, so a
/// settlement that reaches the payment fails there with `SettlementAmountMismatch`.
pub(super) fn stub_token_that_never_credits(storage: &mut HashMapStorageProvider) {
    use crate::sol_ext::IERC20;
    storage.stub_sub_call_at_selector(settlement_asset(), IERC20::balanceOfCall::SELECTOR, word(0));
    storage.stub_sub_call_at_selector(
        settlement_asset(),
        IERC20::transferFromCall::SELECTOR,
        word(1),
    );
}

/// The storage behind [`with_dual_currency_series`], for tests that move the clock.
pub(super) fn dual_currency_series(iso: u64) -> HashMapStorageProvider {
    use crate::sol_ext::{IReferenceCurrency, IERC1155, IERC20};
    use outbe_vaultrouter::api::IVaultRouter;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    storage.stub_sub_call_at(crate::constants::INTEX_NFT1155_ADDRESS, word(0));
    storage.stub_sub_call_at_selector(
        crate::constants::INTEX_NFT1155_ADDRESS,
        IERC1155::balanceOfCall::SELECTOR,
        word(2),
    );
    storage.stub_sub_call_at(crate::constants::ORIGIN_ROUTER_ADDRESS, word(0));
    storage.stub_sub_call_at_selector(
        outbe_primitives::addresses::VAULT_ROUTER_ADDRESS,
        IVaultRouter::assetVaultsCountCall::SELECTOR,
        word(1),
    );
    storage.stub_sub_call_at_selector(
        settlement_asset(),
        IReferenceCurrency::isoCodeCall::SELECTOR,
        word(iso),
    );
    storage.stub_sub_call_at_selector(settlement_asset(), IERC20::decimalsCall::SELECTOR, word(18));

    StorageHandle::enter(&mut storage, |s| {
        let params = IssuanceParams {
            issuance_currency: EUR_ISO,
            ..sample(7)
        };
        runtime::issue_intex(&s, params).unwrap();
    });
    storage
}

/// Every stablecoin-backed COEN/ISO Oracle rate uses six decimals.
pub(super) const COEN_ISO_RATE_SCALE: U256 = U256::from_limbs([1_000_000, 0, 0, 0]);

#[test]
fn the_issuance_currency_settles_through_the_coen_pivot() {
    with_dual_currency_series(EUR_ISO as u64, |s| {
        let oracle = OracleContract::new(s.clone());
        // COEN buys twice as many dollars as euros, so the euro price of one
        // Intex is half its dollar price.
        write_day_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(2u64) * COEN_ISO_RATE_SCALE,
        );
        write_day_rate(&oracle, EUR_ISO, EUR_PAIR_ID, COEN_ISO_RATE_SCALE);

        let (_, cost, _) =
            runtime::quote_settlement(&s, sid(7), settlement_asset(), U256::ONE).unwrap();
        assert_eq!(cost, U256::from(500_000_000_000_000_000u64));
    });
}

#[test]
fn the_issuance_rail_converts_at_the_trailing_window_not_the_closed_day() {
    with_dual_currency_series(EUR_ISO as u64, |s| {
        let oracle = OracleContract::new(s.clone());
        write_day_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(2u64) * COEN_ISO_RATE_SCALE,
        );
        write_day_rate(&oracle, EUR_ISO, EUR_PAIR_ID, COEN_ISO_RATE_SCALE);
        let day = previous_date_key(timestamp_to_date_key(ISSUED_AT as u64));
        oracle
            .record_utc_day_vwap(day, EUR_PAIR_ID, U256::from(4u64) * COEN_ISO_RATE_SCALE)
            .unwrap();

        let (_, cost, _) =
            runtime::quote_settlement(&s, sid(7), settlement_asset(), U256::ONE).unwrap();
        assert_eq!(cost, U256::from(500_000_000_000_000_000u64));
    });
}

#[test]
fn issuance_currency_settlement_floors_a_non_divisible_fx_result_once() {
    with_dual_currency_series(EUR_ISO as u64, |s| {
        let oracle = OracleContract::new(s.clone());
        write_day_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(3u64) * COEN_ISO_RATE_SCALE,
        );
        write_day_rate(&oracle, EUR_ISO, EUR_PAIR_ID, COEN_ISO_RATE_SCALE);

        let (_, cost, _) =
            runtime::quote_settlement(&s, sid(7), settlement_asset(), U256::ONE).unwrap();
        assert_eq!(cost, U256::from(333_333_333_333_333_333u64));
    });
}

#[test]
fn an_unpriced_issuance_currency_cannot_be_settled_in() {
    with_dual_currency_series(EUR_ISO as u64, |s| {
        let oracle = OracleContract::new(s.clone());
        write_day_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(2u64) * COEN_ISO_RATE_SCALE,
        );
        // No euro pair at all.
        let err = runtime::quote_settlement(&s, sid(7), settlement_asset(), U256::ONE).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                outbe_vaultrouter::settlement::SettlementError::OracleUnavailable,
            )
            .to_string(),
            "{err}"
        );
    });
}

#[test]
fn a_rate_the_window_never_priced_cannot_be_settled_in() {
    with_dual_currency_series(EUR_ISO as u64, |s| {
        let oracle = OracleContract::new(s.clone());
        write_day_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(2u64) * COEN_ISO_RATE_SCALE,
        );
        // The euro trades live, but the window it converts at holds no euro price.
        write_rate(&oracle, EUR_ISO, EUR_PAIR_ID, COEN_ISO_RATE_SCALE);

        let err = runtime::quote_settlement(&s, sid(7), settlement_asset(), U256::ONE).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                outbe_vaultrouter::settlement::SettlementError::OracleUnavailable,
            )
            .to_string(),
            "{err}"
        );
    });
}

#[test]
fn issuance_currency_settlement_rejects_fx_overflow() {
    with_dual_currency_series(EUR_ISO as u64, |s| {
        let oracle = OracleContract::new(s.clone());
        write_day_rate(&oracle, REFERENCE_ISO, PAIR_ID, COEN_ISO_RATE_SCALE);
        write_day_rate(
            &oracle,
            EUR_ISO,
            EUR_PAIR_ID,
            U256::MAX / COEN_ISO_RATE_SCALE,
        );

        let err = runtime::quote_settlement(&s, sid(7), settlement_asset(), U256::ONE).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                outbe_vaultrouter::settlement::SettlementError::Rounding(
                    outbe_primitives::settlement::RoundingError::Overflow,
                ),
            )
            .to_string(),
            "{err}"
        );
    });
}

#[test]
fn the_reference_currency_settles_without_reading_any_rate() {
    // No rate is published at all, yet the reference currency still settles.
    with_dual_currency_series(REFERENCE_ISO as u64, |s| {
        let (_, cost, _) =
            runtime::quote_settlement(&s, sid(7), settlement_asset(), U256::ONE).unwrap();
        assert_eq!(cost, U256::from(1_000_000_000_000_000_000u64));
    });
}

#[test]
fn an_issuance_payment_must_name_the_snapshot_required_at_execution() {
    let mut storage = dual_currency_series(EUR_ISO as u64);
    stub_token_that_never_credits(&mut storage);
    let quote = |storage: &mut HashMapStorageProvider| {
        StorageHandle::enter(storage, |s| {
            runtime::quote_settlement(&s, sid(7), settlement_asset(), U256::ONE).unwrap()
        })
    };
    let settle = refused_settlement;
    StorageHandle::enter(&mut storage, |s| {
        let oracle = OracleContract::new(s.clone());
        write_day_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(2u64) * COEN_ISO_RATE_SCALE,
        );
        write_day_rate(&oracle, EUR_ISO, EUR_PAIR_ID, COEN_ISO_RATE_SCALE);
    });
    let (_, amount, quoted) = quote(&mut storage);
    let cutoff = outbe_oracle::api::VwapSnapshotId::from_u256(quoted)
        .unwrap()
        .cutoff();
    let reached_payment = outbe_primitives::error::PrecompileError::from(
        outbe_vaultrouter::settlement::SettlementError::AmountMismatch,
    )
    .to_string();

    storage.set_timestamp(U256::from(cutoff + 3_599));
    assert_eq!(settle(&mut storage, quoted), reached_payment);

    storage.set_timestamp(U256::from(cutoff + 3_600));
    let (_, next_amount, required) = quote(&mut storage);
    assert_eq!(next_amount, amount, "the next window holds the same price");
    assert_eq!(
        settle(&mut storage, quoted),
        outbe_primitives::error::PrecompileError::from(
            outbe_vaultrouter::settlement::SettlementError::SnapshotMismatch {
                authorized: quoted,
                required,
            }
        )
        .to_string()
    );
    let other_policy = outbe_oracle::api::get_vwap_snapshot_id(
        cutoff + 3_600,
        &outbe_oracle::api::VwapPolicy {
            policy_version: 2,
            ..outbe_oracle::api::DEFAULT_VWAP_POLICY
        },
    )
    .unwrap();
    assert_eq!(
        settle(&mut storage, other_policy.to_u256()),
        outbe_primitives::error::PrecompileError::from(
            outbe_vaultrouter::settlement::SettlementError::SnapshotMismatch {
                authorized: other_policy.to_u256(),
                required,
            }
        )
        .to_string()
    );
    StorageHandle::enter(&mut storage, |s| {
        assert_eq!(outbe_intex::api::settled_units(&s, sid(7)).unwrap(), 0);
    });
}

#[test]
fn the_hourly_rollover_at_the_call_deadline_grants_no_grace() {
    let mut storage = dual_currency_series(EUR_ISO as u64);
    stub_token_that_never_credits(&mut storage);
    let quote = |storage: &mut HashMapStorageProvider| {
        StorageHandle::enter(storage, |s| {
            runtime::quote_settlement(&s, sid(7), settlement_asset(), U256::ONE)
                .unwrap()
                .2
        })
    };
    let settle = refused_settlement;
    StorageHandle::enter(&mut storage, |s| {
        let oracle = OracleContract::new(s.clone());
        write_day_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(2u64) * COEN_ISO_RATE_SCALE,
        );
        write_day_rate(&oracle, EUR_ISO, EUR_PAIR_ID, COEN_ISO_RATE_SCALE);
    });
    let quoted = quote(&mut storage);
    let deadline = outbe_oracle::api::VwapSnapshotId::from_u256(quoted)
        .unwrap()
        .cutoff()
        + 3_600;
    StorageHandle::enter(&mut storage, |s| {
        let notice = outbe_intex::api::read_series(&s, sid(7))
            .unwrap()
            .call_notice_period_seconds;
        outbe_intex::api::call_group_of(&s, sid(7), (deadline - u64::from(notice)) as u32).unwrap();
    });

    storage.set_timestamp(U256::from(deadline));
    let required = quote(&mut storage);
    assert_eq!(
        settle(&mut storage, quoted),
        outbe_primitives::error::PrecompileError::from(
            outbe_vaultrouter::settlement::SettlementError::SnapshotMismatch {
                authorized: quoted,
                required,
            }
        )
        .to_string()
    );

    storage.set_timestamp(U256::from(deadline + 1));
    assert_eq!(
        settle(&mut storage, required),
        outbe_primitives::error::PrecompileError::from(
            crate::errors::IntexFactoryError::SettlementDeadlinePassed
        )
        .to_string()
    );
}

#[test]
fn a_reference_payment_ignores_the_snapshot_it_names() {
    let mut storage = dual_currency_series(REFERENCE_ISO as u64);
    stub_token_that_never_credits(&mut storage);
    StorageHandle::enter(&mut storage, |s| {
        let err = runtime::settle_intex(
            &s,
            sid(7),
            owner(),
            U256::ONE,
            SettlementPayment {
                payer: owner(),
                asset: settlement_asset(),
                snapshot_id: U256::from(7u64),
            },
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                outbe_vaultrouter::settlement::SettlementError::AmountMismatch
            )
            .to_string()
        );
    });
}

fn refused_settlement(storage: &mut HashMapStorageProvider, snapshot: U256) -> String {
    StorageHandle::enter(storage, |s| {
        runtime::settle_intex(
            &s,
            sid(7),
            owner(),
            U256::ONE,
            SettlementPayment {
                payer: owner(),
                asset: settlement_asset(),
                snapshot_id: snapshot,
            },
        )
        .unwrap_err()
        .to_string()
    })
}
