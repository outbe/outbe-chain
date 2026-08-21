use alloy_primitives::{address, keccak256, Address, U256};
use alloy_sol_types::SolCall;
use outbe_common::WorldwideDay;
use outbe_intex::SeriesId;
use outbe_oracle::api::AddressPair;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::addresses::INTEX_FACTORY_ADDRESS;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::math::constants::REAL_ID_SHIFT;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::{date_key_to_utc_timestamp, previous_date_key, timestamp_to_date_key};

use crate::called;
use crate::constants::{
    CALL_RATE, CALL_THRESHOLD, CALL_WINDOW, FLOOR_RATE, MAX_RECIPIENTS_PER_ISSUANCE,
    MAX_SERIES_PER_MESSAGE, QUALIFICATION_PERIOD,
};
use crate::precompile::{self, IIntexFactory};
use crate::qualified;
use crate::runtime;
use crate::schema::{IntexFactoryContract, IssuanceParams};
use crate::state::Group;

/// ISO code every fixture prices in.
const REFERENCE_ISO: u16 = 840;
const DAY: u64 = 24 * 60 * 60;

fn holder() -> Address {
    address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
}

fn payment_token() -> Address {
    address!("0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC")
}

const CHAIN_ID: u64 = 1;
const ISSUED_AT: u32 = 1_700_000_000;
const PROMIS_LOAD_MINOR: u128 = 1_000_000; // 1 PROMIS in PROMIS-unit
const CALL_NOTICE_PERIOD: u32 = 7 * 24 * 60 * 60;

// COEN clearing price and the floor/trigger derived from it at issuance.
const ENTRY_PRICE: u64 = 1_000_000;
const EXPECTED_FLOOR: u64 = 1_080_000; // ENTRY_PRICE * 108/100
const EXPECTED_TRIGGER: u64 = 2_280_000; // ENTRY_PRICE * 228/100

fn with_factory<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    // Stub IntexNFT1155: void calls succeed; balanceOf returns 0 (32 bytes).
    storage.stub_sub_call_at(
        crate::constants::INTEX_NFT1155_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );
    // Stub OriginRouter: send* calls return bytes32 sendId (32 bytes); the value is ignored.
    storage.stub_sub_call_at(
        crate::constants::ORIGIN_ROUTER_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );
    StorageHandle::enter(&mut storage, f)
}

/// Test ids carry a fixed USD/U pair; only the day varies.
fn sid(worldwide_day: u32) -> SeriesId {
    SeriesId::pack(WorldwideDay::new(worldwide_day), *b"USD", b'U').unwrap()
}

/// Force-call one group against the protocol call window; how many series moved.
fn call_group(
    s: &StorageHandle<'_>,
    f: &mut IntexFactoryContract,
    oracle: &OracleContract,
    pair: AddressPair,
    group: &Group,
    last_closed_day: u32,
    now_ts: u64,
) -> u32 {
    let mut vwaps = called::DayVwaps::new(oracle.pair_index_of(pair).unwrap());
    let secs_per_day = DAY as u32;
    let Some(window) = called::call_window(
        oracle,
        &mut vwaps,
        last_closed_day,
        CALL_WINDOW / secs_per_day,
        CALL_THRESHOLD / secs_per_day,
    )
    .unwrap() else {
        return 0;
    };
    called::try_call_group(s, f, oracle, &mut vwaps, group, &window, now_ts).unwrap()
}

/// Qualify one day's group in the reference currency; returns how many series moved.
fn qualify_day(
    s: &StorageHandle<'_>,
    f: &mut IntexFactoryContract,
    worldwide_day: u32,
    qualification_period: u32,
    now: u64,
    rate: U256,
) -> u32 {
    let group = f
        .unqualified_group(REFERENCE_ISO, WorldwideDay::new(worldwide_day))
        .unwrap();
    qualified::try_qualify_group(s, f, &group, qualification_period, now, rate).unwrap()
}

fn sample(worldwide_day: u32) -> IssuanceParams {
    IssuanceParams {
        series_id: sid(worldwide_day),
        worldwide_day: worldwide_day.into(),
        issued_intex_count: 100,
        promis_load_minor: PROMIS_LOAD_MINOR,
        entry_price_minor: U256::from(ENTRY_PRICE),
        issuance_currency: 840,
        reference_currency: 840,
        recipients: vec![],
        quantities: vec![],
        recipient_chains: vec![],
        // One target in the snapshot exercises the per-chain ISSUANCE loop (empty recipients).
        snapshot_chains: vec![1],
    }
}

#[test]
fn issue_creates_series_in_registry() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();

        // The series is captured in Intex with the issuance identity.
        let r = outbe_intex::api::read_series(&s, sid(7)).unwrap();
        assert_eq!(r.series_id, sid(7));
        assert_eq!(r.promis_load_minor, U256::from(PROMIS_LOAD_MINOR));
        assert_eq!(r.entry_price_minor, U256::from(ENTRY_PRICE));
        // Floor and trigger are derived from the clearing price at issuance.
        assert_eq!(r.floor_price_minor, U256::from(EXPECTED_FLOOR));
        assert_eq!(r.issued_intex_count, 100);
        assert_eq!(r.call_notice_period, CALL_NOTICE_PERIOD);
        // Window/threshold/call-period are IntexFactory protocol constants now.
        assert_eq!(r.call_price_minor, U256::from(EXPECTED_TRIGGER));
        assert_eq!(
            r.call_trigger(),
            outbe_intex::IntexCallTrigger {
                call_window: 28 * DAY as u32,
                call_threshold: 21 * DAY as u32,
                call_notice_period: CALL_NOTICE_PERIOD,
            }
        );
        // Born Issued; issued_at is the block timestamp.
        assert_eq!(
            r.lifecycle_state().unwrap(),
            outbe_intex::IntexState::Issued
        );
        assert_eq!(r.issued_at, ISSUED_AT);
        assert_eq!(r.called_at, 0);
    });
}

#[test]
fn issue_rejects_duplicate_series() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        // The registry record-create rejects a duplicate series id.
        assert!(runtime::issue(&s, sample(7)).is_err());
    });
}

#[test]
fn issue_zero_winners_leaves_the_day_untouched() {
    with_factory(|s| {
        // Lysis recorded contributors for the day, but this group had no winners.
        outbe_intex::api::record_contributors(
            &s,
            WorldwideDay::new(7),
            &[(holder(), U256::from(100u64))],
        )
        .unwrap();
        let mut p = sample(7);
        p.issued_intex_count = 0;
        runtime::issue(&s, p).unwrap();

        // No series is created, and the day's map is left for its caller to
        // decide on: sibling groups of the same day may still distribute.
        assert!(!outbe_intex::api::series_exists(&s, sid(7)).unwrap());
        assert_eq!(
            outbe_intex::api::contributor_count(&s, WorldwideDay::new(7)).unwrap(),
            1
        );
    });
}

#[test]
fn issuance_legs_route_winners_to_their_own_chain() {
    // One winner on chain 10, one on chain 20; chain 30 in the snapshot has none.
    let other = address!("0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC");
    let mut p = sample(7);
    p.recipients = vec![holder(), other];
    p.quantities = vec![U256::from(1), U256::from(2)];
    p.recipient_chains = vec![10, 20];
    p.snapshot_chains = vec![10, 20, 30];

    let legs = runtime::issuance_legs(&p);
    assert_eq!(legs.len(), 3);
    assert_eq!(legs[0], (10, vec![holder()], vec![U256::from(1)]));
    assert_eq!(legs[1], (20, vec![other], vec![U256::from(2)]));
    assert_eq!(legs[2], (30, vec![], vec![])); // create-only leg
}

#[test]
fn issue_enrolls_series_in_dense_enumeration() {
    with_factory(|s| {
        runtime::issue(&s, sample(11)).unwrap();
        runtime::issue(&s, sample(22)).unwrap();
        assert_eq!(outbe_intex::api::total_series(&s).unwrap(), 2);
        assert_eq!(outbe_intex::api::series_id_at(&s, 0).unwrap(), sid(11));
        assert_eq!(outbe_intex::api::series_id_at(&s, 1).unwrap(), sid(22));
    });
}

#[test]
fn floor_and_call_derivation() {
    let floor = runtime::marked_up(U256::from(ENTRY_PRICE), FLOOR_RATE).unwrap();
    let call = runtime::marked_up(U256::from(ENTRY_PRICE), CALL_RATE).unwrap();
    assert_eq!(floor, U256::from(EXPECTED_FLOOR));
    assert_eq!(call, U256::from(EXPECTED_TRIGGER));

    let one = U256::from(1_000_000u64);
    assert_eq!(
        runtime::marked_up(one, FLOOR_RATE).unwrap(),
        U256::from(1_080_000u64)
    );
    assert_eq!(
        runtime::marked_up(one, CALL_RATE).unwrap(),
        U256::from(2_280_000u64)
    );
}

