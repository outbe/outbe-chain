use alloy_primitives::{address, keccak256, Address, B256, U256};
use alloy_sol_types::SolCall;
use outbe_common::WorldwideDay;
use outbe_oracle::contract::OracleContract;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::called;
use crate::constants::QUALIFIER_REFERENCE_ISO;
use crate::precompile::{self, IIntexFactory};
use crate::qualified;
use crate::runtime;
use crate::schema::{IntexFactoryContract, IssuanceParams};

const DAY: u64 = 24 * 60 * 60;

fn holder() -> Address {
    address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
}

const CHAIN_ID: u64 = 1;
const ISSUED_AT: u32 = 1_700_000_000;
const PROMIS_LOAD_MINOR: u128 = 1_000_000_000_000_000_000; // 1e18
const CALL_PERIOD: u32 = 21 * 24 * 60 * 60;

// COEN clearing price and the floor/trigger derived from it at issuance.
const COEN_PRICE: u64 = 1_000_000;
const EXPECTED_FLOOR: u64 = 1_080_000; // COEN_PRICE * 108/100
const EXPECTED_TRIGGER: u64 = 2_280_000; // COEN_PRICE * 228/100

fn with_factory<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    // Stub IntexNFT1155: void calls succeed; balanceOf returns 0 (32 bytes).
    storage.stub_sub_call_at(
        crate::constants::INTEX_NFT1155_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );
    // Stub OriginMessenger: returns MessagingFee { nativeFee: 0, lzTokenFee: 0 } (64 bytes).
    storage.stub_sub_call_at(
        crate::constants::ORIGIN_MESSENGER_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 64]),
    );
    StorageHandle::enter(&mut storage, f)
}

fn sample(series_id: u32) -> IssuanceParams {
    IssuanceParams {
        series_id,
        issued_intex_count: 100,
        promis_load_minor: PROMIS_LOAD_MINOR,
        cost_amount_minor: 2_000,
        coen_price: U256::from(COEN_PRICE),
        intex_call_period: CALL_PERIOD,
        call_window_days: 30,
        call_threshold_days: 21,
        issuance_currency: 840,
        reference_currency: 840,
        recipients: vec![],
        quantities: vec![],
    }
}

#[test]
fn issue_creates_series_in_registry() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();

        // The series is captured in Intex with the issuance identity.
        let r = outbe_intex::api::read_series(&s, 7).unwrap();
        assert_eq!(r.series_id, 7);
        assert_eq!(r.promis_load_minor, U256::from(PROMIS_LOAD_MINOR));
        assert_eq!(r.cost_amount_minor, 2_000);
        // Floor and trigger are derived from the clearing price at issuance.
        assert_eq!(r.floor_price_minor, U256::from(EXPECTED_FLOOR));
        assert_eq!(r.issued_intex_count, 100);
        assert_eq!(r.intex_call_period, CALL_PERIOD);
        assert_eq!(
            r.call_trigger(),
            outbe_intex::IntexCallTrigger {
                window_days: 30,
                threshold_days: 21,
                call_price_minor: U256::from(EXPECTED_TRIGGER),
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
fn issue_enrolls_series_in_dense_enumeration() {
    with_factory(|s| {
        runtime::issue(&s, sample(11)).unwrap();
        runtime::issue(&s, sample(22)).unwrap();
        assert_eq!(outbe_intex::api::total_series(&s).unwrap(), 2);
        assert_eq!(outbe_intex::api::series_id_at(&s, 0).unwrap(), 11);
        assert_eq!(outbe_intex::api::series_id_at(&s, 1).unwrap(), 22);
    });
}

#[test]
fn floor_and_call_derivation() {
    let floor = runtime::derived_floor(U256::from(COEN_PRICE)).unwrap();
    let call = runtime::derived_call_price(U256::from(COEN_PRICE)).unwrap();
    assert_eq!(floor, U256::from(EXPECTED_FLOOR));
    assert_eq!(call, U256::from(EXPECTED_TRIGGER));

    let one = U256::from(1_000_000_000_000_000_000u64);
    assert_eq!(
        runtime::derived_floor(one).unwrap(),
        U256::from(1_080_000_000_000_000_000u64)
    );
    assert_eq!(
        runtime::derived_call_price(one).unwrap(),
        U256::from(2_280_000_000_000_000_000u64)
    );
}

// ---------------------------------------------------------------------
// settle gating (value movement is localnet-exercised, not unit tested)
// ---------------------------------------------------------------------

#[test]
fn settle_rejects_zero_amount() {
    with_factory(|s| {
        assert!(runtime::settle(&s, 7, holder(), holder(), U256::ZERO).is_err());
    });
}

#[test]
fn settle_rejects_missing_series() {
    with_factory(|s| {
        assert!(runtime::settle(&s, 7, holder(), holder(), U256::from(1)).is_err());
    });
}

#[test]
fn settle_rejects_wrong_state_issued() {
    with_factory(|s| {
        // Born Issued; settlement is only valid in Qualified/Called.
        runtime::issue(&s, sample(7)).unwrap();
        let err = runtime::settle(&s, 7, holder(), holder(), U256::from(1)).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("settleable"));
    });
}

#[test]
fn settle_rejects_expired_deadline() {
    // Late block timestamp so the Called deadline is already in the past.
    let now = (ISSUED_AT as u64) + (CALL_PERIOD as u64) + 1_000;
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(now));
    storage.stub_sub_call_at(
        crate::constants::INTEX_NFT1155_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );
    storage.stub_sub_call_at(
        crate::constants::ORIGIN_MESSENGER_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 64]),
    );
    StorageHandle::enter(&mut storage, |s| {
        runtime::issue(&s, sample(7)).unwrap();
        // deadline = ISSUED_AT + CALL_PERIOD < now
        outbe_intex::api::mark_called(&s, 7, ISSUED_AT).unwrap();
        let err = runtime::settle(&s, 7, holder(), holder(), U256::from(1)).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("deadline"));
    });
}

