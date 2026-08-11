use alloy_primitives::{address, keccak256, Address, U256};
use alloy_sol_types::SolCall;
use outbe_oracle::api::AddressPair;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::addresses::INTEX_FACTORY_ADDRESS;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::{date_key_to_utc_timestamp, previous_date_key, timestamp_to_date_key};

use crate::called;
use crate::constants::{CALL_RATE, FLOOR_RATE, QUALIFICATION_PERIOD, QUALIFIER_REFERENCE_ISO};
use crate::precompile::{self, IIntexFactory};
use crate::qualified;
use crate::runtime;
use crate::schema::{IntexFactoryContract, IssuanceParams};

const DAY: u64 = 24 * 60 * 60;

fn holder() -> Address {
    address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
}

fn payment_token() -> Address {
    address!("0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC")
}

const CHAIN_ID: u64 = 1;
const ISSUED_AT: u32 = 1_700_000_000;
const PROMIS_LOAD_MINOR: u128 = 1_000_000_000_000_000_000; // 1e18
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

fn sample(worldwide_day: u32) -> IssuanceParams {
    IssuanceParams {
        series_id: worldwide_day,
        worldwide_day,
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
        let r = outbe_intex::api::read_series(&s, 7).unwrap();
        assert_eq!(r.series_id, 7);
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
fn issue_zero_winners_discards_contributor_map() {
    with_factory(|s| {
        // Lysis recorded contributors for the day, but the clearing had no winners.
        outbe_intex::api::record_contributors(&s, 7, &[(holder(), U256::from(100u64))]).unwrap();
        let mut p = sample(7);
        p.issued_intex_count = 0;
        runtime::issue(&s, p).unwrap();

        // No series exists and the never-to-distribute map is discarded.
        assert!(!outbe_intex::api::series_exists(&s, 7).unwrap());
        assert_eq!(outbe_intex::api::contributor_count(&s, 7).unwrap(), 0);
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
        assert_eq!(outbe_intex::api::series_id_at(&s, 0).unwrap(), 11);
        assert_eq!(outbe_intex::api::series_id_at(&s, 1).unwrap(), 22);
    });
}

#[test]
fn floor_and_call_derivation() {
    let floor = runtime::marked_up(U256::from(ENTRY_PRICE), FLOOR_RATE).unwrap();
    let call = runtime::marked_up(U256::from(ENTRY_PRICE), CALL_RATE).unwrap();
    assert_eq!(floor, U256::from(EXPECTED_FLOOR));
    assert_eq!(call, U256::from(EXPECTED_TRIGGER));

    let one = U256::from(1_000_000_000_000_000_000u64);
    assert_eq!(
        runtime::marked_up(one, FLOOR_RATE).unwrap(),
        U256::from(1_080_000_000_000_000_000u64)
    );
    assert_eq!(
        runtime::marked_up(one, CALL_RATE).unwrap(),
        U256::from(2_280_000_000_000_000_000u64)
    );
}

// ---------------------------------------------------------------------
// settlement cost scaling
// ---------------------------------------------------------------------

fn entry_price() -> U256 {
    U256::from(5u64) * U256::from(10u64).pow(U256::from(17u64))
}

fn load_minor() -> U256 {
    U256::from(100_000u64) * U256::from(10u64).pow(U256::from(18u64))
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
fn cost_amount_rejects_decimals_above_the_product_scale() {
    let err = runtime::derived_cost_amount(entry_price(), load_minor(), 37).unwrap_err();
    assert!(err.to_string().contains("unsupported decimals"), "{err}");
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
    with_payment_token(1, 840, 18, |s| {
        let cost = runtime::quote_cost_amount(&s, 7, payment_token()).unwrap();
        assert_eq!(cost, U256::from(1_000_000u64));
    });
}

#[test]
fn cost_amount_rejects_an_unregistered_token() {
    with_payment_token(0, 840, 18, |s| {
        let err = runtime::quote_cost_amount(&s, 7, payment_token()).unwrap_err();
        assert!(err.to_string().contains("no registered vault"), "{err}");
    });
}

#[test]
fn cost_amount_rejects_a_foreign_currency() {
    with_payment_token(1, 978, 18, |s| {
        let err = runtime::quote_cost_amount(&s, 7, payment_token()).unwrap_err();
        assert!(err.to_string().contains("does not match"), "{err}");
    });
}

#[test]
fn cost_amount_rejects_missing_series() {
    with_factory(|s| {
        assert!(runtime::quote_cost_amount(&s, 7, payment_token()).is_err());
    });
}

#[test]
fn cost_amount_dispatch() {
    with_payment_token(1, 840, 18, |s| {
        let out = precompile::dispatch(
            s.clone(),
            &IIntexFactory::quoteCostAmountCall {
                seriesId: 7,
                paymentToken: payment_token(),
            }
            .abi_encode(),
            holder(),
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            IIntexFactory::quoteCostAmountCall::abi_decode_returns(&out).unwrap(),
            U256::from(1_000_000u64)
        );
    });
}

// ---------------------------------------------------------------------
// settle gating (value movement is localnet-exercised, not unit tested)
// ---------------------------------------------------------------------

#[test]
fn settle_rejects_zero_amount() {
    with_factory(|s| {
        assert!(runtime::settle(&s, 7, holder(), holder(), U256::ZERO, payment_token()).is_err());
    });
}

#[test]
fn settle_rejects_missing_series() {
    with_factory(|s| {
        assert!(
            runtime::settle(&s, 7, holder(), holder(), U256::from(1), payment_token()).is_err()
        );
    });
}

#[test]
fn settle_rejects_wrong_state_issued() {
    with_factory(|s| {
        // Born Issued; settlement is only valid in Qualified/Called.
        runtime::issue(&s, sample(7)).unwrap();
        let err =
            runtime::settle(&s, 7, holder(), holder(), U256::from(1), payment_token()).unwrap_err();
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
        outbe_intex::api::mark_called(&s, 7, ISSUED_AT).unwrap();
        let err =
            runtime::settle(&s, 7, holder(), holder(), U256::from(1), payment_token()).unwrap_err();
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
        assert!(runtime::mine_promis(&s, 7, holder(), U256::ZERO, U256::ZERO, no_auth()).is_err());
    });
}

#[test]
fn mine_promis_rejects_missing_series() {
    with_factory(|s| {
        assert!(
            runtime::mine_promis(&s, 7, holder(), U256::from(1), U256::ZERO, no_auth()).is_err()
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
fn try_qualify_gates_qualification_floor_and_latches() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        let mut f = IntexFactoryContract::new(s.clone());
        let floor = U256::from(EXPECTED_FLOOR);
        let immature = ISSUED_AT as u64 + 10;
        let mature = ISSUED_AT as u64 + 21 * DAY + 1;

        // Immature -> false.
        assert!(!qualified::try_qualify(
            &s,
            &mut f,
            7,
            QUALIFICATION_PERIOD,
            immature,
            floor + U256::from(1)
        )
        .unwrap());
        // Mature but rate == floor (strict >) -> false.
        assert!(
            !qualified::try_qualify(&s, &mut f, 7, QUALIFICATION_PERIOD, mature, floor).unwrap()
        );
        // Mature + rate > floor -> qualifies, latched, removed from bin.
        assert!(qualified::try_qualify(
            &s,
            &mut f,
            7,
            QUALIFICATION_PERIOD,
            mature,
            floor + U256::from(1)
        )
        .unwrap());
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
        assert!(!qualified::try_qualify(
            &s,
            &mut f,
            7,
            QUALIFICATION_PERIOD,
            mature,
            floor + U256::from(1)
        )
        .unwrap());
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
    assert!(qualified::try_qualify(
        s,
        &mut f,
        id,
        QUALIFICATION_PERIOD,
        mature,
        floor + U256::from(1)
    )
    .unwrap());
    f
}

/// Registry index the fixtures register the qualifier pair at; the rate
/// columns are keyed by it.
const PAIR_ID: u32 = 1;

fn setup_pair(oracle: &OracleContract) -> AddressPair {
    let pair = outbe_oracle::api::AddressPair::new_coen_to(QUALIFIER_REFERENCE_ISO);
    let pair_id = PAIR_ID;
    oracle.pair_to_index.write(&pair, pair_id).unwrap();
    // Full registry entry so the production VWAP paths (calculate_vwaps
    // iterating registered vote-target pairs) see the pair too. `pair_at` reads
    // the reverse column.
    oracle.pair_count.write(pair_id).unwrap();
    oracle.pair_by_index.write_pair(&pair_id, pair).unwrap();
    oracle.vote_target.write(&pair, true).unwrap();
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
        let pair = setup_pair(&oracle);
        // All 30 window days above the trigger (threshold is 21).
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts));
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        fill_days(&oracle, last_closed_day, pair, 30, breach);

        assert!(called::try_call(
            &s,
            &mut f,
            &oracle,
            &mut called::DayVwaps::new(oracle.pair_index_of(pair).unwrap()),
            7,
            last_closed_day,
            scan_ts
        )
        .unwrap());
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

        assert!(!called::try_call(
            &s,
            &mut f,
            &oracle,
            &mut called::DayVwaps::new(oracle.pair_index_of(pair).unwrap()),
            7,
            last_closed_day,
            scan_ts
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
fn try_call_excludes_pre_issuance_days() {
    with_factory(|s| {
        // window 30, threshold 27: only days from issuance onward may count.
        // Seed the series directly with threshold 27 (above the 21d qualification
        // period), since the protocol default (21) does not exceed it and a
        // qualified series would always have >= 21 completed post-issuance days.
        outbe_intex::api::create_series(
            &s,
            outbe_intex::CreateSeriesParams {
                series_id: 8,
                worldwide_day: 8,
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
        outbe_intex::api::mark_qualified(&s, 8).unwrap();
        let mut f = IntexFactoryContract::new(s.clone());
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        // Scan only ~23 days after issuance, but set all 30 window days as
        // breaches: the ~7 pre-issuance days must not count, so 23 < 27.
        let scan_ts = ISSUED_AT as u64 + 23 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts));
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        fill_days(&oracle, last_closed_day, pair, 30, breach);

        assert!(!called::try_call(
            &s,
            &mut f,
            &oracle,
            &mut called::DayVwaps::new(oracle.pair_index_of(pair).unwrap()),
            8,
            last_closed_day,
            scan_ts
        )
        .unwrap());
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
// Cross-chain notification failure: state transition must survive router outage
// ---------------------------------------------------------------------

/// Seed a series directly in the registry + bin index, bypassing issue()
/// so tests can omit the OriginRouter stub.
fn seed_issued(s: &StorageHandle<'_>, id: u32) {
    outbe_intex::api::create_series(
        s,
        outbe_intex::CreateSeriesParams {
            series_id: id,
            worldwide_day: id,
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
        .insert_unqualified(id, U256::from(EXPECTED_FLOOR))
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
        assert!(qualified::try_qualify(
            &s,
            &mut f,
            7,
            QUALIFICATION_PERIOD,
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
        outbe_intex::api::mark_qualified(&s, 7).unwrap();
        let mut f = IntexFactoryContract::new(s.clone());
        f.insert_qualified(7, U256::from(EXPECTED_TRIGGER)).unwrap();

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

        assert!(called::try_call(
            &s,
            &mut f,
            &oracle,
            &mut called::DayVwaps::new(oracle.pair_index_of(pair).unwrap()),
            7,
            last_closed_day,
            scan_ts
        )
        .unwrap());
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
fn scan_and_qualify_promotes_aged_series() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        // Qualifier pair live rate above the floor.
        let oracle = OracleContract::new(s.clone());
        let pair = outbe_oracle::api::AddressPair::new_coen_to(QUALIFIER_REFERENCE_ISO);
        // The ISO resolves through the pair registry, so the pair must exist
        // and the rate columns are keyed by its index.
        oracle.pair_to_index.write(&pair, PAIR_ID).unwrap();
        oracle
            .exchange_rate
            .write(&PAIR_ID, U256::from(EXPECTED_FLOOR) + U256::from(1))
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
            outbe_intex::api::read_series(&s, 7)
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
                        outbe_oracle::api::AddressPair::new_coen_to(QUALIFIER_REFERENCE_ISO),
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
            outbe_intex::api::read_series(&s, 7)
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
        let pair = outbe_oracle::api::AddressPair::new_coen_to(QUALIFIER_REFERENCE_ISO);
        oracle.pair_to_index.write(&pair, PAIR_ID).unwrap();
        // Out-of-range rate: price_to_bin overflows.
        oracle.exchange_rate.write(&PAIR_ID, U256::MAX).unwrap();

        let mature_ts = ISSUED_AT as u64 + 21 * DAY + 1;
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, mature_ts, CHAIN_ID),
            s.clone(),
        );
        // Must not halt: returns Ok(0) and leaves the series untouched.
        assert_eq!(qualified::scan_and_qualify(&ctx).unwrap(), 0);
        assert_eq!(
            outbe_intex::api::read_series(&s, 7)
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
            .insert_unqualified(999, U256::from(EXPECTED_FLOOR))
            .unwrap();

        let oracle = OracleContract::new(s.clone());
        let pair = outbe_oracle::api::AddressPair::new_coen_to(QUALIFIER_REFERENCE_ISO);
        // The ISO resolves through the pair registry, so the pair must exist
        // and the rate columns are keyed by its index.
        oracle.pair_to_index.write(&pair, PAIR_ID).unwrap();
        oracle
            .exchange_rate
            .write(&PAIR_ID, U256::from(EXPECTED_FLOOR) + U256::from(1))
            .unwrap();

        let mature_ts = ISSUED_AT as u64 + 21 * DAY + 1;
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, mature_ts, CHAIN_ID),
            s.clone(),
        );
        // Bad series (999) skipped; healthy series (7) still qualifies.
        assert_eq!(qualified::scan_and_qualify(&ctx).unwrap(), 1);
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
            outbe_intex::api::read_series(&s, 7)
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
        let pair = outbe_oracle::api::AddressPair::new_coen_to(QUALIFIER_REFERENCE_ISO);
        // The ISO resolves through the pair registry, so the pair must exist
        // and the rate columns are keyed by its index.
        oracle.pair_to_index.write(&pair, PAIR_ID).unwrap();
        // Rate well above both floors so both bins are eligible.
        oracle
            .exchange_rate
            .write(&PAIR_ID, U256::from(EXPECTED_FLOOR) * U256::from(1000))
            .unwrap();

        // Two distinct bins: the first holds exactly MAX_SERIES_PER_BLOCK entries, the second a few.
        // Bogus ids (no series record) are per-series skipped but still count toward the cap.
        let cap = qualified::MAX_SERIES_PER_BLOCK;
        let f1 = U256::from(EXPECTED_FLOOR);
        let f2 = U256::from(EXPECTED_FLOOR) * U256::from(4);
        {
            let mut factory = IntexFactoryContract::new(s.clone());
            for id in 1..=cap {
                factory.insert_unqualified(id, f1).unwrap();
            }
            for id in 1001..=1005u32 {
                factory.insert_unqualified(id, f2).unwrap();
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
            .read()
            .unwrap();
        assert!(cursor1 > 0, "cursor advanced past the capped bin");
        assert_eq!(
            IntexFactoryContract::new(s.clone())
                .unqualified_bin_count
                .read(&bin2)
                .unwrap(),
            5,
            "second bin untouched in block 1"
        );

        // Block 2 resumes at the second bin and wraps the cursor to 0.
        qualified::scan_and_qualify(&ctx).unwrap();
        let cursor2 = IntexFactoryContract::new(s.clone())
            .qualify_scan_cursor
            .read()
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
        let r = outbe_intex::api::read_series(&s, 7).unwrap();
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
        assert!(qualified::try_qualify(
            &s,
            &mut f,
            7,
            dev.qualification_period,
            after_qualification,
            rate
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
            7,
            &[
                (owners[0], U256::from(100u64)),
                (owners[1], U256::from(200u64)),
                (owners[2], U256::from(300u64)),
            ],
        )
        .unwrap();
        // A single winning chain: its arrival completes the fan-in immediately.
        outbe_intex::api::arm_proceeds(&s, 7, &[10], DEADLINE_FUTURE).unwrap();
        // Simulate the native value arriving on the precompile via distribute{value}.
        let amount = U256::from(1000u64);
        s.increase_balance(INTEX_FACTORY_ADDRESS, amount).unwrap();

        runtime::distribute(&s, crate::constants::ORIGIN_ROUTER_ADDRESS, 7, 10, amount).unwrap();

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
        assert_eq!(outbe_intex::api::get_progress(&s, 7).unwrap(), None);
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 0);
        assert_eq!(outbe_intex::api::contributor_count(&s, 7).unwrap(), 0);
    });
}

#[test]
fn distribute_waits_for_all_winning_chains_then_pays_the_sum() {
    with_factory(|s| {
        let owners = [contrib(1), contrib(2)];
        outbe_intex::api::record_contributors(
            &s,
            7,
            &[
                (owners[0], U256::from(100u64)),
                (owners[1], U256::from(100u64)),
            ],
        )
        .unwrap();
        outbe_intex::api::arm_proceeds(&s, 7, &[10, 20], DEADLINE_FUTURE).unwrap();

        // Chain 10 arrives first: pot accumulates, fan-in not complete → no payout yet.
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(300u64))
            .unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7,
            10,
            U256::from(300u64),
        )
        .unwrap();
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 0);
        assert!(!outbe_intex::api::proceeds_ready(&s, 7).unwrap());

        // Chain 20 completes the fan-in: one distribution over the summed pot.
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(500u64))
            .unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7,
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
        assert_eq!(outbe_intex::api::contributor_count(&s, 7).unwrap(), 0);
    });
}

#[test]
fn distribute_deadline_forces_partial_payout_then_late_chain_supplements() {
    with_factory(|s| {
        let owners = [contrib(1), contrib(2)];
        outbe_intex::api::record_contributors(
            &s,
            7,
            &[
                (owners[0], U256::from(100u64)),
                (owners[1], U256::from(100u64)),
            ],
        )
        .unwrap();
        outbe_intex::api::arm_proceeds(&s, 7, &[10, 20], DEADLINE_FUTURE).unwrap();

        // Only chain 10 arrives before the deadline.
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(200u64))
            .unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7,
            10,
            U256::from(200u64),
        )
        .unwrap();
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 0);

        // Past the deadline the sweep pays out what arrived; the map is retained.
        runtime::try_settle_proceeds(&s, 7, DEADLINE_FUTURE + 1).unwrap();
        assert!(!outbe_intex::api::proceeds_finalize_on_done(&s, 7).unwrap());
        runtime::drain_distributions(&s).unwrap();
        assert_eq!(s.balance(owners[0]).unwrap(), U256::from(100u64));
        assert_eq!(s.balance(owners[1]).unwrap(), U256::from(100u64));
        assert_eq!(outbe_intex::api::contributor_count(&s, 7).unwrap(), 2); // retained

        // The straggler arrives: a supplementary payout over the same map, then finalize.
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(400u64))
            .unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7,
            20,
            U256::from(400u64),
        )
        .unwrap();
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);
        runtime::drain_distributions(&s).unwrap();
        assert_eq!(s.balance(owners[0]).unwrap(), U256::from(300u64)); // +200
        assert_eq!(s.balance(owners[1]).unwrap(), U256::from(300u64));
        assert_eq!(outbe_intex::api::contributor_count(&s, 7).unwrap(), 0); // finalized
    });
}

#[test]
fn late_top_up_during_final_round_reaches_creators() {
    use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;
    with_factory(|s| {
        let owners = [contrib(1), contrib(2)];
        outbe_intex::api::record_contributors(
            &s,
            7,
            &[
                (owners[0], U256::from(100u64)),
                (owners[1], U256::from(100u64)),
            ],
        )
        .unwrap();
        // A single winning chain: the first arrival completes the fan-in, so the
        // round runs finalize-on-done (its end clears the contributor map).
        outbe_intex::api::arm_proceeds(&s, 7, &[10], DEADLINE_FUTURE).unwrap();
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(200u64))
            .unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7,
            10,
            U256::from(200u64),
        )
        .unwrap();
        assert!(outbe_intex::api::proceeds_finalize_on_done(&s, 7).unwrap());

        // Pay only the first contributor: the round is mid-drain, still active.
        runtime::pay_chunk(&s, 7, 1).unwrap();
        assert_eq!(s.balance(owners[0]).unwrap(), U256::from(100u64));
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);

        // Chain 10 sends the rest of its proceeds while the round still drains: it
        // only tops the pot up (an in-flight round is never overlapped).
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(400u64))
            .unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7,
            10,
            U256::from(400u64),
        )
        .unwrap();
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);

        // Finishing the round finds the topped-up pot and opens a supplementary
        // round over the same map instead of finalizing (which would strand it).
        runtime::pay_chunk(&s, 7, 1).unwrap();
        assert_eq!(s.balance(owners[1]).unwrap(), U256::from(100u64));
        assert_eq!(outbe_intex::api::contributor_count(&s, 7).unwrap(), 2); // retained
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);

        // The supplementary round pays the top-up to the creators, then finalizes.
        runtime::drain_distributions(&s).unwrap();
        assert_eq!(s.balance(owners[0]).unwrap(), U256::from(300u64)); // +200
        assert_eq!(s.balance(owners[1]).unwrap(), U256::from(300u64)); // +200
        assert_eq!(outbe_intex::api::contributor_count(&s, 7).unwrap(), 0); // finalized
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
            7,
            &[
                (owners[0], U256::from(100u64)),
                (owners[1], U256::from(200u64)),
                (owners[2], U256::from(300u64)),
            ],
        )
        .unwrap();
        let amount = U256::from(600u64);
        s.increase_balance(INTEX_FACTORY_ADDRESS, amount).unwrap();
        outbe_intex::api::start_distribution(&s, 7, amount, U256::from(600u64)).unwrap();

        // Chunk 1 (limit 2): pays the first two, cursor advances, still active.
        runtime::pay_chunk(&s, 7, 2).unwrap();
        assert_eq!(s.balance(owners[0]).unwrap(), U256::from(100u64));
        assert_eq!(s.balance(owners[1]).unwrap(), U256::from(200u64));
        assert_eq!(s.balance(owners[2]).unwrap(), U256::ZERO);
        assert_eq!(
            outbe_intex::api::get_progress(&s, 7)
                .unwrap()
                .unwrap()
                .cursor,
            2
        );
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);

        // Chunk 2: pays the last and finalizes.
        runtime::pay_chunk(&s, 7, 2).unwrap();
        assert_eq!(s.balance(owners[2]).unwrap(), U256::from(300u64));
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(outbe_intex::api::get_progress(&s, 7).unwrap(), None);
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 0);
    });
}