#[test]
fn coen_iso_one_maps_to_the_center_price_bin_at_six_decimals() {
    assert_eq!(
        IntexFactoryContract::price_to_bin(U256::from(1_000_000u64)).unwrap(),
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

// ---------------------------------------------------------------------
// settlement cost scaling
// ---------------------------------------------------------------------

fn entry_price() -> U256 {
    U256::from(500_000u64)
}

fn load_minor() -> U256 {
    U256::from(100_000u64) * U256::from(1_000_000u64)
}

#[test]
fn cost_amount_six_decimals() {
    let cost = runtime::derived_cost_amount(entry_price(), load_minor(), 6).unwrap();
    assert_eq!(cost, U256::from(50_000_000_000u64));
}

#[test]
fn cost_amount_eighteen_decimals_is_1e12_larger() {
    let six = runtime::derived_cost_amount(entry_price(), load_minor(), 6).unwrap();
    let eighteen = runtime::derived_cost_amount(entry_price(), load_minor(), 18).unwrap();
    assert_eq!(eighteen, six * U256::from(10u64).pow(U256::from(12u64)));
}

#[test]
fn cost_amount_zero_decimals() {
    let cost = runtime::derived_cost_amount(entry_price(), load_minor(), 0).unwrap();
    assert_eq!(cost, U256::from(50_000u64));
}

#[test]
fn cost_amount_twelve_decimals() {
    let cost = runtime::derived_cost_amount(entry_price(), load_minor(), 12).unwrap();
    assert_eq!(cost, U256::from(50_000_000_000_000_000u64));
}

#[test]
fn cost_amount_rounds_positive_subunit_payment_up_to_one() {
    let cost = runtime::derived_cost_amount(U256::ONE, U256::ONE, 0).unwrap();
    assert_eq!(cost, U256::ONE);
}

#[test]
fn cost_amount_rejects_unsupported_payment_decimals() {
    let err = runtime::derived_cost_amount(entry_price(), load_minor(), 19).unwrap_err();
    assert!(err.to_string().contains("unsupported decimals"), "{err}");
}

#[test]
fn cost_amount_rejects_product_overflow() {
    let err = runtime::derived_cost_amount(U256::MAX, U256::from(2u64), 6).unwrap_err();
    assert!(err.to_string().to_lowercase().contains("overflow"), "{err}");
}

fn word(value: u64) -> alloy_primitives::Bytes {
    alloy_primitives::Bytes::from(U256::from(value).to_be_bytes::<32>().to_vec())
}

/// Storage with an issued series 7 and a payment token the router reports
/// `vaults` vaults for, reporting `iso` and `decimals`.
fn with_payment_token<R>(
    vaults: u64,
    iso: u64,
    decimals: u64,
    f: impl FnOnce(StorageHandle) -> R,
) -> R {
    use crate::sol_ext::{IReferenceCurrency, IERC20};
    use outbe_vaultrouter::api::IVaultRouter;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    storage.stub_sub_call_at(crate::constants::INTEX_NFT1155_ADDRESS, word(0));
    storage.stub_sub_call_at(crate::constants::ORIGIN_ROUTER_ADDRESS, word(0));
    storage.stub_sub_call_at_selector(
        outbe_primitives::addresses::VAULT_ROUTER_ADDRESS,
        IVaultRouter::assetVaultsCountCall::SELECTOR,
        word(vaults),
    );
    storage.stub_sub_call_at_selector(
        payment_token(),
        IReferenceCurrency::isoCodeCall::SELECTOR,
        word(iso),
    );
    storage.stub_sub_call_at_selector(
        payment_token(),
        IERC20::decimalsCall::SELECTOR,
        word(decimals),
    );

    StorageHandle::enter(&mut storage, |s| {
        runtime::issue(&s, sample(7)).unwrap();
        f(s)
    })
}

#[test]
fn cost_amount_prices_an_accepted_token() {
    for (decimals, expected) in [
        (0, U256::ONE),
        (6, U256::from(1_000_000u64)),
        (12, U256::from(1_000_000_000_000u64)),
        (18, U256::from(1_000_000_000_000_000_000u64)),
    ] {
        with_payment_token(1, 840, decimals, |s| {
            let cost = runtime::quote_cost_amount(&s, sid(7), payment_token()).unwrap();
            assert_eq!(cost, expected, "payment token decimals {decimals}");
        });
    }
}

#[test]
fn cost_amount_rejects_an_unregistered_token() {
    with_payment_token(0, 840, 18, |s| {
        let err = runtime::quote_cost_amount(&s, sid(7), payment_token()).unwrap_err();
        assert!(err.to_string().contains("no registered vault"), "{err}");
    });
}

#[test]
fn cost_amount_rejects_a_foreign_currency() {
    with_payment_token(1, 978, 18, |s| {
        let err = runtime::quote_cost_amount(&s, sid(7), payment_token()).unwrap_err();
        assert!(err.to_string().contains("does not match"), "{err}");
    });
}

#[test]
fn cost_amount_rejects_missing_series() {
    with_factory(|s| {
        assert!(runtime::quote_cost_amount(&s, sid(7), payment_token()).is_err());
    });
}

#[test]
fn cost_amount_dispatch() {
    with_payment_token(1, 840, 18, |s| {
        let out = precompile::dispatch(
            s.clone(),
            &IIntexFactory::quoteCostAmountCall {
                seriesId: sid(7).into(),
                paymentToken: payment_token(),
            }
            .abi_encode(),
            holder(),
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            IIntexFactory::quoteCostAmountCall::abi_decode_returns(&out).unwrap(),
            U256::from(1_000_000_000_000_000_000u64)
        );
    });
}

// ---------------------------------------------------------------------
// settle gating (value movement is localnet-exercised, not unit tested)
// ---------------------------------------------------------------------

#[test]
fn settle_rejects_zero_amount() {
    with_factory(|s| {
        assert!(
            runtime::settle(&s, sid(7), holder(), holder(), U256::ZERO, payment_token()).is_err()
        );
    });
}

#[test]
fn settle_rejects_missing_series() {
    with_factory(|s| {
        assert!(runtime::settle(
            &s,
            sid(7),
            holder(),
            holder(),
            U256::from(1),
            payment_token()
        )
        .is_err());
    });
}

#[test]
fn settle_rejects_wrong_state_issued() {
    with_factory(|s| {
        // Born Issued; settlement is only valid in Qualified/Called.
        runtime::issue(&s, sample(7)).unwrap();
        let err = runtime::settle(
            &s,
            sid(7),
            holder(),
            holder(),
            U256::from(1),
            payment_token(),
        )
        .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("settleable"));
    });
}

#[test]
fn settle_rejects_expired_deadline() {
    // Late block timestamp so the Called deadline is already in the past.
    let now = (ISSUED_AT as u64) + (CALL_NOTICE_PERIOD as u64) + 1_000;
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(now));
    storage.stub_sub_call_at(
        crate::constants::INTEX_NFT1155_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );
    // Stub OriginRouter: send* calls return bytes32 sendId (32 bytes); the value is ignored.
    storage.stub_sub_call_at(
        crate::constants::ORIGIN_ROUTER_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );
    StorageHandle::enter(&mut storage, |s| {
        runtime::issue(&s, sample(7)).unwrap();
        // deadline = ISSUED_AT + CALL_NOTICE_PERIOD < now
        outbe_intex::api::mark_called(&s, sid(7), ISSUED_AT).unwrap();
        let err = runtime::settle(
            &s,
            sid(7),
            holder(),
            holder(),
            U256::from(1),
            payment_token(),
        )
        .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("deadline"));
    });
}

#[test]
fn set_authorized_settler_round_trip() {
    with_factory(|s| {
        let settler = address!("0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB");
        runtime::set_authorized_settler(&s, holder(), sid(7), settler).unwrap();
        let f = IntexFactoryContract::new(s.clone());
        assert_eq!(
            f.read_authorized_settler(holder(), sid(7)).unwrap(),
            settler
        );
    });
}

// ---------------------------------------------------------------------
// minePromis PoW + derivations (value movement is localnet-exercised)
// ---------------------------------------------------------------------

#[test]
fn settled_token_id_derivation() {
    // uint256(keccak256("SETTLED" ++ seriesId_be64))
    let series_id = sid(7);
    let mut buf = Vec::new();
    buf.extend_from_slice(b"SETTLED");
    buf.extend_from_slice(series_id.as_bytes());
    assert_eq!(
        runtime::settled_token_id(series_id),
        U256::from_be_bytes(keccak256(&buf).0)
    );
}

#[test]
fn compute_pow_hash_matches_manual_sha256() {
    // SHA256(hex(holder)++hex(promisAmount)++hex(seriesId)++hex(seq) ++ nonce_be8)
    let promis_amount = U256::from(1_000u64);
    let (series_id, seq, nonce) = (sid(7), 3u32, 42u64);
    let got = runtime::compute_pow_hash(holder(), promis_amount, series_id, seq, U256::from(nonce))
        .unwrap();

    let mut preimage = String::new();
    preimage.push_str(&hex::encode(holder().as_slice()));
    preimage.push_str(&hex::encode(promis_amount.to_be_bytes::<32>()));
    preimage.push_str(&hex::encode(series_id.as_bytes()));
    preimage.push_str(&hex::encode(seq.to_be_bytes()));
    let mut data = preimage.into_bytes();
    data.extend_from_slice(&nonce.to_be_bytes());
    let expected = ring::digest::digest(&ring::digest::SHA256, &data);
    assert_eq!(got.as_slice(), expected.as_ref());
}

#[test]
fn validate_pow_accepts_valid_and_rejects_invalid_nonce() {
    let pa = U256::from(1_000u64);
    let (series_id, seq) = (sid(7), 0u32);
    // Difficulty 1: ~1/256 of nonces pass; brute-force a valid and an invalid one.
    let mut good = None;
    let mut bad = None;
    for n in 0u64..100_000 {
        let ok = runtime::validate_pow(holder(), pa, series_id, seq, U256::from(n)).is_ok();
        if ok && good.is_none() {
            good = Some(n);
        }
        if !ok && bad.is_none() {
            bad = Some(n);
        }
        if good.is_some() && bad.is_some() {
            break;
        }
    }
    assert!(runtime::validate_pow(
        holder(),
        pa,
        series_id,
        seq,
        U256::from(good.expect("a valid nonce"))
    )
    .is_ok());
    assert!(runtime::validate_pow(
        holder(),
        pa,
        series_id,
        seq,
        U256::from(bad.expect("an invalid nonce"))
    )
    .is_err());
}

#[test]
fn validate_pow_rejects_nonce_over_u64() {
    assert!(runtime::validate_pow(
        holder(),
        U256::from(1u64),
        sid(1),
        0,
        U256::from(u64::MAX) + U256::from(1)
    )
    .is_err());
}

/// A dummy authorization for mine_promis paths that reject before the (enclave)
/// Promis mint (zero amount / missing series).
fn no_auth() -> outbe_promisfactory::api::ModifyAuth {
    outbe_promisfactory::api::ModifyAuth {
        mac: [0u8; 32],
        op_nonce: 0,
    }
}