#[test]
fn set_authorized_settler_round_trip() {
    with_factory(|s| {
        let settler = address!("0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB");
        runtime::set_authorized_settler(&s, holder(), 7, settler).unwrap();
        let f = IntexFactoryContract::new(s.clone());
        assert_eq!(f.read_authorized_settler(holder(), 7).unwrap(), settler);
    });
}

// ---------------------------------------------------------------------
// minePromis PoW + derivations (value movement is localnet-exercised)
// ---------------------------------------------------------------------

#[test]
fn settled_token_id_derivation() {
    // uint256(keccak256("SETTLED" ++ seriesId_be32))
    let series_id = 7u32;
    let mut buf = Vec::new();
    buf.extend_from_slice(b"SETTLED");
    buf.extend_from_slice(&series_id.to_be_bytes());
    assert_eq!(
        runtime::settled_token_id(series_id),
        U256::from_be_bytes(keccak256(&buf).0)
    );
}

#[test]
fn compute_pow_hash_matches_manual_sha256() {
    // SHA256(hex(holder)++hex(promisAmount)++hex(seriesId)++hex(seq) ++ nonce_be8)
    let promis_amount = U256::from(1_000u64);
    let (series_id, seq, nonce) = (7u32, 3u32, 42u64);
    let got = runtime::compute_pow_hash(holder(), promis_amount, series_id, seq, U256::from(nonce))
        .unwrap();

    let mut preimage = String::new();
    preimage.push_str(&hex::encode(holder().as_slice()));
    preimage.push_str(&hex::encode(promis_amount.to_be_bytes::<32>()));
    preimage.push_str(&hex::encode(series_id.to_be_bytes()));
    preimage.push_str(&hex::encode(seq.to_be_bytes()));
    let mut data = preimage.into_bytes();
    data.extend_from_slice(&nonce.to_be_bytes());
    let expected = ring::digest::digest(&ring::digest::SHA256, &data);
    assert_eq!(got.as_slice(), expected.as_ref());
}

#[test]
fn validate_pow_accepts_valid_and_rejects_invalid_nonce() {
    let pa = U256::from(1_000u64);
    let (sid, seq) = (7u32, 0u32);
    // Difficulty 1: ~1/256 of nonces pass; brute-force a valid and an invalid one.
    let mut good = None;
    let mut bad = None;
    for n in 0u64..100_000 {
        let ok = runtime::validate_pow(holder(), pa, sid, seq, U256::from(n)).is_ok();
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
        sid,
        seq,
        U256::from(good.expect("a valid nonce"))
    )
    .is_ok());
    assert!(runtime::validate_pow(
        holder(),
        pa,
        sid,
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
        1,
        0,
        U256::from(u64::MAX) + U256::from(1)
    )
    .is_err());
}