#[test]
fn distribute_rejects_non_origin_router() {
    with_factory(|s| {
        outbe_intex::api::record_contributors(&s, 7, &[(contrib(1), U256::from(100u64))]).unwrap();
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(100u64))
            .unwrap();
        let err = runtime::distribute(&s, holder(), 7, 10, U256::from(100u64)).unwrap_err();
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
        outbe_intex::api::arm_proceeds(&s, 7, &[10], DEADLINE_FUTURE).unwrap();
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(100u64))
            .unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7,
            10,
            U256::from(100u64),
        )
        .unwrap();

        // A day without contributor authority holds its pot until the window
        // closes, in case a certified root is still landing.
        assert_eq!(
            s.balance(INTEX_FACTORY_ADDRESS).unwrap(),
            U256::from(100u64)
        );
        runtime::sweep_proceeds_deadlines(&s, DEADLINE_FUTURE).unwrap();

        // No distribution opened; the ownerless proceeds were destroyed, not vaulted.
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 0);
        assert_eq!(s.balance(VAULT_ROUTER_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
    });

    let sig = IIntexFactory::ProceedsBurned::SIGNATURE_HASH;
    let found = storage.get_events(INTEX_FACTORY_ADDRESS).iter().any(|log| {
        log.topics().first() == Some(&sig)
            && IIntexFactory::ProceedsBurned::decode_log_data(log)
                .map(|ev| ev.seriesId == 7 && ev.amount == U256::from(100u64))
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
                sid,
                &[
                    (owners[0], U256::from(100u64)),
                    (owners[1], U256::from(200u64)),
                    (owners[2], U256::from(300u64)),
                ],
            )
            .unwrap();
            s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(600u64))
                .unwrap();
            outbe_intex::api::start_distribution(&s, sid, U256::from(600u64), U256::from(600u64))
                .unwrap();
            // Pay only the first contributor, leaving the series active.
            runtime::pay_chunk(&s, sid, 1).unwrap();
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
        outbe_intex::api::record_contributors(&s, 7, &[(contrib(1), U256::from(100u64))]).unwrap();
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(100u64))
            .unwrap();
        outbe_intex::api::start_distribution(&s, 7, U256::from(100u64), U256::from(100u64))
            .unwrap();

        // Series 9 is unfunded: its first transfer fails mid-chunk.
        outbe_intex::api::record_contributors(
            &s,
            9,
            &[
                (contrib(2), U256::from(100u64)),
                (contrib(3), U256::from(500u64)),
            ],
        )
        .unwrap();
        outbe_intex::api::start_distribution(&s, 9, U256::from(600u64), U256::from(600u64))
            .unwrap();

        // The drain must not error: the failing series is skipped and rolled back.
        runtime::drain_distributions(&s).unwrap();

        assert_eq!(s.balance(contrib(1)).unwrap(), U256::from(100u64));
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);
        let p = outbe_intex::api::get_progress(&s, 9).unwrap().unwrap();
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
            seriesId: 0u32,
            intexHolder: Address::ZERO,
            amount: U256::ZERO,
            paymentToken: Address::ZERO,
        }
        .abi_encode(),
        IIntexFactory::setAuthorizedSettlerCall {
            seriesId: 0u32,
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

use alloy_primitives::FixedBytes;

use outbe_intex::payout::test_support::{
    contributor_leaf, contributor_range_proof, contributor_root, metadata_word,
};
use outbe_intex::payout::ContributorLeafData;

use crate::constants::ORIGIN_ROUTER_ADDRESS;

const WWD: u32 = 20_260_725;
const CHAIN: u32 = 10;
fn population(count: u32) -> Vec<ContributorLeafData> {
    (0..count)
        .map(|i| contributor_leaf(i, u64::from(i) + 1))
        .collect()
}

fn nominal_total(leaves: &[ContributorLeafData]) -> U256 {
    leaves
        .iter()
        .fold(U256::ZERO, |acc, leaf| acc + leaf.nominal)
}

/// Writes the constant-size authority that OCOMP activation installs.
fn install_generation(storage: &StorageHandle<'_>, leaves: &[ContributorLeafData]) {
    install_generation_with_total(storage, leaves, nominal_total(leaves));
}

fn install_generation_with_total(
    storage: &StorageHandle<'_>,
    leaves: &[ContributorLeafData],
    eligible_nominal_total: U256,
) {
    let count = u32::try_from(leaves.len()).expect("count fits u32");
    let registry = outbe_intex::IntexContract::new(storage.clone());
    registry
        .ocomp_contributor_root
        .write(&WWD, contributor_root(leaves))
        .unwrap();
    registry
        .ocomp_eligible_nominal_total
        .write(&WWD, eligible_nominal_total)
        .unwrap();
    registry
        .ocomp_contributor_metadata
        .write(&WWD, metadata_word(1, count))
        .unwrap();
}

/// Installs an empty certified generation (a day whose contributors were all
/// excluded), which has a root but no payable leaves.
fn install_empty_generation(storage: &StorageHandle<'_>) {
    let registry = outbe_intex::IntexContract::new(storage.clone());
    registry
        .ocomp_contributor_root
        .write(&WWD, contributor_root(&[]))
        .unwrap();
    registry
        .ocomp_eligible_nominal_total
        .write(&WWD, U256::ZERO)
        .unwrap();
    registry
        .ocomp_contributor_metadata
        .write(&WWD, metadata_word(1, 0))
        .unwrap();
}

fn abi_leaves(leaves: &[ContributorLeafData]) -> Vec<IIntexFactory::ContributorLeaf> {
    leaves
        .iter()
        .map(|leaf| IIntexFactory::ContributorLeaf {
            owner: leaf.owner,
            sourceTributeDigest: B256::from_slice(&leaf.source_tribute_id[..32]),
            sourceTributeIndex: FixedBytes::<4>::from_slice(&leaf.source_tribute_id[32..]),
            nominal: leaf.nominal,
        })
        .collect()
}

/// Arms the fan-in and delivers the whole pot from one winning chain.
fn deliver_proceeds(storage: &StorageHandle<'_>, amount: U256) {
    outbe_intex::api::arm_proceeds(storage, WWD, &[CHAIN], DEADLINE_FUTURE).unwrap();
    storage
        .increase_balance(INTEX_FACTORY_ADDRESS, amount)
        .unwrap();
    runtime::distribute(storage, ORIGIN_ROUTER_ADDRESS, WWD, CHAIN, amount).unwrap();
}

#[test]
fn certified_day_opens_a_payout_round_instead_of_burning_the_pot() {
    with_factory(|s| {
        let leaves = population(300);
        install_generation(&s, &leaves);
        let amount = U256::from(1_000u64);

        deliver_proceeds(&s, amount);

        let round = outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .expect("certified day must open a payout round");
        assert_eq!(round.amount, amount);
        // The pot stays on the precompile until batches draw it down; the legacy
        // cursor path must not have claimed the day.
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), amount);
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 0);
    });
}

