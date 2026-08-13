use alloy_primitives::{address, keccak256, Address, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::constants::{CALL_RATE_PCT, FLOOR_RATE_PCT, MAX_OPEN_POSITIONS_PER_OWNER};
use crate::errors::CredisError;
use crate::precompile::{dispatch, ICredis};
use crate::runtime::{marked_up, settlement_deadline, OpenPositionParams};
use crate::schema::{CredisContract, CredisState};

const CHAIN_ID: u64 = 1;
const DAY: u64 = 86_400;
const ORIGINATED_AT: u64 = 1_700_000_000;

// ---------------------------------------------------------------------------
// The product paper's §5 worked example, in exact on-chain units.
//
// Maria pledges Gratis worth $1,000 at P₀ = $0.50/COEN. Stablecoin minor units
// are 6-decimal; COEN and the oracle rate are 18-decimal.
// ---------------------------------------------------------------------------

/// P = 1,000 USDC.
const PRINCIPAL: u64 = 1_000_000_000;
/// G = 2,000 COEN units.
fn collateral() -> U256 {
    U256::from(2_000u64) * U256::from(10u64).pow(U256::from(18u64))
}
/// P₀ = $0.50, 1e18 oracle scale.
fn entry_price() -> U256 {
    U256::from(500_000_000_000_000_000u64)
}
/// r = 4% annual, 1e18 scaled.
fn policy_rate() -> U256 {
    U256::from(40_000_000_000_000_000u64)
}

fn alice() -> Address {
    address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
}

fn bob() -> Address {
    address!("0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB")
}

fn cca() -> Address {
    address!("0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC")
}

fn asset() -> Address {
    address!("0x0000000000000000000000000000000000000888")
}

/// Opaque sealed-EOA blob stored verbatim on the position. Credis unit tests
/// treat it as bytes — decryption is exercised in the credisfactory enclave tests.
fn eoa_ct() -> Vec<u8> {
    vec![0xEEu8; 48]
}

fn handle(tag: u8) -> U256 {
    U256::from_be_bytes(keccak256([0x33, tag]).0)
}

fn with_credis<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ORIGINATED_AT));
    StorageHandle::enter(&mut storage, f)
}

fn params(handle_id: U256, owner: Address) -> OpenPositionParams {
    OpenPositionParams {
        handle_id,
        smart_account: owner,
        cca: cca(),
        eoa_ct: eoa_ct(),
        asset: asset(),
        issuance_currency: 840,
        policy_rate: policy_rate(),
        principal: U256::from(PRINCIPAL),
        entry_price: entry_price(),
        collateral: collateral(),
        originated_at: ORIGINATED_AT,
    }
}

/// Opens the worked-example position and latches it settleable.
fn open_settleable(credis: &mut CredisContract<'_>, tag: u8) -> U256 {
    let id = credis.open_position(params(handle(tag), alice())).unwrap();
    assert!(credis.mark_settleable(id).unwrap());
    id
}

fn at(day: u64) -> u64 {
    ORIGINATED_AT + day * DAY
}

// ---------------------------------------------------------------------------
// Key derivation and sealed terms
// ---------------------------------------------------------------------------

#[test]
fn position_id_matches_keccak() {
    let id = CredisContract::position_id(handle(1), alice());
    let mut buf = [0u8; 52];
    buf[0..32].copy_from_slice(&handle(1).to_be_bytes::<32>());
    buf[32..52].copy_from_slice(alice().as_slice());
    assert_eq!(id, U256::from_be_bytes(keccak256(buf).0));
}

#[test]
fn open_position_seals_floor_and_call_from_the_entry_price() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        let id = credis.open_position(params(handle(1), alice())).unwrap();
        let p = credis.get_position(id).unwrap();

        // $0.50 + 8% = $0.54; $0.50 + 32% = $0.66.
        assert_eq!(p.floor_price, U256::from(540_000_000_000_000_000u64));
        assert_eq!(p.call_price, U256::from(660_000_000_000_000_000u64));
        assert_eq!(
            p.floor_price,
            marked_up(entry_price(), FLOOR_RATE_PCT).unwrap()
        );
        assert_eq!(
            p.call_price,
            marked_up(entry_price(), CALL_RATE_PCT).unwrap()
        );

        // Collateral starts fully locked; accrual anchors at origination.
        assert_eq!(p.outstanding, p.principal);
        assert_eq!(p.collateral_locked, p.collateral);
        assert_eq!(p.last_settled_at, p.originated_at);
        assert_eq!(p.called_at, 0);
        assert_eq!(p.lifecycle_state().unwrap(), CredisState::Open);
        assert_eq!(p.cca, cca());
        assert_eq!(p.eoa_ct, eoa_ct());
    });
}