#[test]
fn mine_promis_rejects_zero_amount() {
    with_factory(|s| {
        assert!(
            runtime::mine_promis(&s, sid(7), holder(), U256::ZERO, U256::ZERO, no_auth()).is_err()
        );
    });
}

#[test]
fn mine_promis_rejects_missing_series() {
    with_factory(|s| {
        assert!(
            runtime::mine_promis(&s, sid(7), holder(), U256::from(1), U256::ZERO, no_auth())
                .is_err()
        );
    });
}

// ---------------------------------------------------------------------
// Qualified: floor-bin index + per-series qualify gates
// (full begin_block scan reads the oracle -> localnet)
// ---------------------------------------------------------------------

#[test]
fn issue_enrolls_in_floor_bin() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        let f = IntexFactoryContract::new(s.clone());
        let bin = IntexFactoryContract::price_to_bin(U256::from(EXPECTED_FLOOR)).unwrap();
        assert_eq!(
            f.unqualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            1
        );
    });
}

#[test]
fn insert_remove_unqualified_roundtrip() {
    with_factory(|s| {
        let mut f = IntexFactoryContract::new(s.clone());
        let floor = U256::from(2_000u64);
        let bin = IntexFactoryContract::price_to_bin(floor).unwrap();
        f.insert_unqualified(sid(11), REFERENCE_ISO, floor).unwrap();
        f.insert_unqualified(sid(22), REFERENCE_ISO, floor).unwrap();
        assert_eq!(
            f.unqualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            2
        );
        f.remove_unqualified_group(REFERENCE_ISO, WorldwideDay::new(11))
            .unwrap();
        assert_eq!(
            f.unqualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            1
        );
        f.remove_unqualified_group(REFERENCE_ISO, WorldwideDay::new(22))
            .unwrap();
        assert_eq!(
            f.unqualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            0
        );
    });
}

#[test]
fn try_qualify_gates_qualification_floor_and_latches() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        let mut f = IntexFactoryContract::new(s.clone());
        let floor = U256::from(EXPECTED_FLOOR);
        let immature = ISSUED_AT as u64 + 10;
        let mature = ISSUED_AT as u64 + 21 * DAY + 1;

        // Immature -> false.
        assert_eq!(
            qualify_day(
                &s,
                &mut f,
                7,
                QUALIFICATION_PERIOD,
                immature,
                floor + U256::from(1)
            ),
            0
        );
        // Mature but rate == floor (strict >) -> false.
        assert_eq!(
            qualify_day(&s, &mut f, 7, QUALIFICATION_PERIOD, mature, floor),
            0
        );
        // Mature + rate > floor -> qualifies, latched, removed from bin.
        assert_eq!(
            qualify_day(
                &s,
                &mut f,
                7,
                QUALIFICATION_PERIOD,
                mature,
                floor + U256::from(1)
            ),
            1
        );
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(7))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Qualified
        );
        let bin = IntexFactoryContract::price_to_bin(floor).unwrap();
        assert_eq!(
            f.unqualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            0
        );
        // Already Qualified -> false.
        assert_eq!(
            qualify_day(
                &s,
                &mut f,
                7,
                QUALIFICATION_PERIOD,
                mature,
                floor + U256::from(1)
            ),
            0
        );
    });
}

// ---------------------------------------------------------------------
// precompile dispatch (decode -> runtime -> encode)
// ---------------------------------------------------------------------

#[test]
fn dispatch_set_authorized_settler_round_trip() {
    with_factory(|s| {
        let settler = address!("0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB");
        let data = IIntexFactory::setAuthorizedSettlerCall {
            seriesId: sid(7).into(),
            settler,
        }
        .abi_encode();
        // Caller (holder) is taken from msg.sender, not the calldata.
        precompile::dispatch(s.clone(), &data, holder(), U256::ZERO).unwrap();
        let f = IntexFactoryContract::new(s.clone());
        assert_eq!(
            f.read_authorized_settler(holder(), sid(7)).unwrap(),
            settler
        );
    });
}

#[test]
fn dispatch_rejects_value() {
    with_factory(|s| {
        let settler = address!("0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB");
        let data = IIntexFactory::setAuthorizedSettlerCall {
            seriesId: sid(7).into(),
            settler,
        }
        .abi_encode();
        assert!(precompile::dispatch(s.clone(), &data, holder(), U256::from(1)).is_err());
    });
}

#[test]
fn dispatch_mine_promis_routes_to_runtime() {
    with_factory(|s| {
        // Missing series -> the runtime error surfaces through dispatch.
        let data = IIntexFactory::minePromisCall {
            seriesId: sid(7).into(),
            amount: U256::from(1),
            nonce: U256::ZERO,
            mac: alloy_primitives::FixedBytes([0u8; 32]),
            opNonce: 0,
        }
        .abi_encode();
        assert!(precompile::dispatch(s.clone(), &data, holder(), U256::ZERO).is_err());
    });
}

// ---------------------------------------------------------------------
// Called: call-trigger bin index + daily windowed-breach scan
// ---------------------------------------------------------------------

fn qualify_series<'a>(
    s: &StorageHandle<'a>,
    id: u32,
    params: IssuanceParams,
) -> IntexFactoryContract<'a> {
    runtime::issue(s, params).unwrap();
    let mut f = IntexFactoryContract::new(s.clone());
    let mature = ISSUED_AT as u64 + 21 * DAY + 1;
    let floor = U256::from(EXPECTED_FLOOR);
    assert!(
        qualify_day(
            s,
            &mut f,
            id,
            QUALIFICATION_PERIOD,
            mature,
            floor + U256::from(1)
        ) == 1
    );
    f
}

/// Registry index the fixtures register the qualifier pair at; the rate
/// columns are keyed by it.
const PAIR_ID: u32 = 1;

/// List the qualifier currency in the oracle's reference registry, which the
/// scans walk to decide which currencies to price this block.
fn list_reference(oracle: &OracleContract) {
    if oracle.reference_currencies.len().unwrap() == 0 {
        oracle.reference_currencies.push(REFERENCE_ISO).unwrap();
    }
}

fn setup_pair(oracle: &OracleContract) -> AddressPair {
    let pair = outbe_oracle::api::AddressPair::new_coen_to(REFERENCE_ISO);
    let pair_id = PAIR_ID;
    oracle.pair_to_index.write(&pair, pair_id).unwrap();
    // Full registry entry so the production VWAP paths (calculate_vwaps
    // iterating registered vote-target pairs) see the pair too. `pair_at` reads
    // the reverse column.
    oracle.pair_count.write(pair_id).unwrap();
    oracle.pair_by_index.write_pair(&pair_id, pair).unwrap();
    oracle.vote_target.write(&pair, true).unwrap();
    list_reference(oracle);
    pair
}

fn set_vwap(oracle: &OracleContract, utc_day: u32, pair: AddressPair, value: U256) {
    // The value column is keyed by the pair's registry index.
    let pair_id = oracle.pair_index_of(pair).unwrap();
    oracle
        .utc_day_vwap_value
        .get_nested(&utc_day)
        .write(&pair_id, value)
        .unwrap();
    // Mirror the begin-block hook: the watermark covers every seeded day.
    if oracle.utc_day_vwap_last_finalized.read().unwrap() < utc_day {
        oracle.utc_day_vwap_last_finalized.write(utc_day).unwrap();
    }
}

/// Set `days` consecutive closed UTC days ending at `latest` to `value`.
fn fill_days(oracle: &OracleContract, latest: u32, pair: AddressPair, days: u32, value: U256) {
    let mut d = latest;
    for _ in 0..days {
        set_vwap(oracle, d, pair, value);
        d = previous_date_key(d);
    }
}

#[test]
fn qualify_enrolls_in_call_trigger_bin() {
    with_factory(|s| {
        let f = qualify_series(&s, 7, sample(7));
        // Moved out of the floor index, into the call-trigger index.
        let floor_bin = IntexFactoryContract::price_to_bin(U256::from(EXPECTED_FLOOR)).unwrap();
        let trig_bin = IntexFactoryContract::price_to_bin(U256::from(EXPECTED_TRIGGER)).unwrap();
        assert_eq!(
            f.unqualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, floor_bin))
                .unwrap(),
            0
        );
        assert_eq!(
            f.qualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, trig_bin))
                .unwrap(),
            1
        );
    });
}

#[test]
fn insert_remove_qualified_roundtrip() {
    with_factory(|s| {
        let mut f = IntexFactoryContract::new(s.clone());
        let trigger = U256::from(EXPECTED_TRIGGER);
        let bin = IntexFactoryContract::price_to_bin(trigger).unwrap();
        f.insert_qualified_group(REFERENCE_ISO, WorldwideDay::new(11), trigger, &[sid(11)])
            .unwrap();
        f.insert_qualified_group(REFERENCE_ISO, WorldwideDay::new(22), trigger, &[sid(22)])
            .unwrap();
        assert_eq!(
            f.qualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            2
        );
        f.remove_qualified_group(REFERENCE_ISO, WorldwideDay::new(11))
            .unwrap();
        assert_eq!(
            f.qualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            1
        );
        f.remove_qualified_group(REFERENCE_ISO, WorldwideDay::new(22))
            .unwrap();
        assert_eq!(
            f.qualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            0
        );
    });
}

#[test]
fn try_call_marks_called_when_threshold_met() {
    with_factory(|s| {
        let mut f = qualify_series(&s, 7, sample(7));
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        // All 30 window days above the trigger (threshold is 21).
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts));
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        fill_days(&oracle, last_closed_day, pair, 30, breach);

        let group = f
            .qualified_group(REFERENCE_ISO, WorldwideDay::new(7))
            .unwrap();
        assert_eq!(
            call_group(&s, &mut f, &oracle, pair, &group, last_closed_day, scan_ts),
            1
        );
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(7))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Called
        );
        let bin = IntexFactoryContract::price_to_bin(U256::from(EXPECTED_TRIGGER)).unwrap();
        assert_eq!(
            f.qualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            0
        );
    });
}