#[test]
fn batches_pay_every_certified_contributor() {
    with_factory(|s| {
        let leaves = population(300);
        install_generation(&s, &leaves);
        let amount = U256::from(1_000_000u64);
        deliver_proceeds(&s, amount);

        for (start, len) in [(0_u32, 256_u32), (256, 44)] {
            runtime::pay_contributor_batch(
                &s,
                WWD,
                start,
                &abi_leaves(&leaves[start as usize..(start + len) as usize]),
                &contributor_range_proof(&leaves, start),
            )
            .unwrap_or_else(|e| panic!("batch at {start} must pay: {e:?}"));
        }

        let total = nominal_total(&leaves);
        for leaf in &leaves {
            let expected = amount * leaf.nominal / total;
            assert_eq!(
                s.balance(leaf.owner).unwrap(),
                expected,
                "owner {:?} share",
                leaf.owner
            );
        }

        let round = outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .unwrap();
        assert_eq!(round.paid_leaf_count, 300);
        // The last batch closed the round and burned what floor division left,
        // so nothing of this day remains on the precompile.
        assert!(round.paid_so_far < amount, "floor shares must fall short");
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
    });
}

#[test]
fn an_unfinished_round_keeps_the_outstanding_shares() {
    with_factory(|s| {
        let leaves = population(300);
        install_generation(&s, &leaves);
        let amount = U256::from(1_000_000u64);
        deliver_proceeds(&s, amount);

        // Only the first chunk is paid: the balance still owes the tail its
        // shares, so nothing may be burned yet.
        runtime::pay_contributor_batch(
            &s,
            WWD,
            0,
            &abi_leaves(&leaves[0..256]),
            &contributor_range_proof(&leaves, 0),
        )
        .unwrap();

        let round = outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .unwrap();
        assert_eq!(round.paid_leaf_count, 256);
        assert_eq!(
            s.balance(INTEX_FACTORY_ADDRESS).unwrap(),
            amount - round.paid_so_far
        );
    });
}