#[test]
fn open_position_rejects_duplicates_and_zero_amounts() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        credis.open_position(params(handle(1), alice())).unwrap();

        let err = credis
            .open_position(params(handle(1), alice()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("already exists"), "got: {err}");

        let mut zero_principal = params(handle(2), alice());
        zero_principal.principal = U256::ZERO;
        assert!(credis.open_position(zero_principal).is_err());

        let mut zero_collateral = params(handle(3), alice());
        zero_collateral.collateral = U256::ZERO;
        assert!(credis.open_position(zero_collateral).is_err());
    });
}

// ---------------------------------------------------------------------------
// The worked example, end to end (§5)
// ---------------------------------------------------------------------------

#[test]
fn worked_example_ledger_closes_exactly() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        let id = open_settleable(&mut credis, 1);

        // --- Day 100: partial settlement of $400 while latched. -------------
        let first = credis
            .settle(id, U256::from(400_000_000u64), at(100))
            .unwrap();
        // I = ceil(1_000_000_000 × 0.04 × 100/365) = $10.958905
        assert_eq!(first.interest, U256::from(10_958_905u64));
        assert_eq!(first.principal_paid, U256::from(389_041_095u64));
        assert_eq!(first.total_paid, U256::from(400_000_000u64));
        // ΔG = 2000e18 × 389_041_095 / 1e9 = 778.08219 COEN
        assert_eq!(
            first.gratis_released,
            U256::from(778_082_190_000_000_000_000u128)
        );
        assert!(!first.closed);

        let p = credis.get_position(id).unwrap();
        assert_eq!(p.outstanding, U256::from(610_958_905u64));
        assert_eq!(
            p.collateral_locked,
            U256::from(1_221_917_810_000_000_000_000u128)
        );
        assert_eq!(p.last_settled_at, at(100), "accrual restarts");

        // --- Day 458: the call. ---------------------------------------------
        assert!(credis.mark_called(id, at(458)).unwrap());
        let p = credis.get_position(id).unwrap();
        assert_eq!(p.lifecycle_state().unwrap(), CredisState::Called);
        assert_eq!(settlement_deadline(&p), at(472), "14-day window");

        // --- Day 465: partial settlement of $400 while called. --------------
        let second = credis
            .settle(id, U256::from(400_000_000u64), at(465))
            .unwrap();
        // d = 365 exactly, so I = P_out × 4%.
        assert_eq!(second.interest, U256::from(24_438_357u64));
        assert_eq!(second.principal_paid, U256::from(375_561_643u64));
        assert_eq!(
            second.gratis_released,
            U256::from(751_123_286_000_000_000_000u128)
        );

        let p = credis.get_position(id).unwrap();
        assert_eq!(p.outstanding, U256::from(235_397_262u64));
        assert_eq!(
            p.collateral_locked,
            U256::from(470_794_524_000_000_000_000u128)
        );

        // --- Day 472: the window lapses. ------------------------------------
        let void = credis.void_remainder(id, at(472)).unwrap();
        assert_eq!(
            void.gratis_burned,
            U256::from(470_794_524_000_000_000_000u128)
        );
        assert_eq!(void.principal_written_off, U256::from(235_397_262u64));
        // 7 days of interest on the remainder — never collected.
        assert_eq!(void.interest_written_off, U256::from(180_579u64));
        // Unpaid fraction 235_397_262 / 1_000_000_000 = 23.5397262%.
        assert_eq!(void.unpaid_share, U256::from(235_397_262_000_000_000u64));
        assert_eq!(void.cca, cca());
        assert_eq!(void.eoa_ct, eoa_ct());

        // --- The paper's ledger check. ---------------------------------------
        let released = first.gratis_released + second.gratis_released + void.gratis_burned;
        assert_eq!(released, collateral(), "collateral closes on G");

        let principal = first.principal_paid + second.principal_paid + void.principal_written_off;
        assert_eq!(principal, U256::from(PRINCIPAL), "principal closes on P");

        // Interest collected across both settlements: $35.397262.
        assert_eq!(first.interest + second.interest, U256::from(35_397_262u64));

        let p = credis.get_position(id).unwrap();
        assert_eq!(p.lifecycle_state().unwrap(), CredisState::Void);
        assert!(p.outstanding.is_zero());
        assert!(p.collateral_locked.is_zero());
    });
}