#[test]
fn try_call_skips_when_below_threshold() {
    with_factory(|s| {
        let mut f = qualify_series(&s, 7, sample(7));
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts));
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        let calm = U256::from(EXPECTED_TRIGGER); // equal: strict `>` is not a breach
                                                 // 20 breach days + 10 calm days; threshold is 21.
        let mut d = last_closed_day;
        for _ in 0..20 {
            set_vwap(&oracle, d, pair, breach);
            d = previous_date_key(d);
        }
        for _ in 0..10 {
            set_vwap(&oracle, d, pair, calm);
            d = previous_date_key(d);
        }

        let group = f
            .qualified_group(REFERENCE_ISO, WorldwideDay::new(7))
            .unwrap();
        assert_eq!(
            call_group(&s, &mut f, &oracle, pair, &group, last_closed_day, scan_ts),
            0
        );
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(7))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Qualified
        );
    });
}

#[test]
fn try_call_excludes_pre_issuance_days() {
    with_factory(|s| {
        // window 30, threshold 27: only days from issuance onward may count.
        // Seed the series directly with threshold 27 (above the 21d qualification
        // period), since the protocol default (21) does not exceed it and a
        // qualified series would always have >= 21 completed post-issuance days.
        outbe_intex::api::create_series(
            &s,
            outbe_intex::CreateSeriesParams {
                series_id: sid(8),
                worldwide_day: 8.into(),
                issued_intex_count: 100,
                promis_load_minor: PROMIS_LOAD_MINOR,
                entry_price_minor: U256::from(ENTRY_PRICE),
                floor_price_minor: U256::from(EXPECTED_FLOOR),
                call_price_minor: U256::from(EXPECTED_TRIGGER),
                call_trigger: outbe_intex::IntexCallTrigger {
                    call_window: 30 * DAY as u32,
                    call_threshold: 27 * DAY as u32,
                    call_notice_period: CALL_NOTICE_PERIOD,
                },
                issued_at: ISSUED_AT,
                issuance_currency: 840,
                reference_currency: 840,
            },
        )
        .unwrap();
        outbe_intex::api::mark_qualified(&s, sid(8)).unwrap();
        let mut f = IntexFactoryContract::new(s.clone());
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        // Scan only ~23 days after issuance, but set all 30 window days as
        // breaches: the ~7 pre-issuance days must not count, so 23 < 27.
        let scan_ts = ISSUED_AT as u64 + 23 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts));
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        fill_days(&oracle, last_closed_day, pair, 30, breach);

        let group = Group {
            iso_code: REFERENCE_ISO,
            worldwide_day: WorldwideDay::new(8),
            members: vec![sid(8)],
        };
        assert_eq!(
            call_group(&s, &mut f, &oracle, pair, &group, last_closed_day, scan_ts),
            0
        );
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(8))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Qualified
        );
    });
}

// ---------------------------------------------------------------------
// Cross-chain notification failure: state transition must survive router outage
// ---------------------------------------------------------------------

/// Seed a series directly in the registry + bin index, bypassing issue()
/// so tests can omit the OriginRouter stub.
fn seed_issued(s: &StorageHandle<'_>, id: u32) {
    outbe_intex::api::create_series(
        s,
        outbe_intex::CreateSeriesParams {
            series_id: sid(id),
            worldwide_day: id.into(),
            issued_intex_count: 100,
            promis_load_minor: PROMIS_LOAD_MINOR,
            entry_price_minor: U256::from(ENTRY_PRICE),
            floor_price_minor: U256::from(EXPECTED_FLOOR),
            call_price_minor: U256::from(EXPECTED_TRIGGER),
            call_trigger: outbe_intex::IntexCallTrigger {
                call_window: 30 * DAY as u32,
                call_threshold: 21 * DAY as u32,
                call_notice_period: CALL_NOTICE_PERIOD,
            },
            issued_at: ISSUED_AT,
            issuance_currency: 840,
            reference_currency: 840,
        },
    )
    .unwrap();
    IntexFactoryContract::new(s.clone())
        .insert_unqualified(sid(id), REFERENCE_ISO, U256::from(EXPECTED_FLOOR))
        .unwrap();
}

#[test]
fn qualify_survives_router_failure() {
    // No OriginRouter stub: notify_qualified fails silently.
    // The Issued -> Qualified transition must still complete.
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    storage.stub_sub_call_at(
        crate::constants::INTEX_NFT1155_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );
    StorageHandle::enter(&mut storage, |s| {
        seed_issued(&s, 7);
        let mut f = IntexFactoryContract::new(s.clone());
        let mature = ISSUED_AT as u64 + 21 * DAY + 1;
        assert_eq!(
            qualify_day(
                &s,
                &mut f,
                7,
                QUALIFICATION_PERIOD,
                mature,
                U256::from(EXPECTED_FLOOR) + U256::from(1)
            ),
            1
        );
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(7))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Qualified
        );
    });
}

#[test]
fn call_survives_router_failure() {
    // No OriginRouter stub: notify_called fails silently.
    // The Qualified -> Called transition must still complete.
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    storage.stub_sub_call_at(
        crate::constants::INTEX_NFT1155_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );
    StorageHandle::enter(&mut storage, |s| {
        seed_issued(&s, 7);
        outbe_intex::api::mark_qualified(&s, sid(7)).unwrap();
        let mut f = IntexFactoryContract::new(s.clone());
        f.insert_qualified_group(
            REFERENCE_ISO,
            WorldwideDay::new(7),
            U256::from(EXPECTED_TRIGGER),
            &[sid(7)],
        )
        .unwrap();

        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts));
        fill_days(
            &oracle,
            last_closed_day,
            pair,
            30,
            U256::from(EXPECTED_TRIGGER) + U256::from(1),
        );

        let group = f
            .qualified_group(REFERENCE_ISO, WorldwideDay::new(7))
            .unwrap();
        assert_eq!(
            call_group(&s, &mut f, &oracle, pair, &group, last_closed_day, scan_ts),
            1
        );
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(7))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Called
        );
    });
}

// ---------------------------------------------------------------------
// full begin_block / daily scans (oracle read + bin iteration)
// ---------------------------------------------------------------------

#[test]
fn scan_and_qualify_promotes_aged_series() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        // Qualifier pair live rate above the floor.
        let oracle = OracleContract::new(s.clone());
        let pair = outbe_oracle::api::AddressPair::new_coen_to(REFERENCE_ISO);
        // The ISO resolves through the pair registry, so the pair must exist
        // and the rate columns are keyed by its index.
        oracle.pair_to_index.write(&pair, PAIR_ID).unwrap();
        oracle
            .exchange_rate
            .write(&PAIR_ID, U256::from(EXPECTED_FLOOR) + U256::from(1))
            .unwrap();
        list_reference(&oracle);

        let mature_ts = ISSUED_AT as u64 + 21 * DAY + 1;
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, mature_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(qualified::scan_and_qualify(&ctx).unwrap(), 1);

        let r = outbe_intex::api::read_series(&s, sid(7)).unwrap();
        assert_eq!(
            r.lifecycle_state().unwrap(),
            outbe_intex::IntexState::Qualified
        );
        let f = IntexFactoryContract::new(s.clone());
        let trig_bin = IntexFactoryContract::price_to_bin(U256::from(EXPECTED_TRIGGER)).unwrap();
        assert_eq!(
            f.qualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, trig_bin))
                .unwrap(),
            1
        );
    });
}

#[test]
fn scan_and_call_force_calls_breached_series() {
    with_factory(|s| {
        let _f = qualify_series(&s, 7, sample(7));
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts));
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        fill_days(&oracle, last_closed_day, pair, 30, breach);

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(called::scan_and_call(&ctx).unwrap(), 1);
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(7))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Called
        );
    });
}

#[test]
fn scan_and_call_reads_daily_vwap_at_midnight() {
    // Regression: the scan fires on the midnight Cycle tick, when yesterday's
    // WorldwideDay snapshot does not exist yet (metadosis writes it at noon of
    // the current day). The finalized per-UTC-day VWAP is already closed by
    // then and must be the scan's price source. Exactly `threshold` (21)
    // breach days are seeded through the production finalization path and the
    // scan day itself stays unfinalized, so reading any other day — or any
    // other store — drops below the threshold and fails the call.
    with_factory(|s| {
        let _f = qualify_series(&s, 7, sample(7));
        let mut oracle = OracleContract::new(s.clone());
        setup_pair(&oracle);
        // Exact midnight UTC, well past the qualification period.
        let scan_ts = (ISSUED_AT as u64 / DAY + 60) * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts));
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);

        // Oldest-first: snapshot + finalize 21 closed days ending yesterday.
        let mut days = [0u32; 21];
        let mut d = last_closed_day;
        for slot in days.iter_mut().rev() {
            *slot = d;
            d = previous_date_key(d);
        }
        for day in days {
            let noon = date_key_to_utc_timestamp(day) + DAY / 2;
            oracle
                .write_snapshot(
                    noon,
                    &[(
                        outbe_oracle::api::AddressPair::new_coen_to(REFERENCE_ISO),
                        breach,
                        U256::from(1),
                    )],
                )
                .unwrap();
            oracle.finalize_utc_day_vwap(day).unwrap();
        }
        // The begin-block hook advances the watermark after finalizing.
        oracle
            .utc_day_vwap_last_finalized
            .write(last_closed_day)
            .unwrap();

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(called::scan_and_call(&ctx).unwrap(), 1);
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(7))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Called
        );
    });
}

// ---------------------------------------------------------------------
// begin-block / daily scan error isolation (no chain halt)
// ---------------------------------------------------------------------