#[test]
fn proceeds_arriving_after_the_round_opened_are_burned() {
    with_factory(|s| {
        let leaves = population(300);
        install_generation(&s, &leaves);
        let amount = U256::from(1_000u64);
        deliver_proceeds(&s, amount);

        // A second chain finally delivers, long after the fan-in window closed.
        let late = U256::from(400u64);
        s.increase_balance(INTEX_FACTORY_ADDRESS, late).unwrap();
        runtime::distribute(&s, ORIGIN_ROUTER_ADDRESS, WWD, CHAIN + 1, late).unwrap();

        // The round still distributes only what it froze, and the late delivery
        // is destroyed rather than left on the balance.
        let round = outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .unwrap();
        assert_eq!(round.amount, amount);
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), amount);
    });
}

#[test]
fn an_understated_nominal_total_cannot_spend_another_days_proceeds() {
    with_factory(|s| {
        let leaves = population(300);
        // The certified total is off-chain input and is never reconciled against
        // the leaves under the root, so a halved one inflates every share.
        install_generation_with_total(&s, &leaves, nominal_total(&leaves) / U256::from(2u64));
        let amount = U256::from(100_000u64);
        deliver_proceeds(&s, amount);

        // Another day's money sits on the same balance and must stay there.
        let other_day = U256::from(1_000_000u64);
        s.increase_balance(INTEX_FACTORY_ADDRESS, other_day)
            .unwrap();

        let err = runtime::pay_contributor_batch(
            &s,
            WWD,
            0,
            &abi_leaves(&leaves[0..256]),
            &contributor_range_proof(&leaves, 0),
        )
        .unwrap_err();

        assert!(
            format!("{err:?}").contains("exceed the round amount"),
            "{err:?}"
        );
        assert_eq!(s.balance(leaves[0].owner).unwrap(), U256::ZERO);
        assert_eq!(
            s.balance(INTEX_FACTORY_ADDRESS).unwrap(),
            amount + other_day
        );
    });
}