// ---------------------------------------------------------------------------
// Settlement rules
// ---------------------------------------------------------------------------

#[test]
fn settle_rejects_a_payment_below_the_accrued_interest() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        let id = open_settleable(&mut credis, 1);

        let position = credis.get_position(id).unwrap();
        let interest = CredisContract::accrued_interest(&position, at(100)).unwrap();
        assert!(!interest.is_zero());

        let err = credis
            .settle(id, interest - U256::from(1u64), at(100))
            .unwrap_err()
            .to_string();
        assert!(err.contains("below the interest"), "got: {err}");

        // Nothing moved.
        let after = credis.get_position(id).unwrap();
        assert_eq!(after.outstanding, position.outstanding);
        assert_eq!(after.last_settled_at, position.last_settled_at);
    });
}

#[test]
fn settle_of_exactly_the_interest_leaves_principal_and_restarts_accrual() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        let id = open_settleable(&mut credis, 1);

        let interest =
            CredisContract::accrued_interest(&credis.get_position(id).unwrap(), at(100)).unwrap();
        let paid = credis.settle(id, interest, at(100)).unwrap();

        assert_eq!(paid.interest, interest);
        assert!(paid.principal_paid.is_zero());
        assert!(paid.gratis_released.is_zero());

        let p = credis.get_position(id).unwrap();
        assert_eq!(p.outstanding, U256::from(PRINCIPAL));
        assert_eq!(p.collateral_locked, collateral());
        // d resets to 0, so nothing is owed a moment later.
        assert_eq!(
            CredisContract::accrued_interest(&p, at(100)).unwrap(),
            U256::ZERO
        );
    });
}

#[test]
fn settle_takes_only_what_the_position_needs() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        let id = open_settleable(&mut credis, 1);

        // Wildly over-pay; only interest + outstanding principal is charged.
        let paid = credis
            .settle(id, U256::from(999_999_999_999u64), at(100))
            .unwrap();
        assert_eq!(paid.principal_paid, U256::from(PRINCIPAL));
        assert_eq!(paid.total_paid, paid.interest + U256::from(PRINCIPAL));
        assert!(paid.closed);

        let p = credis.get_position(id).unwrap();
        assert_eq!(p.lifecycle_state().unwrap(), CredisState::Settled);
        assert!(p.outstanding.is_zero());
        assert!(
            p.collateral_locked.is_zero(),
            "the final settlement leaves no dust locked"
        );
    });
}

#[test]
fn settle_is_rejected_before_the_floor_latch_and_after_closing() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        let id = credis.open_position(params(handle(1), alice())).unwrap();

        let err = credis
            .settle(id, U256::from(400_000_000u64), at(100))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not settleable"), "got: {err}");

        assert!(credis.mark_settleable(id).unwrap());
        credis
            .settle(id, U256::from(999_999_999_999u64), at(100))
            .unwrap();

        let err = credis
            .settle(id, U256::from(1_000u64), at(200))
            .unwrap_err()
            .to_string();
        assert!(err.contains("closed"), "got: {err}");
    });
}

#[test]
fn settlement_stays_open_on_unchanged_terms_while_called() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        let id = open_settleable(&mut credis, 1);
        credis.mark_called(id, at(10)).unwrap();

        let paid = credis
            .settle(id, U256::from(400_000_000u64), at(100))
            .unwrap();
        // Same arithmetic as the un-called day-100 settlement: being called
        // changes nothing about the terms.
        assert_eq!(paid.interest, U256::from(10_958_905u64));
        assert_eq!(paid.principal_paid, U256::from(389_041_095u64));
        assert_eq!(
            credis.get_position(id).unwrap().lifecycle_state().unwrap(),
            CredisState::Called,
            "a partial settlement does not resolve the call"
        );
    });
}