#[test]
fn scan_does_not_halt_on_overflow_rate() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        let oracle = OracleContract::new(s.clone());
        let pair = outbe_oracle::api::AddressPair::new_coen_to(REFERENCE_ISO);
        oracle.pair_to_index.write(&pair, PAIR_ID).unwrap();
        // Out-of-range rate: price_to_bin overflows.
        oracle.exchange_rate.write(&PAIR_ID, U256::MAX).unwrap();
        list_reference(&oracle);

        let mature_ts = ISSUED_AT as u64 + 21 * DAY + 1;
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, mature_ts, CHAIN_ID),
            s.clone(),
        );
        // Must not halt: returns Ok(0) and leaves the series untouched.
        assert_eq!(qualified::scan_and_qualify(&ctx).unwrap(), 0);
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(7))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Issued
        );
    });
}

#[test]
fn scan_isolates_bad_series() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        // A bin entry whose series record does not exist: read_series errors -> the series must be
        // skipped (logged), not halt the block.
        IntexFactoryContract::new(s.clone())
            .insert_unqualified(sid(999), REFERENCE_ISO, U256::from(EXPECTED_FLOOR))
            .unwrap();

        let oracle = OracleContract::new(s.clone());
        let pair = outbe_oracle::api::AddressPair::new_coen_to(REFERENCE_ISO);
        // The ISO resolves through the pair registry, so the pair must exist
        // and the rate columns are keyed by its index.
        oracle.pair_to_index.write(&pair, PAIR_ID).unwrap();
        oracle
            .exchange_rate
            .write(&PAIR_ID, U256::from(EXPECTED_FLOOR) + U256::from(1))
            .unwrap();
        list_reference(&oracle);

        let mature_ts = ISSUED_AT as u64 + 21 * DAY + 1;
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, mature_ts, CHAIN_ID),
            s.clone(),
        );
        // Bad series (999) skipped; healthy series (7) still qualifies.
        assert_eq!(qualified::scan_and_qualify(&ctx).unwrap(), 1);
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(7))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Qualified
        );
    });
}

#[test]
fn call_scan_does_not_halt_on_overflow_vwap() {
    with_factory(|s| {
        let _f = qualify_series(&s, 7, sample(7));
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts));
        // Out-of-range VWAP for the completed day: price_to_bin overflows.
        fill_days(&oracle, last_closed_day, pair, 1, U256::MAX);

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        // Must not halt: returns Ok(0) and leaves the series Qualified.
        assert_eq!(called::scan_and_call(&ctx).unwrap(), 0);
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(7))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Qualified
        );
    });
}

#[test]
fn scan_caps_work_per_block_and_resumes_via_cursor() {
    with_factory(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = outbe_oracle::api::AddressPair::new_coen_to(REFERENCE_ISO);
        // The ISO resolves through the pair registry, so the pair must exist
        // and the rate columns are keyed by its index.
        oracle.pair_to_index.write(&pair, PAIR_ID).unwrap();
        // Rate well above both floors so both bins are eligible.
        oracle
            .exchange_rate
            .write(&PAIR_ID, U256::from(EXPECTED_FLOOR) * U256::from(1000))
            .unwrap();
        list_reference(&oracle);

        // Two distinct bins: the first holds exactly MAX_SERIES_PER_BLOCK entries, the second a few.
        // Bogus ids (no series record) are per-series skipped but still count toward the cap.
        let cap = crate::constants::MAX_GROUP_DECISIONS_PER_SWEEP;
        let f1 = U256::from(EXPECTED_FLOOR);
        let f2 = U256::from(EXPECTED_FLOOR) * U256::from(4);
        {
            let mut factory = IntexFactoryContract::new(s.clone());
            for id in 1..=cap {
                factory
                    .insert_unqualified(sid(id), REFERENCE_ISO, f1)
                    .unwrap();
            }
            for id in 1001..=1005u32 {
                factory
                    .insert_unqualified(sid(id), REFERENCE_ISO, f2)
                    .unwrap();
            }
        }
        let bin2 = IntexFactoryContract::price_to_bin(f2).unwrap();

        let ts = ISSUED_AT as u64 + 21 * DAY + 1;
        let ctx =
            BlockRuntimeContext::new(BlockContext::empty_for_tests(1, ts, CHAIN_ID), s.clone());

        // Block 1 caps after the first (cap-sized) bin; the second bin is deferred.
        qualified::scan_and_qualify(&ctx).unwrap();
        let cursor1 = IntexFactoryContract::new(s.clone())
            .qualify_scan_cursor
            .read(&REFERENCE_ISO)
            .unwrap();
        assert!(cursor1 > 0, "cursor advanced past the capped bin");
        assert_eq!(
            IntexFactoryContract::new(s.clone())
                .unqualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, bin2))
                .unwrap(),
            5,
            "second bin untouched in block 1"
        );

        // Block 2 resumes at the second bin and wraps the cursor to 0.
        qualified::scan_and_qualify(&ctx).unwrap();
        let cursor2 = IntexFactoryContract::new(s.clone())
            .qualify_scan_cursor
            .read(&REFERENCE_ISO)
            .unwrap();
        assert_eq!(cursor2, 0, "cursor wrapped after a full sweep");
    });
}

// ---------------------------------------------------------------------
// Genesis parameter profile: prod by default, dev when selected
// ---------------------------------------------------------------------

#[test]
fn config_defaults_to_prod_when_unset() {
    with_factory(|s| {
        let f = IntexFactoryContract::new(s.clone());
        // No genesis profile selected -> selector reads 0 -> prod bundle.
        assert_eq!(
            crate::config::read(&f).unwrap(),
            crate::config::IntexParams::PROD
        );
        assert_eq!(
            crate::config::IntexParams::PROD.commit_bond_minor,
            100_000_000u128 * 1_000_000u128
        );
        assert_eq!(
            crate::config::IntexParams::DEV.commit_bond_minor,
            100u128 * 1_000_000u128
        );
    });
}

#[test]
fn config_dev_profile_drives_issuance_and_qualification() {
    with_factory(|s| {
        let mut f = IntexFactoryContract::new(s.clone());
        // Select the dev profile through the single selector byte.
        f.config_profile.write(crate::config::PROFILE_DEV).unwrap();
        assert_eq!(
            crate::config::read(&f).unwrap(),
            crate::config::IntexParams::DEV
        );

        runtime::issue(&s, sample(7)).unwrap();

        // Issuance captures the dev call-trigger and dev-derived prices.
        let dev = crate::config::IntexParams::DEV;
        let r = outbe_intex::api::read_series(&s, sid(7)).unwrap();
        assert_eq!(r.call_notice_period, dev.call_notice_period);
        assert_eq!(
            r.floor_price_minor,
            U256::from(ENTRY_PRICE * u64::from(100 + dev.floor_rate) / 100)
        );
        assert_eq!(
            r.call_price_minor,
            U256::from(ENTRY_PRICE * u64::from(100 + dev.call_rate) / 100)
        );
        assert_eq!(
            r.call_trigger(),
            outbe_intex::IntexCallTrigger {
                call_window: dev.call_window,
                call_threshold: dev.call_threshold,
                call_notice_period: dev.call_notice_period,
            }
        );

        // The dev qualification period elapses long before the 21-day prod one.
        let rate = r.floor_price_minor + U256::from(1);
        let after_qualification = ISSUED_AT as u64 + u64::from(dev.qualification_period) + 1;
        assert_eq!(
            qualify_day(
                &s,
                &mut f,
                7,
                dev.qualification_period,
                after_qualification,
                rate
            ),
            1
        );
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(7))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Qualified
        );
    });
}

#[test]
fn config_unknown_selector_errors() {
    with_factory(|s| {
        let f = IntexFactoryContract::new(s.clone());
        f.config_profile.write(99u8).unwrap();
        assert!(crate::config::read(&f).is_err());
    });
}

/// Pin the selector slot index: the seeder writes raw slot 13, so the schema
/// must map `config_profile` there.
#[test]
fn config_profile_slot_matches_seeder_layout() {
    with_factory(|s| {
        let f = IntexFactoryContract::new(s.clone());
        assert_eq!(f.config_profile.slot(), U256::from(13));
    });
}

// ---------------------------------------------------------------------
// Creator-reward: distribute (paginated, native COEN, dust to last)
// ---------------------------------------------------------------------

fn contrib(n: u8) -> Address {
    Address::from([n; 20])
}

/// A future fan-in deadline relative to the harness clock (`ISSUED_AT`).
const DEADLINE_FUTURE: u64 = ISSUED_AT as u64 + 1000;

#[test]
fn distribute_pays_contributors_proportionally_with_dust_to_last() {
    with_factory(|s| {
        let owners = [contrib(1), contrib(2), contrib(3)];
        outbe_intex::api::record_contributors(
            &s,
            WorldwideDay::new(7),
            &[
                (owners[0], U256::from(100u64)),
                (owners[1], U256::from(200u64)),
                (owners[2], U256::from(300u64)),
            ],
        )
        .unwrap();
        // A single winning chain: its arrival completes the fan-in immediately.
        outbe_intex::api::arm_proceeds(&s, WorldwideDay::new(7), &[10], DEADLINE_FUTURE).unwrap();
        // Simulate the native value arriving on the precompile via distribute{value}.
        let amount = U256::from(1000u64);
        s.increase_balance(INTEX_FACTORY_ADDRESS, amount).unwrap();

        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7.into(),
            10,
            amount,
        )
        .unwrap();

        // distribute only registers; nothing is paid until the begin-block drain.
        assert_eq!(s.balance(owners[0]).unwrap(), U256::ZERO);
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);

        runtime::drain_distributions(&s).unwrap();

        // floor shares (amount * nominal / total); the last owner absorbs the
        // rounding remainder, so the sum is exactly `amount`.
        assert_eq!(s.balance(owners[0]).unwrap(), U256::from(166u64)); // 1000*100/600
        assert_eq!(s.balance(owners[1]).unwrap(), U256::from(333u64)); // 1000*200/600
        assert_eq!(s.balance(owners[2]).unwrap(), U256::from(501u64)); // 1000-166-333
                                                                       // precompile fully drained, progress + contributors cleared.
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(
            outbe_intex::api::get_progress(&s, WorldwideDay::new(7)).unwrap(),
            None
        );
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 0);
        assert_eq!(
            outbe_intex::api::contributor_count(&s, WorldwideDay::new(7)).unwrap(),
            0
        );
    });
}