#[test]
fn paying_the_same_batch_twice_reverts_before_verification() {
    with_factory(|s| {
        let leaves = population(300);
        install_generation(&s, &leaves);
        deliver_proceeds(&s, U256::from(1_000_000u64));

        let batch = abi_leaves(&leaves[0..256]);
        let proof = contributor_range_proof(&leaves, 0);
        runtime::pay_contributor_batch(&s, WWD, 0, &batch, &proof).unwrap();
        let paid_once = s.balance(leaves[0].owner).unwrap();

        // A garbage proof proves the bitmap gate runs first: the batch is
        // rejected as already paid, never as a bad proof.
        let err = runtime::pay_contributor_batch(&s, WWD, 0, &batch, &[B256::ZERO; 2]).unwrap_err();
        assert!(format!("{err:?}").contains("already paid"), "{err:?}");
        assert_eq!(s.balance(leaves[0].owner).unwrap(), paid_once);
    });
}

#[test]
fn batch_without_an_open_round_is_rejected() {
    with_factory(|s| {
        let leaves = population(300);
        install_generation(&s, &leaves);

        let err = runtime::pay_contributor_batch(
            &s,
            WWD,
            0,
            &abi_leaves(&leaves[0..256]),
            &contributor_range_proof(&leaves, 0),
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("no open certified"), "{err:?}");
    });
}