#[test]
fn full_settlement_while_called_closes_the_position() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        let id = open_settleable(&mut credis, 1);
        credis.mark_called(id, at(10)).unwrap();

        let paid = credis
            .settle(id, U256::from(999_999_999_999u64), at(100))
            .unwrap();
        assert!(paid.closed);
        assert_eq!(
            credis.get_position(id).unwrap().lifecycle_state().unwrap(),
            CredisState::Settled
        );
        assert!(!credis.has_called_position(alice()).unwrap());
    });
}

/// Repeated partials must compose without stranding or over-releasing
/// collateral, whatever the split.
#[test]
fn repeated_partials_release_exactly_the_collateral() {
    for chunk in [1_000_000u64, 7_777_777, 123_456_789, 333_333_333] {
        with_credis(|storage| {
            let mut credis = CredisContract::new(storage);
            let id = open_settleable(&mut credis, 1);

            let mut released = U256::ZERO;
            let mut day = 1u64;
            loop {
                let position = credis.get_position(id).unwrap();
                if position.outstanding.is_zero() {
                    break;
                }
                let interest = CredisContract::accrued_interest(&position, at(day)).unwrap();
                let paid = credis
                    .settle(id, interest + U256::from(chunk), at(day))
                    .unwrap();
                released += paid.gratis_released;
                // Every unit of collateral is either released or still locked —
                // never double-counted, never stranded mid-flight.
                let after = credis.get_position(id).unwrap();
                assert_eq!(released + after.collateral_locked, collateral());
                day += 1;
            }

            assert_eq!(released, collateral(), "chunk={chunk}");
            assert!(credis.get_position(id).unwrap().collateral_locked.is_zero());
        });
    }
}

#[test]
fn interest_rounds_up_and_collateral_release_rounds_down() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        let id = open_settleable(&mut credis, 1);

        // 1 day of interest on $1,000 at 4% = 109_589.041… minor units.
        let position = credis.get_position(id).unwrap();
        assert_eq!(
            CredisContract::accrued_interest(&position, at(1)).unwrap(),
            U256::from(109_590u64),
            "interest rounds up, favoring the protocol"
        );

        // A principal payment of 1 minor unit is worth 2e21/1e9 = 2e12 gratis
        // exactly; 1 unit less than that must floor strictly below.
        let paid = credis
            .settle(id, U256::from(109_590u64) + U256::from(3u64), at(1))
            .unwrap();
        assert_eq!(paid.principal_paid, U256::from(3u64));
        assert_eq!(paid.gratis_released, U256::from(6_000_000_000_000u64));
    });
}

#[test]
fn accrual_counts_whole_days_only() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        let id = open_settleable(&mut credis, 1);
        let position = credis.get_position(id).unwrap();

        assert_eq!(
            CredisContract::accrued_interest(&position, at(0) + DAY - 1).unwrap(),
            U256::ZERO
        );
        assert!(!CredisContract::accrued_interest(&position, at(1))
            .unwrap()
            .is_zero());
    });
}

// ---------------------------------------------------------------------------
// Latch and call transitions
// ---------------------------------------------------------------------------

#[test]
fn the_floor_latch_is_one_way() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        let id = credis.open_position(params(handle(1), alice())).unwrap();

        assert!(credis.mark_settleable(id).unwrap());
        // Re-latching is a no-op, and nothing in the position can un-latch it:
        // a later price fall never reaches this state.
        assert!(!credis.mark_settleable(id).unwrap());
        assert_eq!(
            credis.get_position(id).unwrap().lifecycle_state().unwrap(),
            CredisState::Settleable
        );

        credis.mark_called(id, at(10)).unwrap();
        assert!(
            !credis.mark_settleable(id).unwrap(),
            "a called position is not demoted back to settleable"
        );
    });
}

#[test]
fn mark_called_requires_a_latched_position_and_does_not_move_the_deadline() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        let id = credis.open_position(params(handle(1), alice())).unwrap();

        assert!(
            !credis.mark_called(id, at(10)).unwrap(),
            "an unlatched position cannot be called"
        );

        credis.mark_settleable(id).unwrap();
        assert!(credis.mark_called(id, at(10)).unwrap());
        assert!(
            !credis.mark_called(id, at(20)).unwrap(),
            "re-calling must not extend the window"
        );
        assert_eq!(credis.get_position(id).unwrap().called_at, at(10));
    });
}