#[test]
fn mine_promis_rejects_zero_amount() {
    with_factory(|s| {
        assert!(runtime::mine_promis(&s, 7, holder(), U256::ZERO, U256::ZERO).is_err());
    });
}

#[test]
fn mine_promis_rejects_missing_series() {
    with_factory(|s| {
        assert!(runtime::mine_promis(&s, 7, holder(), U256::from(1), U256::ZERO).is_err());
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
        assert_eq!(f.unqualified_bin_count.read(&bin).unwrap(), 1);
    });
}

#[test]
fn insert_remove_unqualified_roundtrip() {
    with_factory(|s| {
        let mut f = IntexFactoryContract::new(s.clone());
        let floor = U256::from(2_000u64);
        let bin = IntexFactoryContract::price_to_bin(floor).unwrap();
        f.insert_unqualified(11, floor).unwrap();
        f.insert_unqualified(22, floor).unwrap();
        assert_eq!(f.unqualified_bin_count.read(&bin).unwrap(), 2);
        f.remove_unqualified(11, floor).unwrap();
        assert_eq!(f.unqualified_bin_count.read(&bin).unwrap(), 1);
        f.remove_unqualified(22, floor).unwrap();
        assert_eq!(f.unqualified_bin_count.read(&bin).unwrap(), 0);
    });
}

#[test]
fn try_qualify_gates_maturity_floor_and_latches() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        let mut f = IntexFactoryContract::new(s.clone());
        let floor = U256::from(EXPECTED_FLOOR);
        let immature = ISSUED_AT as u64 + 10;
        let mature = ISSUED_AT as u64 + 21 * DAY + 1;

        // Immature -> false.
        assert!(!qualified::try_qualify(&s, &mut f, 7, immature, floor + U256::from(1)).unwrap());
        // Mature but rate == floor (strict >) -> false.
        assert!(!qualified::try_qualify(&s, &mut f, 7, mature, floor).unwrap());
        // Mature + rate > floor -> qualifies, latched, removed from bin.
        assert!(qualified::try_qualify(&s, &mut f, 7, mature, floor + U256::from(1)).unwrap());
        assert_eq!(
            outbe_intex::api::read_series(&s, 7)
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Qualified
        );
        let bin = IntexFactoryContract::price_to_bin(floor).unwrap();
        assert_eq!(f.unqualified_bin_count.read(&bin).unwrap(), 0);
        // Already Qualified -> false.
        assert!(!qualified::try_qualify(&s, &mut f, 7, mature, floor + U256::from(1)).unwrap());
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
            seriesId: 7,
            settler,
        }
        .abi_encode();
        // Caller (holder) is taken from msg.sender, not the calldata.
        precompile::dispatch(s.clone(), &data, holder(), U256::ZERO).unwrap();
        let f = IntexFactoryContract::new(s.clone());
        assert_eq!(f.read_authorized_settler(holder(), 7).unwrap(), settler);
    });
}