#[test]
fn certified_day_without_contributors_burns_the_pot() {
    with_factory(|s| {
        install_empty_generation(&s);
        deliver_proceeds(&s, U256::from(1_000u64));

        assert!(outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .is_none());
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
    });
}

#[test]
fn day_without_contributor_authority_holds_the_pot_until_the_deadline() {
    with_factory(|s| {
        let amount = U256::from(1_000u64);
        // Fan-in completes immediately, but no root has landed yet.
        deliver_proceeds(&s, amount);

        // Nothing is burned while the window is still open.
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), amount);
        assert!(outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .is_none());

        // The root arrives late; the preserved pot now funds the round.
        let leaves = population(300);
        install_generation(&s, &leaves);
        runtime::try_settle_proceeds(&s, WWD, DEADLINE_FUTURE - 1).unwrap();

        let round = outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .expect("late root must still open a round");
        assert_eq!(round.amount, amount);
    });
}

#[test]
fn legacy_day_keeps_using_the_cursor_path() {
    with_factory(|s| {
        let owners = [contrib(1), contrib(2)];
        outbe_intex::api::record_contributors(
            &s,
            WWD,
            &[
                (owners[0], U256::from(100u64)),
                (owners[1], U256::from(100u64)),
            ],
        )
        .unwrap();
        deliver_proceeds(&s, U256::from(1_000u64));

        // Legacy authority opens a paginated distribution, not a certified round.
        assert!(outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .is_none());
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);

        runtime::drain_distributions(&s).unwrap();
        assert_eq!(s.balance(owners[0]).unwrap(), U256::from(500u64));
        assert_eq!(s.balance(owners[1]).unwrap(), U256::from(500u64));
    });
}