// ---------------------------------------------------------------------------
// Void
// ---------------------------------------------------------------------------

#[test]
fn void_requires_a_called_position_past_its_window_with_a_remainder() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        let id = open_settleable(&mut credis, 1);

        let err = credis.void_remainder(id, at(500)).unwrap_err().to_string();
        assert!(err.contains("not called"), "got: {err}");

        credis.mark_called(id, at(10)).unwrap();
        let deadline = settlement_deadline(&credis.get_position(id).unwrap());
        let err = credis
            .void_remainder(id, deadline - 1)
            .unwrap_err()
            .to_string();
        assert!(err.contains("window has not lapsed"), "got: {err}");

        // Fully settled inside the window — nothing is left to void.
        credis
            .settle(id, U256::from(999_999_999_999u64), at(11))
            .unwrap();
        let err = credis.void_remainder(id, deadline).unwrap_err().to_string();
        assert!(
            err.contains("closed") || err.contains("not called"),
            "got: {err}"
        );
    });
}

#[test]
fn a_fully_unpaid_void_burns_all_collateral_and_scores_a_full_unpaid_share() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        let id = open_settleable(&mut credis, 1);
        credis.mark_called(id, at(10)).unwrap();

        let void = credis.void_remainder(id, at(24)).unwrap();
        assert_eq!(void.gratis_burned, collateral());
        assert_eq!(void.principal_written_off, U256::from(PRINCIPAL));
        assert_eq!(
            void.unpaid_share,
            U256::from(10u64).pow(U256::from(18u64)),
            "100% unpaid"
        );
    });
}

#[test]
fn void_is_terminal() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        let id = open_settleable(&mut credis, 1);
        credis.mark_called(id, at(10)).unwrap();
        credis.void_remainder(id, at(24)).unwrap();

        // A second sweep finds nothing: the position is no longer Called.
        assert!(credis.void_remainder(id, at(25)).is_err());
        assert!(credis.settle(id, U256::from(1u64), at(25)).is_err());
        assert!(!credis.has_called_position(alice()).unwrap());
    });
}

// ---------------------------------------------------------------------------
// Reads and indexes
// ---------------------------------------------------------------------------

#[test]
fn has_called_position_tracks_the_owners_book() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        let first = open_settleable(&mut credis, 1);
        let _second = open_settleable(&mut credis, 2);
        assert!(!credis.has_called_position(alice()).unwrap());

        credis.mark_called(first, at(10)).unwrap();
        assert!(credis.has_called_position(alice()).unwrap());
        assert!(
            !credis.has_called_position(bob()).unwrap(),
            "another owner is unaffected"
        );

        credis
            .settle(first, U256::from(999_999_999_999u64), at(11))
            .unwrap();
        assert!(
            !credis.has_called_position(alice()).unwrap(),
            "settling the call in full clears the owner's block"
        );
    });
}

#[test]
fn the_open_position_cap_bounds_live_exposure_not_lifetime_originations() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        for tag in 0..MAX_OPEN_POSITIONS_PER_OWNER {
            credis
                .open_position(params(handle(tag as u8), alice()))
                .unwrap();
        }
        assert_eq!(
            credis.open_position_count(alice()).unwrap(),
            MAX_OPEN_POSITIONS_PER_OWNER
        );

        let err = credis
            .open_position(params(handle(200), alice()))
            .unwrap_err();
        assert!(err.to_string().contains("maximum number"), "got: {err}");

        // Another owner is unaffected — the cap is per account.
        credis.open_position(params(handle(201), bob())).unwrap();

        // Closing one frees a slot: the cap counts live positions, not the
        // owner's lifetime book.
        let first = CredisContract::position_id(handle(0), alice());
        credis.mark_settleable(first).unwrap();
        credis
            .settle(first, U256::from(999_999_999_999u64), at(1))
            .unwrap();
        assert_eq!(
            credis.open_position_count(alice()).unwrap(),
            MAX_OPEN_POSITIONS_PER_OWNER - 1
        );
        credis.open_position(params(handle(200), alice())).unwrap();
    });
}