#[test]
fn dispatch_rejects_value() {
    with_factory(|s| {
        let settler = address!("0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB");
        let data = IIntexFactory::setAuthorizedSettlerCall {
            seriesId: 7,
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
            seriesId: 7,
            amount: U256::from(1),
            nonce: U256::ZERO,
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
    assert!(qualified::try_qualify(s, &mut f, id, mature, floor + U256::from(1)).unwrap());
    f
}

fn setup_pair(oracle: &OracleContract) -> u32 {
    let pair_hash = B256::repeat_byte(0x11);
    let pair_id = 1u32;
    oracle
        .settlement_iso_to_pair
        .write(&QUALIFIER_REFERENCE_ISO, pair_hash)
        .unwrap();
    oracle.pair_hash_to_id.write(&pair_hash, pair_id).unwrap();
    pair_id
}

fn set_vwap(oracle: &OracleContract, wwd: WorldwideDay, pair_id: u32, value: U256) {
    oracle.worldwide_day_vwap_exists.write(&wwd, true).unwrap();
    oracle.worldwide_day_vwap_pair_count.write(&wwd, 1).unwrap();
    oracle
        .worldwide_day_vwap_pair_id
        .get_nested(&wwd)
        .write(&0, pair_id)
        .unwrap();
    oracle
        .worldwide_day_vwap_value
        .get_nested(&wwd)
        .write(&0, value)
        .unwrap();
}

/// Set `days` consecutive completed days ending at `today` to `value`.
fn fill_days(oracle: &OracleContract, today: WorldwideDay, pair_id: u32, days: u32, value: U256) {
    let mut d = today;
    for _ in 0..days {
        set_vwap(oracle, d, pair_id, value);
        d = d.previous_date_key();
    }
}

#[test]
fn qualify_enrolls_in_call_trigger_bin() {
    with_factory(|s| {
        let f = qualify_series(&s, 7, sample(7));
        // Moved out of the floor index, into the call-trigger index.
        let floor_bin = IntexFactoryContract::price_to_bin(U256::from(EXPECTED_FLOOR)).unwrap();
        let trig_bin = IntexFactoryContract::price_to_bin(U256::from(EXPECTED_TRIGGER)).unwrap();
        assert_eq!(f.unqualified_bin_count.read(&floor_bin).unwrap(), 0);
        assert_eq!(f.qualified_bin_count.read(&trig_bin).unwrap(), 1);
    });
}

#[test]
fn insert_remove_qualified_roundtrip() {
    with_factory(|s| {
        let mut f = IntexFactoryContract::new(s.clone());
        let trigger = U256::from(EXPECTED_TRIGGER);
        let bin = IntexFactoryContract::price_to_bin(trigger).unwrap();
        f.insert_qualified(11, trigger).unwrap();
        f.insert_qualified(22, trigger).unwrap();
        assert_eq!(f.qualified_bin_count.read(&bin).unwrap(), 2);
        f.remove_qualified(11, trigger).unwrap();
        assert_eq!(f.qualified_bin_count.read(&bin).unwrap(), 1);
        f.remove_qualified(22, trigger).unwrap();
        assert_eq!(f.qualified_bin_count.read(&bin).unwrap(), 0);
    });
}

#[test]
fn try_call_marks_called_when_threshold_met() {
    with_factory(|s| {
        let mut f = qualify_series(&s, 7, sample(7));
        let oracle = OracleContract::new(s.clone());
        let pair_id = setup_pair(&oracle);
        // All 30 window days above the trigger (threshold is 20).
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let today = WorldwideDay::from_timestamp(scan_ts).previous_date_key();
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        fill_days(&oracle, today, pair_id, 30, breach);

        assert!(called::try_call(&s, &mut f, &oracle, 7, pair_id, today, scan_ts).unwrap());
        assert_eq!(
            outbe_intex::api::read_series(&s, 7)
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Called
        );
        let bin = IntexFactoryContract::price_to_bin(U256::from(EXPECTED_TRIGGER)).unwrap();
        assert_eq!(f.qualified_bin_count.read(&bin).unwrap(), 0);
    });
}

#[test]
fn try_call_skips_when_below_threshold() {
    with_factory(|s| {
        let mut f = qualify_series(&s, 7, sample(7));
        let oracle = OracleContract::new(s.clone());
        let pair_id = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let today = WorldwideDay::from_timestamp(scan_ts).previous_date_key();
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        let calm = U256::from(EXPECTED_TRIGGER); // equal: strict `>` is not a breach
                                                 // 19 breach days + 11 calm days; threshold is 20.
        let mut d = today;
        for _ in 0..19 {
            set_vwap(&oracle, d, pair_id, breach);
            d = d.previous_date_key();
        }
        for _ in 0..11 {
            set_vwap(&oracle, d, pair_id, calm);
            d = d.previous_date_key();
        }

        assert!(!called::try_call(&s, &mut f, &oracle, 7, pair_id, today, scan_ts).unwrap());
        assert_eq!(
            outbe_intex::api::read_series(&s, 7)
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
        let mut p = sample(8);
        p.call_threshold_days = 27;
        let mut f = qualify_series(&s, 8, p);
        let oracle = OracleContract::new(s.clone());
        let pair_id = setup_pair(&oracle);
        // Scan only ~23 days after issuance, but set all 30 window days as
        // breaches: the ~7 pre-issuance days must not count, so 23 < 27.
        let scan_ts = ISSUED_AT as u64 + 23 * DAY;
        let today = WorldwideDay::from_timestamp(scan_ts).previous_date_key();
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        fill_days(&oracle, today, pair_id, 30, breach);

        assert!(!called::try_call(&s, &mut f, &oracle, 8, pair_id, today, scan_ts).unwrap());
        assert_eq!(
            outbe_intex::api::read_series(&s, 8)
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Qualified
        );
    });
}

// ---------------------------------------------------------------------
// LZ notification failure: state transition must survive messenger outage
// ---------------------------------------------------------------------

/// Seed a series directly in the registry + bin index, bypassing issue()
/// so tests can omit the OriginMessenger stub.
fn seed_issued(s: &StorageHandle<'_>, id: u32) {
    outbe_intex::api::create_series(
        s,
        outbe_intex::CreateSeriesParams {
            series_id: id,
            issued_intex_count: 100,
            promis_load_minor: PROMIS_LOAD_MINOR,
            cost_amount_minor: 2_000,
            floor_price_minor: U256::from(EXPECTED_FLOOR),
            intex_call_period: CALL_PERIOD,
            call_trigger: outbe_intex::IntexCallTrigger {
                window_days: 30,
                threshold_days: 21,
                call_price_minor: U256::from(EXPECTED_TRIGGER),
            },
            issued_at: ISSUED_AT,
            issuance_currency: 840,
            reference_currency: 840,
        },
    )
    .unwrap();
    IntexFactoryContract::new(s.clone())
        .insert_unqualified(id, U256::from(EXPECTED_FLOOR))
        .unwrap();
}

#[test]
fn qualify_survives_lz_messenger_failure() {
    // No OriginMessenger stub: notify_lz_qualified fails silently.
    // The Issued -> Qualified transition must still complete.
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    StorageHandle::enter(&mut storage, |s| {
        seed_issued(&s, 7);
        let mut f = IntexFactoryContract::new(s.clone());
        let mature = ISSUED_AT as u64 + 21 * DAY + 1;
        assert!(qualified::try_qualify(
            &s,
            &mut f,
            7,
            mature,
            U256::from(EXPECTED_FLOOR) + U256::from(1)
        )
        .unwrap());
        assert_eq!(
            outbe_intex::api::read_series(&s, 7)
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Qualified
        );
    });
}

#[test]
fn call_survives_lz_messenger_failure() {
    // No OriginMessenger stub: notify_lz_called fails silently.
    // The Qualified -> Called transition must still complete.
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    StorageHandle::enter(&mut storage, |s| {
        seed_issued(&s, 7);
        outbe_intex::api::mark_qualified(&s, 7).unwrap();
        let mut f = IntexFactoryContract::new(s.clone());
        f.insert_qualified(7, U256::from(EXPECTED_TRIGGER)).unwrap();

        let oracle = OracleContract::new(s.clone());
        let pair_id = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let today = WorldwideDay::from_timestamp(scan_ts).previous_date_key();
        fill_days(
            &oracle,
            today,
            pair_id,
            30,
            U256::from(EXPECTED_TRIGGER) + U256::from(1),
        );

        assert!(called::try_call(&s, &mut f, &oracle, 7, pair_id, today, scan_ts).unwrap());
        assert_eq!(
            outbe_intex::api::read_series(&s, 7)
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
fn scan_and_qualify_promotes_matured_series() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        // Qualifier pair live rate above the floor.
        let oracle = OracleContract::new(s.clone());
        let pair_hash = B256::repeat_byte(0x11);
        oracle
            .settlement_iso_to_pair
            .write(&QUALIFIER_REFERENCE_ISO, pair_hash)
            .unwrap();
        oracle
            .exchange_rate
            .write(&pair_hash, U256::from(EXPECTED_FLOOR) + U256::from(1))
            .unwrap();

        let mature_ts = ISSUED_AT as u64 + 21 * DAY + 1;
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, mature_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(qualified::scan_and_qualify(&ctx).unwrap(), 1);

        let r = outbe_intex::api::read_series(&s, 7).unwrap();
        assert_eq!(
            r.lifecycle_state().unwrap(),
            outbe_intex::IntexState::Qualified
        );
        let f = IntexFactoryContract::new(s.clone());
        let trig_bin = IntexFactoryContract::price_to_bin(U256::from(EXPECTED_TRIGGER)).unwrap();
        assert_eq!(f.qualified_bin_count.read(&trig_bin).unwrap(), 1);
    });
}

#[test]
fn scan_and_call_force_calls_breached_series() {
    with_factory(|s| {
        let _f = qualify_series(&s, 7, sample(7));
        let oracle = OracleContract::new(s.clone());
        let pair_id = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let today = WorldwideDay::from_timestamp(scan_ts).previous_date_key();
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        fill_days(&oracle, today, pair_id, 30, breach);

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(called::scan_and_call(&ctx).unwrap(), 1);
        assert_eq!(
            outbe_intex::api::read_series(&s, 7)
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Called
        );
    });
}