#[test]
fn a_corrupt_total_reverts_cleanly_before_any_transfer() {
    with_factory(|s| {
        let leaves = population(300);
        install_generation_with_total(&s, &leaves, U256::from(1u64));
        let amount = U256::from(100_000u64);
        deliver_proceeds(&s, amount);

        let err = runtime::pay_contributor_batch(
            &s,
            WWD,
            0,
            &abi_leaves(&leaves[0..256]),
            &contributor_range_proof(&leaves, 0),
        )
        .unwrap_err();

        // The inflated batch dwarfs the whole factory balance: the cap must
        // reject it as a clean revert before the first transfer, never as an
        // insufficient-balance Fatal mid-batch.
        assert!(
            format!("{err:?}").contains("exceed the round amount"),
            "{err:?}"
        );
        assert_eq!(s.balance(leaves[0].owner).unwrap(), U256::ZERO);
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), amount);
    });
}

#[test]
fn late_proceeds_after_an_ownerless_certified_day_burn() {
    with_factory(|s| {
        let registry = outbe_intex::IntexContract::new(s.clone());
        registry
            .ocomp_contributor_root
            .write(&WWD, B256::repeat_byte(1))
            .unwrap();
        registry
            .ocomp_contributor_metadata
            .write(&WWD, metadata_word(1, 0))
            .unwrap();
        deliver_proceeds(&s, U256::from(500u64));
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);

        // A duplicate delivery lands after finalization cleared the deadline;
        // it must burn like any other late arrival, not sit on the balance.
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(70u64))
            .unwrap();
        runtime::distribute(&s, ORIGIN_ROUTER_ADDRESS, WWD, CHAIN, U256::from(70u64)).unwrap();
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
    });
}