#[test]
fn the_called_counter_also_clears_on_a_void() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        let id = open_settleable(&mut credis, 1);
        credis.mark_called(id, at(10)).unwrap();
        assert!(credis.has_called_position(alice()).unwrap());

        credis.void_remainder(id, at(24)).unwrap();
        assert!(
            !credis.has_called_position(alice()).unwrap(),
            "a void resolves the call as much as a settlement does"
        );
    });
}

/// Reads the active index back as a plain list, so the assertions below are
/// about membership rather than about a particular slot order.
fn active_ids(credis: &CredisContract<'_>) -> Vec<U256> {
    let mut out = Vec::new();
    for i in 0..credis.active_len().unwrap() {
        if let Some(id) = credis.active_at(i).unwrap() {
            out.push(id);
        }
    }
    out
}

#[test]
fn the_active_index_holds_exactly_the_non_terminal_positions() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        let id = credis.open_position(params(handle(1), alice())).unwrap();
        assert_eq!(active_ids(&credis), vec![id], "an open position is listed");

        // Latching and calling keep it listed — both are non-terminal.
        credis.mark_settleable(id).unwrap();
        assert_eq!(active_ids(&credis), vec![id]);
        credis.mark_called(id, at(10)).unwrap();
        assert_eq!(active_ids(&credis), vec![id]);

        // A partial settlement leaves it listed; the settlement that closes it
        // does not.
        credis
            .settle(id, U256::from(400_000_000u64), at(11))
            .unwrap();
        assert_eq!(active_ids(&credis), vec![id]);
        credis
            .settle(id, U256::from(999_999_999_999u64), at(12))
            .unwrap();
        assert!(active_ids(&credis).is_empty(), "Settled leaves the index");
    });
}

#[test]
fn a_void_leaves_the_active_index() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        let id = open_settleable(&mut credis, 1);
        credis.mark_called(id, at(10)).unwrap();
        credis.void_remainder(id, at(24)).unwrap();
        assert!(active_ids(&credis).is_empty());
    });
}

#[test]
fn removing_from_the_middle_keeps_the_active_index_consistent() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        let first = open_settleable(&mut credis, 1);
        let second = open_settleable(&mut credis, 2);
        let third = open_settleable(&mut credis, 3);
        assert_eq!(active_ids(&credis), vec![first, second, third]);

        // Close the middle one: the tail swaps into its slot.
        credis
            .settle(second, U256::from(999_999_999_999u64), at(1))
            .unwrap();
        let after = active_ids(&credis);
        assert_eq!(after.len(), 2);
        assert!(after.contains(&first) && after.contains(&third));

        // The swapped entry's stored slot must have followed it, or removing it
        // next would corrupt the list.
        credis
            .settle(third, U256::from(999_999_999_999u64), at(2))
            .unwrap();
        assert_eq!(active_ids(&credis), vec![first]);

        credis
            .settle(first, U256::from(999_999_999_999u64), at(3))
            .unwrap();
        assert!(active_ids(&credis).is_empty());
    });
}

#[test]
fn sums_and_indexes_span_all_of_an_accounts_positions() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage);
        let first = open_settleable(&mut credis, 1);
        credis.open_position(params(handle(2), alice())).unwrap();
        credis.open_position(params(handle(3), bob())).unwrap();

        assert_eq!(
            credis.get_principal_amount(alice()).unwrap(),
            U256::from(PRINCIPAL) * U256::from(2u64)
        );
        assert_eq!(
            credis.get_outstanding_amount(alice()).unwrap(),
            U256::from(PRINCIPAL) * U256::from(2u64)
        );

        // Settling one reduces outstanding but never principal.
        credis
            .settle(first, U256::from(999_999_999_999u64), at(1))
            .unwrap();
        assert_eq!(
            credis.get_principal_amount(alice()).unwrap(),
            U256::from(PRINCIPAL) * U256::from(2u64)
        );
        assert_eq!(
            credis.get_outstanding_amount(alice()).unwrap(),
            U256::from(PRINCIPAL)
        );

        assert_eq!(credis.get_positions_by_address(alice()).unwrap().len(), 2);
        assert_eq!(credis.get_positions_by_address(bob()).unwrap().len(), 1);
        assert_eq!(credis.total_positions().unwrap(), 3);
        assert_eq!(credis.get_all_positions().unwrap().len(), 3);
        assert_eq!(credis.position_id_at(0).unwrap(), first);
    });
}