#[test]
fn distribute_waits_for_all_winning_chains_then_pays_the_sum() {
    with_factory(|s| {
        let owners = [contrib(1), contrib(2)];
        outbe_intex::api::record_contributors(
            &s,
            WorldwideDay::new(7),
            &[
                (owners[0], U256::from(100u64)),
                (owners[1], U256::from(100u64)),
            ],
        )
        .unwrap();
        outbe_intex::api::arm_proceeds(&s, WorldwideDay::new(7), &[10, 20], DEADLINE_FUTURE)
            .unwrap();

        // Chain 10 arrives first: pot accumulates, fan-in not complete → no payout yet.
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(300u64))
            .unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7.into(),
            10,
            U256::from(300u64),
        )
        .unwrap();
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 0);
        assert!(!outbe_intex::api::proceeds_ready(&s, WorldwideDay::new(7)).unwrap());

        // Chain 20 completes the fan-in: one distribution over the summed pot.
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(500u64))
            .unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7.into(),
            20,
            U256::from(500u64),
        )
        .unwrap();
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);

        runtime::drain_distributions(&s).unwrap();
        // 800 split 100:100 → 400 each; map cleared since every chain is in.
        assert_eq!(s.balance(owners[0]).unwrap(), U256::from(400u64));
        assert_eq!(s.balance(owners[1]).unwrap(), U256::from(400u64));
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(
            outbe_intex::api::contributor_count(&s, WorldwideDay::new(7)).unwrap(),
            0
        );
    });
}

#[test]
fn distribute_deadline_forces_partial_payout_then_late_chain_supplements() {
    with_factory(|s| {
        let owners = [contrib(1), contrib(2)];
        outbe_intex::api::record_contributors(
            &s,
            WorldwideDay::new(7),
            &[
                (owners[0], U256::from(100u64)),
                (owners[1], U256::from(100u64)),
            ],
        )
        .unwrap();
        outbe_intex::api::arm_proceeds(&s, WorldwideDay::new(7), &[10, 20], DEADLINE_FUTURE)
            .unwrap();

        // Only chain 10 arrives before the deadline.
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(200u64))
            .unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7.into(),
            10,
            U256::from(200u64),
        )
        .unwrap();
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 0);

        // Past the deadline the sweep pays out what arrived; the map is retained.
        runtime::try_settle_proceeds(&s, WorldwideDay::new(7), DEADLINE_FUTURE + 1).unwrap();
        assert!(!outbe_intex::api::proceeds_finalize_on_done(&s, WorldwideDay::new(7)).unwrap());
        runtime::drain_distributions(&s).unwrap();
        assert_eq!(s.balance(owners[0]).unwrap(), U256::from(100u64));
        assert_eq!(s.balance(owners[1]).unwrap(), U256::from(100u64));
        assert_eq!(
            outbe_intex::api::contributor_count(&s, WorldwideDay::new(7)).unwrap(),
            2
        ); // retained

        // The straggler arrives: a supplementary payout over the same map, then finalize.
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(400u64))
            .unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7.into(),
            20,
            U256::from(400u64),
        )
        .unwrap();
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);
        runtime::drain_distributions(&s).unwrap();
        assert_eq!(s.balance(owners[0]).unwrap(), U256::from(300u64)); // +200
        assert_eq!(s.balance(owners[1]).unwrap(), U256::from(300u64));
        assert_eq!(
            outbe_intex::api::contributor_count(&s, WorldwideDay::new(7)).unwrap(),
            0
        ); // finalized
    });
}

#[test]
fn late_top_up_during_final_round_reaches_creators() {
    use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;
    with_factory(|s| {
        let owners = [contrib(1), contrib(2)];
        outbe_intex::api::record_contributors(
            &s,
            WorldwideDay::new(7),
            &[
                (owners[0], U256::from(100u64)),
                (owners[1], U256::from(100u64)),
            ],
        )
        .unwrap();
        // A single winning chain: the first arrival completes the fan-in, so the
        // round runs finalize-on-done (its end clears the contributor map).
        outbe_intex::api::arm_proceeds(&s, WorldwideDay::new(7), &[10], DEADLINE_FUTURE).unwrap();
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(200u64))
            .unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7.into(),
            10,
            U256::from(200u64),
        )
        .unwrap();
        assert!(outbe_intex::api::proceeds_finalize_on_done(&s, WorldwideDay::new(7)).unwrap());

        // Pay only the first contributor: the round is mid-drain, still active.
        runtime::pay_chunk(&s, WorldwideDay::new(7), 1).unwrap();
        assert_eq!(s.balance(owners[0]).unwrap(), U256::from(100u64));
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);

        // Chain 10 sends the rest of its proceeds while the round still drains: it
        // only tops the pot up (an in-flight round is never overlapped).
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(400u64))
            .unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7.into(),
            10,
            U256::from(400u64),
        )
        .unwrap();
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);

        // Finishing the round finds the topped-up pot and opens a supplementary
        // round over the same map instead of finalizing (which would strand it).
        runtime::pay_chunk(&s, WorldwideDay::new(7), 1).unwrap();
        assert_eq!(s.balance(owners[1]).unwrap(), U256::from(100u64));
        assert_eq!(
            outbe_intex::api::contributor_count(&s, WorldwideDay::new(7)).unwrap(),
            2
        ); // retained
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);

        // The supplementary round pays the top-up to the creators, then finalizes.
        runtime::drain_distributions(&s).unwrap();
        assert_eq!(s.balance(owners[0]).unwrap(), U256::from(300u64)); // +200
        assert_eq!(s.balance(owners[1]).unwrap(), U256::from(300u64)); // +200
        assert_eq!(
            outbe_intex::api::contributor_count(&s, WorldwideDay::new(7)).unwrap(),
            0
        ); // finalized
           // The money reached creators, never the vault or the burn path.
        assert_eq!(s.balance(VAULT_ROUTER_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
    });
}

#[test]
fn distribute_paginates_across_chunks() {
    with_factory(|s| {
        let owners = [contrib(1), contrib(2), contrib(3)];
        outbe_intex::api::record_contributors(
            &s,
            WorldwideDay::new(7),
            &[
                (owners[0], U256::from(100u64)),
                (owners[1], U256::from(200u64)),
                (owners[2], U256::from(300u64)),
            ],
        )
        .unwrap();
        let amount = U256::from(600u64);
        s.increase_balance(INTEX_FACTORY_ADDRESS, amount).unwrap();
        outbe_intex::api::start_distribution(&s, WorldwideDay::new(7), amount, U256::from(600u64))
            .unwrap();

        // Chunk 1 (limit 2): pays the first two, cursor advances, still active.
        runtime::pay_chunk(&s, WorldwideDay::new(7), 2).unwrap();
        assert_eq!(s.balance(owners[0]).unwrap(), U256::from(100u64));
        assert_eq!(s.balance(owners[1]).unwrap(), U256::from(200u64));
        assert_eq!(s.balance(owners[2]).unwrap(), U256::ZERO);
        assert_eq!(
            outbe_intex::api::get_progress(&s, WorldwideDay::new(7))
                .unwrap()
                .unwrap()
                .cursor,
            2
        );
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);

        // Chunk 2: pays the last and finalizes.
        runtime::pay_chunk(&s, WorldwideDay::new(7), 2).unwrap();
        assert_eq!(s.balance(owners[2]).unwrap(), U256::from(300u64));
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(
            outbe_intex::api::get_progress(&s, WorldwideDay::new(7)).unwrap(),
            None
        );
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 0);
    });
}

#[test]
fn distribute_rejects_non_origin_router() {
    with_factory(|s| {
        outbe_intex::api::record_contributors(
            &s,
            WorldwideDay::new(7),
            &[(contrib(1), U256::from(100u64))],
        )
        .unwrap();
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(100u64))
            .unwrap();
        let err = runtime::distribute(&s, holder(), 7.into(), 10, U256::from(100u64)).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("origin router"));
    });
}

#[test]
fn distribute_no_contributors_burns() {
    use alloy_sol_types::SolEvent;
    use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    storage.stub_sub_call_at(
        crate::constants::INTEX_NFT1155_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );
    storage.stub_sub_call_at(
        crate::constants::ORIGIN_ROUTER_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );

    StorageHandle::enter(&mut storage, |s| {
        // Armed but no contributors recorded; the single chain completes the fan-in.
        outbe_intex::api::arm_proceeds(&s, WorldwideDay::new(7), &[10], DEADLINE_FUTURE).unwrap();
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(100u64))
            .unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7.into(),
            10,
            U256::from(100u64),
        )
        .unwrap();

        // No distribution opened; the ownerless proceeds were destroyed, not vaulted.
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 0);
        assert_eq!(s.balance(VAULT_ROUTER_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
    });

    let sig = IIntexFactory::ProceedsBurned::SIGNATURE_HASH;
    let found = storage.get_events(INTEX_FACTORY_ADDRESS).iter().any(|log| {
        log.topics().first() == Some(&sig)
            && IIntexFactory::ProceedsBurned::decode_log_data(log)
                .map(|ev| ev.worldwideDay == 7 && ev.amount == U256::from(100u64))
                .unwrap_or(false)
    });
    assert!(found, "expected ProceedsBurned event");
}