#[test]
fn invalid_state_bytes_are_rejected_rather_than_silently_reinterpreted() {
    assert!(matches!(
        CredisState::from_u8(9),
        Err(CredisError::InvalidStateValue(9))
    ));
    for (value, expected) in [
        (0u8, CredisState::Open),
        (1, CredisState::Settleable),
        (2, CredisState::Called),
        (3, CredisState::Settled),
        (4, CredisState::Void),
    ] {
        assert_eq!(CredisState::from_u8(value).unwrap(), expected);
    }
    assert!(CredisState::Settled.is_terminal());
    assert!(CredisState::Void.is_terminal());
    assert!(!CredisState::Called.is_terminal());
}

// ---------------------------------------------------------------------------
// Precompile surface
// ---------------------------------------------------------------------------

#[test]
fn precompile_get_position_returns_the_full_record() {
    with_credis(|storage| {
        let id = {
            let mut credis = CredisContract::new(storage.clone());
            open_settleable(&mut credis, 1)
        };

        let data = ICredis::getPositionCall { positionId: id }.abi_encode();
        let out = dispatch(storage, &data, alice(), U256::ZERO).unwrap();
        let decoded = ICredis::getPositionCall::abi_decode_returns(&out).unwrap();

        assert_eq!(decoded.positionId, id);
        assert_eq!(decoded.smartAccount, alice());
        assert_eq!(decoded.cca, cca());
        assert_eq!(decoded.asset, asset());
        assert_eq!(decoded.issuanceCurrency, 840);
        assert_eq!(decoded.principal, U256::from(PRINCIPAL));
        assert_eq!(decoded.collateral, collateral());
        assert_eq!(decoded.entryPrice, entry_price());
        assert_eq!(decoded.floorPrice, U256::from(540_000_000_000_000_000u64));
        assert_eq!(decoded.callPrice, U256::from(660_000_000_000_000_000u64));
        assert_eq!(decoded.policyRate, policy_rate());
        assert_eq!(decoded.state, CredisState::Settleable as u8);
        assert_eq!(decoded.eoaCiphertext.to_vec(), eoa_ct());
    });
}

#[test]
fn precompile_accrued_interest_uses_the_storage_timestamp() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ORIGINATED_AT));
    let id = StorageHandle::enter(&mut storage, |handle| {
        let mut credis = CredisContract::new(handle);
        open_settleable(&mut credis, 1)
    });

    // At origination nothing has accrued.
    let data = ICredis::accruedInterestCall { positionId: id }.abi_encode();
    let out = StorageHandle::enter(&mut storage, |handle| {
        dispatch(handle, &data, alice(), U256::ZERO).unwrap()
    });
    assert_eq!(
        ICredis::accruedInterestCall::abi_decode_returns(&out).unwrap(),
        U256::ZERO
    );

    // 100 days later it matches the worked example.
    storage.set_timestamp(U256::from(at(100)));
    let out = StorageHandle::enter(&mut storage, |handle| {
        dispatch(handle, &data, alice(), U256::ZERO).unwrap()
    });
    assert_eq!(
        ICredis::accruedInterestCall::abi_decode_returns(&out).unwrap(),
        U256::from(10_958_905u64)
    );
}

#[test]
fn precompile_rejects_msg_value() {
    with_credis(|storage| {
        let data = ICredis::getAllPositionsCall {}.abi_encode();
        let err = dispatch(storage, &data, alice(), U256::from(1u64)).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("value"));
    });
}

#[test]
fn precompile_supports_erc165() {
    with_credis(|storage| {
        let data = ICredis::supportsInterfaceCall {
            interfaceId: ERC165_INTERFACE_ID.into(),
        }
        .abi_encode();
        let out = dispatch(storage, &data, alice(), U256::ZERO).unwrap();
        assert!(ICredis::supportsInterfaceCall::abi_decode_returns(&out).unwrap());
    });
}