#[test]
fn begin_block_drain_completes_active_distributions() {
    with_factory(|s| {
        // Two series, each left partially distributed (1 of 3 contributors paid).
        for (sid, owners) in [
            (7u32, [contrib(1), contrib(2), contrib(3)]),
            (9u32, [contrib(4), contrib(5), contrib(6)]),
        ] {
            outbe_intex::api::record_contributors(
                &s,
                WorldwideDay::new(sid),
                &[
                    (owners[0], U256::from(100u64)),
                    (owners[1], U256::from(200u64)),
                    (owners[2], U256::from(300u64)),
                ],
            )
            .unwrap();
            s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(600u64))
                .unwrap();
            outbe_intex::api::start_distribution(
                &s,
                WorldwideDay::new(sid),
                U256::from(600u64),
                U256::from(600u64),
            )
            .unwrap();
            // Pay only the first contributor, leaving the series active.
            runtime::pay_chunk(&s, WorldwideDay::new(sid), 1).unwrap();
        }
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 2);

        // One begin-block drain finishes both (3 <= DIST_CHUNK_LIMIT).
        runtime::drain_distributions(&s).unwrap();

        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 0);
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
        // series 7 fully paid
        assert_eq!(s.balance(contrib(1)).unwrap(), U256::from(100u64));
        assert_eq!(s.balance(contrib(2)).unwrap(), U256::from(200u64));
        assert_eq!(s.balance(contrib(3)).unwrap(), U256::from(300u64));
        // series 9 fully paid
        assert_eq!(s.balance(contrib(4)).unwrap(), U256::from(100u64));
        assert_eq!(s.balance(contrib(5)).unwrap(), U256::from(200u64));
        assert_eq!(s.balance(contrib(6)).unwrap(), U256::from(300u64));
    });
}

#[test]
fn begin_block_drain_isolates_failing_series() {
    with_factory(|s| {
        outbe_intex::api::record_contributors(
            &s,
            WorldwideDay::new(7),
            &[(contrib(1), U256::from(100u64))],
        )
        .unwrap();
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(100u64))
            .unwrap();
        outbe_intex::api::start_distribution(
            &s,
            WorldwideDay::new(7),
            U256::from(100u64),
            U256::from(100u64),
        )
        .unwrap();

        // Series 9 is unfunded: its first transfer fails mid-chunk.
        outbe_intex::api::record_contributors(
            &s,
            WorldwideDay::new(9),
            &[
                (contrib(2), U256::from(100u64)),
                (contrib(3), U256::from(500u64)),
            ],
        )
        .unwrap();
        outbe_intex::api::start_distribution(
            &s,
            WorldwideDay::new(9),
            U256::from(600u64),
            U256::from(600u64),
        )
        .unwrap();

        // The drain must not error: the failing series is skipped and rolled back.
        runtime::drain_distributions(&s).unwrap();

        assert_eq!(s.balance(contrib(1)).unwrap(), U256::from(100u64));
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);
        let p = outbe_intex::api::get_progress(&s, WorldwideDay::new(9))
            .unwrap()
            .unwrap();
        assert_eq!(p.cursor, 0);
        assert_eq!(p.paid_so_far, U256::ZERO);
        assert_eq!(s.balance(contrib(2)).unwrap(), U256::ZERO);
    });
}

/// IntexFactory is a payable route, so the boundary credits value to its
/// address. Its dispatch must refuse value for every selector outside
/// `PAYABLE_SELECTORS`, or a funded call to a non-payable selector would strand
/// native value at an address with no accounting entry for it.
///
/// Characterization: the per-arm checks this replaced already covered these two
/// selectors with the same message, so the test pins current behavior rather
/// than proving a fix. Its value is catching a future removal of the guard.
#[test]
fn unpublished_selectors_refuse_native_value() {
    use crate::precompile::{dispatch, IIntexFactory};

    let calls = [
        IIntexFactory::settleCall {
            seriesId: Default::default(),
            intexHolder: Address::ZERO,
            amount: U256::ZERO,
            paymentToken: Address::ZERO,
        }
        .abi_encode(),
        IIntexFactory::setAuthorizedSettlerCall {
            seriesId: Default::default(),
            settler: Address::ZERO,
        }
        .abi_encode(),
    ];

    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        for data in &calls {
            let funded = dispatch(storage.clone(), data, Address::ZERO, U256::from(1u64));
            assert!(
                matches!(
                    funded,
                    Err(outbe_primitives::error::PrecompileError::Revert(ref message))
                        if message == "non-payable function called with value"
                ),
                "unpublished selector must refuse value, got {funded:?}"
            );
        }
    });
}

/// `distribute` is the one published payable selector here, and it credits
/// auction proceeds straight from `msg.value`. The route table only decides that
/// this *address* may be credited; nothing in it proves the dispatch actually
/// hands the value to the handler. This does: the handler rejects a zero amount,
/// so the two outcomes separate exactly on whether the value arrived.
#[test]
fn distribute_receives_the_call_value() {
    use alloy_sol_types::SolCall;

    use crate::constants::ORIGIN_ROUTER_ADDRESS;
    use crate::precompile::{dispatch, IIntexFactory};

    let data = IIntexFactory::distributeCall {
        worldwideDay: 7u32,
        srcChainId: 10u32,
    }
    .abi_encode();

    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let unfunded = dispatch(storage.clone(), &data, ORIGIN_ROUTER_ADDRESS, U256::ZERO);
        assert!(
            matches!(
                unfunded,
                Err(outbe_primitives::error::PrecompileError::Revert(ref message))
                    if message == "amount must be positive"
            ),
            "a zero-value distribute must reach the handler with nothing, got {unfunded:?}"
        );

        let funded = dispatch(
            storage.clone(),
            &data,
            ORIGIN_ROUTER_ADDRESS,
            U256::from(1_000u64),
        );
        assert!(
            !matches!(
                funded,
                Err(outbe_primitives::error::PrecompileError::Revert(ref message))
                    if message == "amount must be positive"
            ),
            "a funded distribute must reach the handler with the value, got {funded:?}"
        );
    });
}

// ---------------------------------------------------------------------
// Monitoring per reference currency
// ---------------------------------------------------------------------

/// A second reference currency, priced by its own pair at its own index.
const EUR_ISO: u16 = 978;
const EUR_PAIR_ID: u32 = 2;

fn eur_series(worldwide_day: u32) -> IssuanceParams {
    IssuanceParams {
        series_id: SeriesId::pack(WorldwideDay::new(worldwide_day), *b"EUR", b'E').unwrap(),
        reference_currency: EUR_ISO,
        issuance_currency: EUR_ISO,
        ..sample(worldwide_day)
    }
}

fn write_rate(oracle: &OracleContract, iso_code: u16, pair_id: u32, rate: U256) {
    let pair = outbe_oracle::api::AddressPair::new_coen_to(iso_code);
    oracle.pair_to_index.write(&pair, pair_id).unwrap();
    oracle.exchange_rate.write(&pair_id, rate).unwrap();
}

#[test]
fn a_currency_rate_never_qualifies_another_currency_series() {
    with_factory(|s| {
        // Both series carry the same entry price, so their floors land in the
        // same bin: only the currency namespace keeps them apart.
        runtime::issue(&s, sample(7)).unwrap();
        runtime::issue(&s, eur_series(8)).unwrap();

        let oracle = OracleContract::new(s.clone());
        oracle.reference_currencies.push(REFERENCE_ISO).unwrap();
        oracle.reference_currencies.push(EUR_ISO).unwrap();
        let above = U256::from(EXPECTED_FLOOR) + U256::from(1);
        let below = U256::from(EXPECTED_FLOOR) - U256::from(1);
        write_rate(&oracle, REFERENCE_ISO, PAIR_ID, above);
        write_rate(&oracle, EUR_ISO, EUR_PAIR_ID, below);

        let mature_ts = ISSUED_AT as u64 + 21 * DAY + 1;
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, mature_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(qualified::scan_and_qualify(&ctx).unwrap(), 1);

        // The euro series shares the dollar series' bin and sits below its own
        // rate, so a scan that crossed the currency border would have taken it.
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(7))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Qualified
        );
        let eur_id = eur_series(8).series_id;
        assert_eq!(
            outbe_intex::api::read_series(&s, eur_id)
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Issued
        );

        // Its own rate crossing the floor is what qualifies it.
        write_rate(&oracle, EUR_ISO, EUR_PAIR_ID, above);
        assert_eq!(qualified::scan_and_qualify(&ctx).unwrap(), 1);
        assert_eq!(
            outbe_intex::api::read_series(&s, eur_id)
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Qualified
        );
    });
}

#[test]
fn an_unpriced_reference_currency_is_skipped_not_fatal() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        let oracle = OracleContract::new(s.clone());
        // Listed before its pair exists — the registry and the pair registry are
        // populated independently.
        oracle.reference_currencies.push(EUR_ISO).unwrap();
        oracle.reference_currencies.push(REFERENCE_ISO).unwrap();
        write_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(EXPECTED_FLOOR) + U256::from(1),
        );

        let mature_ts = ISSUED_AT as u64 + 21 * DAY + 1;
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, mature_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(qualified::scan_and_qualify(&ctx).unwrap(), 1);
    });
}

#[test]
fn a_currency_cut_off_by_the_budget_is_scanned_first_next_block() {
    with_factory(|s| {
        runtime::issue(&s, eur_series(8)).unwrap();

        let oracle = OracleContract::new(s.clone());
        oracle.reference_currencies.push(REFERENCE_ISO).unwrap();
        oracle.reference_currencies.push(EUR_ISO).unwrap();
        let above = U256::from(EXPECTED_FLOOR) + U256::from(1);
        write_rate(&oracle, REFERENCE_ISO, PAIR_ID, above);
        write_rate(&oracle, EUR_ISO, EUR_PAIR_ID, above);

        // The dollar bin alone fills a whole block's budget. Ids without a series
        // record are skipped per series but still count against it.
        {
            let mut factory = IntexFactoryContract::new(s.clone());
            for id in 1..=crate::constants::MAX_GROUP_DECISIONS_PER_SWEEP {
                factory
                    .insert_unqualified(sid(id), REFERENCE_ISO, U256::from(EXPECTED_FLOOR))
                    .unwrap();
            }
        }

        let mature_ts = ISSUED_AT as u64 + 21 * DAY + 1;
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, mature_ts, CHAIN_ID),
            s.clone(),
        );

        let eur_id = eur_series(8).series_id;
        qualified::scan_and_qualify(&ctx).unwrap();
        assert_eq!(
            outbe_intex::api::read_series(&s, eur_id)
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Issued,
            "the dollar bin consumed the block's budget"
        );
        assert_eq!(
            IntexFactoryContract::new(s.clone())
                .qualify_currency_cursor
                .read()
                .unwrap(),
            1,
            "the next block resumes at the currency that was cut off"
        );

        qualified::scan_and_qualify(&ctx).unwrap();
        assert_eq!(
            outbe_intex::api::read_series(&s, eur_id)
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Qualified
        );
    });
}

// ---------------------------------------------------------------------
// Settlement in the issuance currency
// ---------------------------------------------------------------------

/// Issue a series whose issuance currency differs from its reference, with a
/// payment token reporting `iso` and 18 decimals and a registered vault.
fn with_dual_currency_series<R>(iso: u64, f: impl FnOnce(StorageHandle) -> R) -> R {
    use crate::sol_ext::{IReferenceCurrency, IERC20};
    use outbe_vaultrouter::api::IVaultRouter;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    storage.stub_sub_call_at(crate::constants::INTEX_NFT1155_ADDRESS, word(0));
    storage.stub_sub_call_at(crate::constants::ORIGIN_ROUTER_ADDRESS, word(0));
    storage.stub_sub_call_at_selector(
        outbe_primitives::addresses::VAULT_ROUTER_ADDRESS,
        IVaultRouter::assetVaultsCountCall::SELECTOR,
        word(1),
    );
    storage.stub_sub_call_at_selector(
        payment_token(),
        IReferenceCurrency::isoCodeCall::SELECTOR,
        word(iso),
    );
    storage.stub_sub_call_at_selector(payment_token(), IERC20::decimalsCall::SELECTOR, word(18));

    StorageHandle::enter(&mut storage, |s| {
        let params = IssuanceParams {
            issuance_currency: EUR_ISO,
            ..sample(7)
        };
        runtime::issue(&s, params).unwrap();
        f(s)
    })
}

/// Every stablecoin-backed COEN/ISO Oracle rate uses six decimals.
const COEN_ISO_RATE_SCALE: U256 = U256::from_limbs([1_000_000, 0, 0, 0]);

/// Publish a COEN rate for `iso_code`, stamped `age` seconds ago.
fn publish_rate(oracle: &OracleContract, iso_code: u16, pair_id: u32, rate: U256, age: u64) {
    write_rate(oracle, iso_code, pair_id, rate);
    oracle
        .exchange_rate_timestamp
        .write(&pair_id, ISSUED_AT as u64 - age)
        .unwrap();
}

#[test]
fn the_issuance_currency_settles_through_the_coen_pivot() {
    with_dual_currency_series(EUR_ISO as u64, |s| {
        let oracle = OracleContract::new(s.clone());
        // COEN buys twice as many dollars as euros, so the euro price of one
        // Intex is half its dollar price.
        publish_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(2u64) * COEN_ISO_RATE_SCALE,
            0,
        );
        publish_rate(&oracle, EUR_ISO, EUR_PAIR_ID, COEN_ISO_RATE_SCALE, 0);

        let cost = runtime::quote_cost_amount(&s, sid(7), payment_token()).unwrap();
        assert_eq!(cost, U256::from(500_000_000_000_000_000u64));
    });
}

#[test]
fn issuance_currency_settlement_rounds_a_non_divisible_fx_result_up_once() {
    with_dual_currency_series(EUR_ISO as u64, |s| {
        let oracle = OracleContract::new(s.clone());
        publish_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(3u64) * COEN_ISO_RATE_SCALE,
            0,
        );
        publish_rate(&oracle, EUR_ISO, EUR_PAIR_ID, COEN_ISO_RATE_SCALE, 0);

        let cost = runtime::quote_cost_amount(&s, sid(7), payment_token()).unwrap();
        assert_eq!(cost, U256::from(333_333_333_333_333_334u64));
    });
}

#[test]
fn an_unpriced_issuance_currency_cannot_be_settled_in() {
    with_dual_currency_series(EUR_ISO as u64, |s| {
        let oracle = OracleContract::new(s.clone());
        publish_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(2u64) * COEN_ISO_RATE_SCALE,
            0,
        );
        // No euro pair at all.
        let err = runtime::quote_cost_amount(&s, sid(7), payment_token()).unwrap_err();
        assert!(err.to_string().contains("no COEN rate published"), "{err}");
    });
}

#[test]
fn a_stale_rate_cannot_be_settled_in() {
    with_dual_currency_series(EUR_ISO as u64, |s| {
        let oracle = OracleContract::new(s.clone());
        publish_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(2u64) * COEN_ISO_RATE_SCALE,
            0,
        );
        publish_rate(
            &oracle,
            EUR_ISO,
            EUR_PAIR_ID,
            COEN_ISO_RATE_SCALE,
            crate::constants::FX_RATE_MAX_AGE_SECONDS + 1,
        );

        let err = runtime::quote_cost_amount(&s, sid(7), payment_token()).unwrap_err();
        assert!(err.to_string().contains("too old"), "{err}");
    });
}

#[test]
fn issuance_currency_settlement_rejects_fx_overflow() {
    with_dual_currency_series(EUR_ISO as u64, |s| {
        let oracle = OracleContract::new(s.clone());
        publish_rate(&oracle, REFERENCE_ISO, PAIR_ID, COEN_ISO_RATE_SCALE, 0);
        publish_rate(&oracle, EUR_ISO, EUR_PAIR_ID, U256::MAX, 0);

        let err = runtime::quote_cost_amount(&s, sid(7), payment_token()).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("overflow"), "{err}");
    });
}

#[test]
fn the_reference_currency_settles_without_reading_any_rate() {
    // No rate is published at all, yet the reference currency still settles.
    with_dual_currency_series(REFERENCE_ISO as u64, |s| {
        let cost = runtime::quote_cost_amount(&s, sid(7), payment_token()).unwrap();
        assert_eq!(cost, U256::from(1_000_000_000_000_000_000u64));
    });
}

// ---------------------------------------------------------------------
// Packing a day's issuance into messages
// ---------------------------------------------------------------------

fn leg(chain_id: u32, series: u32, recipients: usize) -> runtime::IssuanceLeg {
    let mut payload = crate::sol_ext::IOriginRouter::IssuanceInstructionsParams {
        seriesId: sid(series).into(),
        worldwideDay: series,
        issuedIntexCount: 1,
        promisLoadMinor: PROMIS_LOAD_MINOR,
        entryPriceMinor: 0,
        floorPriceMinor: 0,
        callNoticePeriod: 0,
        issuanceCurrency: 840,
        referenceCurrency: 840,
        callWindow: 0,
        callThreshold: 0,
        callPriceMinor: 0,
        recipients: Vec::new(),
        quantities: Vec::new(),
    };
    for i in 0..recipients {
        payload
            .recipients
            .push(Address::from([(i % 250) as u8 + 1; 20]));
        payload.quantities.push(U256::from(1u64));
    }
    runtime::IssuanceLeg { chain_id, payload }
}

fn shape(
    messages: &[(
        u32,
        Vec<crate::sol_ext::IOriginRouter::IssuanceInstructionsParams>,
    )],
) -> Vec<(u32, usize, usize)> {
    messages
        .iter()
        .map(|(chain, series)| {
            (
                *chain,
                series.len(),
                series.iter().map(|s| s.recipients.len()).sum(),
            )
        })
        .collect()
}

#[test]
fn a_chains_series_travel_together_up_to_the_message_caps() {
    // Nine empty-recipient series on one chain: the series cap alone splits them.
    let legs: Vec<_> = (1..=9u32).map(|s| leg(10, s, 0)).collect();
    assert_eq!(
        shape(&runtime::pack_issuance_messages(legs)),
        vec![(10, MAX_SERIES_PER_MESSAGE, 0), (10, 1, 0)]
    );
}

#[test]
fn a_message_never_carries_more_winners_than_the_wire_allows() {
    // Two series whose winners together exceed the recipient cap: no message may overfill.
    let per_series = MAX_RECIPIENTS_PER_ISSUANCE - 1;
    let legs = vec![leg(10, 1, per_series), leg(10, 2, per_series)];
    let messages = runtime::pack_issuance_messages(legs);

    for (_, _, recipients) in shape(&messages) {
        assert!(
            recipients <= MAX_RECIPIENTS_PER_ISSUANCE,
            "a message carried {recipients} winners, over the wire's cap"
        );
    }
    let total: usize = shape(&messages).iter().map(|(_, _, r)| r).sum();
    assert_eq!(total, 2 * per_series, "every winner is carried exactly once");
}

#[test]
fn one_series_with_more_winners_than_a_message_spans_several() {
    // One series far past the cap: it spans full messages plus a remainder, and every piece
    // repeats the series so whichever arrives first can create it.
    let winners = 5 * MAX_RECIPIENTS_PER_ISSUANCE - 10;
    let messages = runtime::pack_issuance_messages(vec![leg(10, 1, winners)]);

    let pieces = shape(&messages);
    assert_eq!(
        pieces.len(),
        winners.div_ceil(MAX_RECIPIENTS_PER_ISSUANCE),
        "one message per full slice plus the remainder"
    );
    assert_eq!(
        pieces.iter().map(|(_, _, r)| r).sum::<usize>(),
        winners,
        "every winner is carried exactly once"
    );
    for (_, series) in &messages {
        assert_eq!(SeriesId::from(series[0].seriesId), sid(1));
    }
}

#[test]
fn a_chains_series_batch_even_when_another_chain_comes_between_them() {
    // A day emits its legs series by series, so one chain's legs are never adjacent.
    // Both of chain 10's series still travel together, or the batching would do
    // nothing precisely when a day has several currency pairs.
    let legs = vec![leg(10, 1, 2), leg(20, 1, 3), leg(10, 2, 1)];
    assert_eq!(
        shape(&runtime::pack_issuance_messages(legs)),
        vec![(10, 2, 3), (20, 1, 3)]
    );
}
