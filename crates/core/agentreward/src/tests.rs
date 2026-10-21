use alloy_primitives::{address, Address, U256};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::test_utils::sol_interface::sol_function_canonical;
use outbe_primitives::time::{previous_date_key, timestamp_to_date_key, WorldwideDay};

use crate::distribution::{advance_accruals, distribute_daily, PoolKind};
use crate::schema::{AgentRewardContract, RewardPool};

const CHAIN_ID: u64 = 1;
const T_NOW: u64 = 1_700_000_000;
/// A COEN price, in the six-decimal protocol scale prices are quoted in.
const ONE_COEN: U256 = U256::from_limbs([1_000_000, 0, 0, 0]);
const USD: u16 = 840;
const EUR: u16 = 978;
const MYR: u16 = 458;

/// Distributes `pools` and credits every queued WAA and SRA address, as the
/// CycleTicks after a settlement do. Returns the excess plus the queued residue.
fn distribute_and_credit(
    ctx: &outbe_primitives::block::BlockRuntimeContext,
    day: WorldwideDay,
    pools: &[(PoolKind, U256)],
) -> outbe_primitives::error::Result<U256> {
    ctx.storage.with_checkpoint(|| {
        let mut total = distribute_daily(ctx, day, pools)?;
        loop {
            let contract = AgentRewardContract::new(ctx.storage.clone());
            if contract.accrual_head.read()? == contract.accrual_tail.read()? {
                return Ok(total);
            }
            total += advance_accruals(ctx)?;
        }
    })
}

fn numbered_address(n: u64) -> Address {
    let mut bytes = [0u8; 20];
    bytes[12..].copy_from_slice(&n.to_be_bytes());
    Address::from(bytes)
}

fn with_contract_mut<R>(f: impl FnOnce(StorageHandle, &mut AgentRewardContract) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut storage, |storage| {
        let mut contract = AgentRewardContract::new(storage.clone());
        f(storage, &mut contract)
    })
}

#[test]
fn gas_06_agentreward_dense_daily_distribution_completes_and_clears_indexes() {
    const DAILY_ADDRESSES_PER_POOL: u64 = 512;
    let day = WorldwideDay::new(20260525);
    let waa_pool = U256::from(DAILY_ADDRESSES_PER_POOL * 1_000);
    let sra_pool = U256::from(DAILY_ADDRESSES_PER_POOL * 1_000);

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let ctx = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::new(1, 1, CHAIN_ID, Address::ZERO, Vec::new()),
            storage.clone(),
        );
        let mut contract = AgentRewardContract::new(storage.clone());
        let mut waa_addresses = Vec::with_capacity(DAILY_ADDRESSES_PER_POOL as usize);
        let mut sra_addresses = Vec::with_capacity(DAILY_ADDRESSES_PER_POOL as usize);

        for n in 0..DAILY_ADDRESSES_PER_POOL {
            let waa = numbered_address(1 + n);
            let sra = numbered_address(10_000 + n);
            contract.increment_waa_tribute(day, waa).unwrap();
            contract.increment_sra_tribute(day, sra).unwrap();
            waa_addresses.push(waa);
            sra_addresses.push(sra);
        }

        let excess = distribute_and_credit(
            &ctx,
            day,
            &[(PoolKind::Waa, waa_pool), (PoolKind::Sra, sra_pool)],
        )
        .expect("dense WAA/SRA daily distribution must complete");

        let contract = AgentRewardContract::new(storage.clone());
        let waa_claimable_total = waa_addresses.iter().fold(U256::ZERO, |total, address| {
            let claimable = contract.get_claimable_reward(*address).unwrap();
            assert!(
                !claimable.is_zero(),
                "GAS-06: dense WAA recipient {address} received zero claimable reward"
            );
            total + claimable
        });
        let sra_claimable_total = sra_addresses.iter().fold(U256::ZERO, |total, address| {
            let claimable = contract.get_claimable_reward(*address).unwrap();
            assert!(
                !claimable.is_zero(),
                "GAS-06: dense SRA recipient {address} received zero claimable reward"
            );
            total + claimable
        });
        let claimable_total = waa_claimable_total + sra_claimable_total;

        assert_eq!(
            excess,
            U256::ZERO,
            "GAS-06: equal dense WAA/SRA distributions should have no excess"
        );
        assert_eq!(
            claimable_total,
            waa_pool + sra_pool,
            "GAS-06: dense distribution must conserve pool amount across claimable + excess"
        );
        assert_eq!(
            ctx.storage
                .balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)
                .unwrap(),
            U256::ZERO,
            "GAS-06: distribution mints no native COEN"
        );
        assert!(
            contract.get_all_waa_counts(day).unwrap().is_empty(),
            "GAS-06: WAA day index must be cleared after dense distribution"
        );
        assert!(
            contract.get_all_sra_counts(day).unwrap().is_empty(),
            "GAS-06: SRA day index must be cleared after dense distribution"
        );
    });
}

#[test]
fn a_day_beyond_one_slice_is_credited_over_several_blocks_in_queue_order() {
    use crate::constants::ACCRUAL_RECIPIENTS_PER_BLOCK;
    let slice = u64::from(ACCRUAL_RECIPIENTS_PER_BLOCK);
    let waa_day = WorldwideDay::new(20260525);
    let sra_day = WorldwideDay::new(20260526);
    let recipients = 2 * slice + 3;
    let waa_pool = U256::from(recipients * 1_000 + 7);
    let sra_pool = U256::from(500u64);
    let sra = numbered_address(50_000);

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let ctx = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::new(1, 1, CHAIN_ID, Address::ZERO, Vec::new()),
            storage.clone(),
        );
        let mut contract = AgentRewardContract::new(storage.clone());
        for n in 0..recipients {
            contract
                .increment_waa_tribute(waa_day, numbered_address(1 + n))
                .unwrap();
        }
        contract.increment_sra_tribute(sra_day, sra).unwrap();
        let claimable = |n: u64| {
            AgentRewardContract::new(storage.clone())
                .get_pool_claimable_reward(RewardPool::Waa, numbered_address(1 + n))
                .unwrap()
        };

        assert_eq!(
            distribute_daily(&ctx, waa_day, &[(PoolKind::Waa, waa_pool)]).unwrap(),
            U256::ZERO
        );
        assert_eq!(advance_accruals(&ctx).unwrap(), U256::ZERO);
        assert_eq!(claimable(slice - 1), U256::from(1_000u64));
        assert_eq!(claimable(slice), U256::ZERO);
        let credited = U256::from(slice * 1_000);
        let contract = AgentRewardContract::new(storage.clone());
        assert_eq!(
            contract.pool_claimable_total(RewardPool::Waa).unwrap(),
            credited
        );
        assert_eq!(
            contract.pool_pending_total(RewardPool::Waa).unwrap(),
            waa_pool - credited
        );
        assert_eq!(advance_accruals(&ctx).unwrap(), U256::ZERO);
        assert_eq!(claimable(2 * slice - 1), U256::from(1_000u64));
        assert_eq!(claimable(2 * slice), U256::ZERO);

        assert_eq!(
            distribute_daily(&ctx, sra_day, &[(PoolKind::Sra, sra_pool)]).unwrap(),
            U256::ZERO
        );
        assert_eq!(advance_accruals(&ctx).unwrap(), U256::from(7u64));
        assert_eq!(claimable(recipients - 1), U256::from(1_000u64));
        let contract = AgentRewardContract::new(storage.clone());
        assert_eq!(
            contract
                .get_pool_claimable_reward(RewardPool::Sra, sra)
                .unwrap(),
            sra_pool
        );
        assert_eq!(
            contract.accrual_head.read().unwrap(),
            contract.accrual_tail.read().unwrap()
        );
        assert_eq!(
            contract.pool_claimable_total(RewardPool::Waa).unwrap(),
            waa_pool - U256::from(7u64)
        );
        assert_eq!(
            contract.pool_pending_total(RewardPool::Waa).unwrap(),
            U256::ZERO
        );
        assert!(contract.get_all_waa_counts(waa_day).unwrap().is_empty());
        assert_eq!(contract.waa_total_count.read(&waa_day).unwrap(), 0);
        assert!(contract.get_all_sra_counts(sra_day).unwrap().is_empty());
        assert_eq!(advance_accruals(&ctx).unwrap(), U256::ZERO);
    });
}

#[test]
fn test_address_list_deduplication() {
    // An address with multiple increments should appear in the list only once.
    let alice = address!("0x1111111111111111111111111111111111111111");
    let wwd = WorldwideDay::new(5);

    with_contract_mut(|_storage, contract| {
        contract.increment_waa_tribute(wwd, alice).unwrap();
        contract.increment_waa_tribute(wwd, alice).unwrap();
        contract.increment_waa_tribute(wwd, alice).unwrap();

        let counts = contract.get_all_waa_counts(wwd).unwrap();
        assert_eq!(counts.len(), 1);
        assert_eq!(counts[0].0, alice);
        assert_eq!(counts[0].1, 3);

        let addr_count = contract.waa_address_count.read(&wwd).unwrap();
        assert_eq!(addr_count, 1);
    });
}

#[test]
fn the_day_total_counts_every_tribute_and_clears_with_the_day() {
    let alice = address!("0x1111111111111111111111111111111111111111");
    let bob = address!("0x2222222222222222222222222222222222222222");
    let wwd = WorldwideDay::new(5);

    with_contract_mut(|_storage, contract| {
        contract.increment_waa_tribute(wwd, alice).unwrap();
        contract.increment_waa_tribute(wwd, alice).unwrap();
        contract.increment_waa_tribute(wwd, bob).unwrap();
        contract.increment_sra_tribute(wwd, bob).unwrap();
        assert_eq!(contract.waa_total_count.read(&wwd).unwrap(), 3);
        assert_eq!(contract.sra_total_count.read(&wwd).unwrap(), 1);

        contract.clear_waa_counts(wwd).unwrap();
        contract.clear_sra_counts(wwd).unwrap();
        assert_eq!(contract.waa_total_count.read(&wwd).unwrap(), 0);
        assert_eq!(contract.sra_total_count.read(&wwd).unwrap(), 0);
    });
}

/// Registers COEN/840 with `price` as its spot and previous-day VWAP.
fn seed_oracle(storage: &StorageHandle<'_>, price: U256) {
    outbe_oracle::test_support::publish_day_type_quote(storage, price, 1, T_NOW).unwrap();
    seed_day_vwap(storage, price);
}

/// Publishes `vwap` as the COEN/840 VWAP of the UTC day before `T_NOW`. Zero clears it.
fn seed_day_vwap(storage: &StorageHandle<'_>, vwap: U256) {
    let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), 840)
        .unwrap()
        .expect("COEN/840 registered");
    let day = previous_date_key(timestamp_to_date_key(T_NOW));
    outbe_oracle::schema::OracleContract::new(storage.clone())
        .record_utc_day_vwap(day, index, vwap)
        .unwrap();
}

/// Registers COEN/`iso` with `vwap` as its previous-day VWAP, in the reference set when `reference`.
fn price_coen_in(storage: &StorageHandle<'_>, iso: u16, vwap: U256, reference: bool) {
    outbe_oracle::api::register_pair(
        storage.clone(),
        outbe_oracle::api::AddressPair::new_coen_to(iso),
    )
    .unwrap();
    let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), iso)
        .unwrap()
        .unwrap();
    let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
    oracle
        .record_utc_day_vwap(previous_date_key(timestamp_to_date_key(T_NOW)), index, vwap)
        .unwrap();
    if reference {
        oracle.reference_currencies.push(iso).unwrap();
    }
}

/// `price` less the agent discount, the entry of an agent reward Gem.
fn discounted(price: U256) -> U256 {
    outbe_primitives::markup::marked_down(price, crate::constants::AGENT_DISCOUNT_PCT).unwrap()
}

fn gem_of(storage: &StorageHandle<'_>, owner: Address) -> outbe_gem::GemView {
    let mut owned = outbe_gem::api::list_by_owner(storage, owner).unwrap();
    assert_eq!(owned.len(), 1, "the owner holds one gem");
    let item = owned.pop().unwrap();
    outbe_gem::api::get_view(storage, item.gem_id.to_u256())
        .unwrap()
        .unwrap()
}

#[test]
fn claiming_a_pool_issues_its_gem() {
    let alice = address!("0x1111111111111111111111111111111111111111");
    let load = U256::from(500u64);

    with_contract_mut(|storage, contract| {
        seed_oracle(&storage, ONE_COEN);
        contract
            .add_claimable_reward(RewardPool::Waa, alice, load)
            .unwrap();
        assert_eq!(
            contract.pool_claimable_total(RewardPool::Waa).unwrap(),
            load
        );

        let gem_id = contract
            .claim_reward(RewardPool::Waa, alice, U256::ZERO, (USD, USD))
            .unwrap();
        assert!(!gem_id.is_zero());
        assert_eq!(
            contract.pool_claimable_total(RewardPool::Waa).unwrap(),
            U256::ZERO
        );

        let gem = gem_of(&storage, alice);
        assert_eq!(gem.item.promis_load_minor, load);
        assert_eq!(gem.item.entry_price_minor, discounted(ONE_COEN));
        let call_rate = outbe_gem::config::read(&storage).unwrap().call_rate;
        assert_eq!(
            gem.terms.call_price_minor,
            outbe_primitives::markup::marked_up(ONE_COEN, call_rate).unwrap()
        );

        assert_eq!(contract.get_claimable_reward(alice).unwrap(), U256::ZERO);
    });
}

#[test]
fn the_sra_pool_issues_a_gem_of_the_whole_load() {
    let alice = address!("0x1111111111111111111111111111111111111111");
    let load = U256::from(1_000_000u64);

    with_contract_mut(|storage, contract| {
        seed_oracle(&storage, ONE_COEN);
        contract
            .add_claimable_reward(RewardPool::Sra, alice, load)
            .unwrap();

        contract
            .claim_reward(RewardPool::Sra, alice, U256::ZERO, (USD, USD))
            .unwrap();
        let sra_gem = gem_of(&storage, alice);
        assert_eq!(sra_gem.item.promis_load_minor, load);
    });
}

fn claim_waa(
    contract: &mut AgentRewardContract,
    alice: Address,
    load: U256,
) -> outbe_primitives::error::Result<U256> {
    claim_waa_in(contract, alice, load, USD, USD)
}

fn claim_waa_in(
    contract: &mut AgentRewardContract,
    alice: Address,
    load: U256,
    issuance_currency: u16,
    reference_currency: u16,
) -> outbe_primitives::error::Result<U256> {
    contract
        .add_claimable_reward(RewardPool::Waa, alice, load)
        .unwrap();
    contract.claim_reward(
        RewardPool::Waa,
        alice,
        U256::ZERO,
        (issuance_currency, reference_currency),
    )
}

#[test]
fn a_claim_prices_on_the_previous_day_vwap() {
    let alice = address!("0x1111111111111111111111111111111111111111");
    let day_vwap = U256::from(2u64) * ONE_COEN;

    with_contract_mut(|storage, contract| {
        seed_oracle(&storage, ONE_COEN);
        seed_day_vwap(&storage, day_vwap);
        claim_waa(contract, alice, U256::from(500u64)).unwrap();
        assert_eq!(
            gem_of(&storage, alice).item.entry_price_minor,
            discounted(day_vwap)
        );
    });
}

#[test]
fn a_claim_ignores_a_spot_above_the_previous_day_vwap() {
    let alice = address!("0x1111111111111111111111111111111111111111");
    let day_vwap = U256::from(2u64) * ONE_COEN;

    with_contract_mut(|storage, contract| {
        seed_oracle(&storage, U256::from(3u64) * ONE_COEN);
        seed_day_vwap(&storage, day_vwap);
        claim_waa(contract, alice, U256::from(500u64)).unwrap();
        assert_eq!(
            gem_of(&storage, alice).item.entry_price_minor,
            discounted(day_vwap)
        );
    });
}

#[test]
fn a_claim_without_a_previous_day_vwap_keeps_the_balance() {
    let alice = address!("0x1111111111111111111111111111111111111111");
    let load = U256::from(500u64);

    with_contract_mut(|storage, contract| {
        seed_oracle(&storage, ONE_COEN);
        seed_day_vwap(&storage, U256::ZERO);
        let err = claim_waa(contract, alice, load).unwrap_err();
        assert!(err
            .to_string()
            .contains("no previous-day COEN price in 840"));
        assert_eq!(contract.get_claimable_reward(alice).unwrap(), load);
    });
}

#[test]
fn a_claim_with_no_price_leaves_the_balance_for_the_next_day() {
    let alice = address!("0x1111111111111111111111111111111111111111");
    let load = U256::from(500u64);

    with_contract_mut(|storage, contract| {
        // USD is a reference currency, with no pair, no quote and no closed day.
        outbe_oracle::schema::OracleContract::new(storage.clone())
            .reference_currencies
            .push(USD)
            .unwrap();
        contract
            .add_claimable_reward(RewardPool::Waa, alice, load)
            .unwrap();

        let err = contract
            .claim_reward(RewardPool::Waa, alice, U256::ZERO, (USD, USD))
            .unwrap_err();
        assert!(err
            .to_string()
            .contains("no previous-day COEN price in 840"));
        assert_eq!(contract.get_claimable_reward(alice).unwrap(), load);
    });
}

#[test]
fn a_claim_issues_the_gem_in_the_callers_currencies() {
    let alice = address!("0x1111111111111111111111111111111111111111");
    let eur_vwap = U256::from(3u64) * ONE_COEN;

    with_contract_mut(|storage, contract| {
        seed_oracle(&storage, ONE_COEN);
        price_coen_in(&storage, EUR, eur_vwap, true);
        claim_waa_in(contract, alice, U256::from(500u64), MYR, EUR).unwrap();
        let gem = gem_of(&storage, alice);
        assert_eq!(gem.item.issuance_currency, MYR);
        assert_eq!(gem.item.reference_currency, EUR);
        assert_eq!(gem.item.entry_price_minor, discounted(eur_vwap));
    });
}

#[test]
fn a_claim_refuses_a_reference_outside_the_reference_set() {
    let alice = address!("0x1111111111111111111111111111111111111111");
    let load = U256::from(500u64);

    for priced in [false, true] {
        with_contract_mut(|storage, contract| {
            seed_oracle(&storage, ONE_COEN);
            if priced {
                price_coen_in(&storage, MYR, ONE_COEN, false);
            }
            let err = claim_waa_in(contract, alice, load, USD, MYR).unwrap_err();
            assert!(err
                .to_string()
                .contains("458 is not a registered reference currency"));
            assert_eq!(contract.get_claimable_reward(alice).unwrap(), load);
        });
    }
}

#[test]
fn a_claim_in_an_unpriced_reference_keeps_the_balance() {
    let alice = address!("0x1111111111111111111111111111111111111111");
    let load = U256::from(500u64);

    with_contract_mut(|storage, contract| {
        seed_oracle(&storage, ONE_COEN);
        outbe_oracle::schema::OracleContract::new(storage.clone())
            .reference_currencies
            .push(EUR)
            .unwrap();
        let err = claim_waa_in(contract, alice, load, USD, EUR).unwrap_err();
        assert!(err
            .to_string()
            .contains("no previous-day COEN price in 978"));
        assert_eq!(contract.get_claimable_reward(alice).unwrap(), load);
    });
}

#[test]
fn a_claim_refuses_an_issuance_that_is_not_an_iso_code() {
    let alice = address!("0x1111111111111111111111111111111111111111");
    let load = U256::from(500u64);

    for issuance in [0, 1000] {
        with_contract_mut(|storage, contract| {
            seed_oracle(&storage, ONE_COEN);
            let err = claim_waa_in(contract, alice, load, issuance, USD).unwrap_err();
            assert!(err
                .to_string()
                .contains(&format!("{issuance} is not an ISO 4217 currency code")));
            assert_eq!(contract.get_claimable_reward(alice).unwrap(), load);
        });
    }
}

#[test]
fn a_load_whose_cost_rounds_to_zero_still_claims() {
    let alice = address!("0x1111111111111111111111111111111111111111");
    let dust = U256::from(1u64);

    with_contract_mut(|storage, contract| {
        // Entry 1 minor unit x load 1 minor unit floors below one minor unit.
        seed_oracle(&storage, U256::from(1u64));
        contract
            .add_claimable_reward(RewardPool::Waa, alice, dust)
            .unwrap();

        contract
            .claim_reward(RewardPool::Waa, alice, U256::ZERO, (USD, USD))
            .unwrap();
        assert_eq!(contract.get_claimable_reward(alice).unwrap(), U256::ZERO);
        let gem = gem_of(&storage, alice);
        assert_eq!(gem.item.entry_price_minor, U256::ONE);
        assert_eq!(gem.item.promis_load_minor, U256::ONE);
    });
}

#[test]
fn a_partial_claim_mints_only_what_was_asked_for() {
    let alice = address!("0x1111111111111111111111111111111111111111");
    let load = U256::from(500u64);
    let taken = U256::from(200u64);

    with_contract_mut(|storage, contract| {
        seed_oracle(&storage, ONE_COEN);
        contract
            .add_claimable_reward(RewardPool::Waa, alice, load)
            .unwrap();

        contract
            .claim_reward(RewardPool::Waa, alice, taken, (USD, USD))
            .unwrap();
        assert_eq!(
            gem_of(&storage, alice).item.promis_load_minor,
            U256::from(200u64)
        );
        // The rest stays a balance, which cannot be Called and cannot be forfeited.
        assert_eq!(contract.get_claimable_reward(alice).unwrap(), load - taken);
    });
}

#[test]
fn claiming_more_than_the_pool_holds_is_refused() {
    let alice = address!("0x1111111111111111111111111111111111111111");
    let load = U256::from(500u64);

    with_contract_mut(|storage, contract| {
        seed_oracle(&storage, ONE_COEN);
        contract
            .add_claimable_reward(RewardPool::Waa, alice, load)
            .unwrap();

        let err = contract
            .claim_reward(RewardPool::Waa, alice, load + U256::from(1u64), (USD, USD))
            .unwrap_err();
        assert!(err.to_string().contains("insufficient claimable balance"));
        assert_eq!(contract.get_claimable_reward(alice).unwrap(), load);
    });
}

#[test]
fn claiming_an_empty_pool_is_refused() {
    let alice = address!("0x1111111111111111111111111111111111111111");

    with_contract_mut(|storage, contract| {
        seed_oracle(&storage, ONE_COEN);
        contract
            .add_claimable_reward(RewardPool::Waa, alice, U256::from(500u64))
            .unwrap();

        let err = contract
            .claim_reward(RewardPool::Sra, alice, U256::ZERO, (USD, USD))
            .unwrap_err();
        assert!(err.to_string().contains("no claimable balance"));
    });
}

#[test]
fn the_pools_are_claimed_apart() {
    let alice = address!("0x1111111111111111111111111111111111111111");
    let waa = U256::from(500u64);
    let sra = U256::from(700u64);

    with_contract_mut(|storage, contract| {
        seed_oracle(&storage, ONE_COEN);
        contract
            .add_claimable_reward(RewardPool::Waa, alice, waa)
            .unwrap();
        contract
            .add_claimable_reward(RewardPool::Sra, alice, sra)
            .unwrap();
        assert_eq!(contract.get_claimable_reward(alice).unwrap(), waa + sra);

        contract
            .claim_reward(RewardPool::Waa, alice, U256::ZERO, (USD, USD))
            .unwrap();
        assert_eq!(
            contract
                .get_pool_claimable_reward(RewardPool::Waa, alice)
                .unwrap(),
            U256::ZERO
        );
        assert_eq!(
            contract
                .get_pool_claimable_reward(RewardPool::Sra, alice)
                .unwrap(),
            sra
        );
    });
}

// ---------------------------------------------------------------------------
// checked_add overflow rejection
// ---------------------------------------------------------------------------

#[test]
fn test_add_claimable_reward_rejects_overflow() {
    with_contract_mut(|_storage, contract| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        let near_max = U256::MAX - U256::from(10u64);

        contract
            .add_claimable_reward(RewardPool::Waa, alice, near_max)
            .unwrap();

        let err = contract
            .add_claimable_reward(RewardPool::Waa, alice, U256::from(100u64))
            .unwrap_err();
        assert!(err.to_string().contains("overflow"));

        assert_eq!(contract.get_claimable_reward(alice).unwrap(), near_max);
    });
}

// ---------------------------------------------------------------------------
// distribute_daily - new daily orchestrator surface
// ---------------------------------------------------------------------------

mod distribute_daily_tests {
    use super::*;
    use outbe_primitives::block::{BlockContext, BlockRuntimeContext};

    const TEST_CHAIN_ID: u64 = 1;
    const DAY: WorldwideDay = WorldwideDay::new(20240115);

    fn block_ctx() -> BlockContext {
        BlockContext::new(
            1,
            1_705_276_800, // 2024-01-15 UTC
            TEST_CHAIN_ID,
            alloy_primitives::Address::ZERO,
            Vec::new(),
        )
    }

    fn run<R>(f: impl FnOnce(&BlockRuntimeContext) -> R) -> R {
        let mut storage = HashMapStorageProvider::new(TEST_CHAIN_ID);
        let mut out = None;
        StorageHandle::enter(&mut storage, |handle| {
            let ctx = BlockRuntimeContext::new(block_ctx(), handle);
            out = Some(f(&ctx));
        });
        out.unwrap()
    }

    #[test]
    fn sole_waa_and_sra_receive_full_pools() {
        check_tribute_distribution(1_000, &[(0xaa, 1, 1_000)], 0);
    }

    #[test]
    fn waa_and_sra_rewards_follow_tribute_weights_without_a_cap() {
        check_tribute_distribution(1_000, &[(0xaa, 9, 900), (0xbb, 1, 100)], 0);
    }

    #[test]
    fn waa_and_sra_floor_each_share_and_return_rounding_residue() {
        check_tribute_distribution(10, &[(0xaa, 1, 3), (0xbb, 1, 3), (0xcc, 1, 3)], 1);
    }

    #[test]
    fn waa_and_sra_leave_rounding_residue_without_redistribution() {
        check_tribute_distribution(
            11,
            &[(0xaa, 1, 2), (0xbb, 1, 2), (0xcc, 1, 2), (0xdd, 1, 2)],
            3,
        );
    }

    fn check_tribute_distribution(pool: u64, recipients: &[(u8, u64, u64)], expected_excess: u64) {
        for (kind, reward_pool) in [
            (PoolKind::Waa, RewardPool::Waa),
            (PoolKind::Sra, RewardPool::Sra),
        ] {
            run(|ctx| {
                let mut contract = AgentRewardContract::new(ctx.storage.clone());
                for &(byte, weight, _) in recipients {
                    for _ in 0..weight {
                        let address = Address::repeat_byte(byte);
                        if kind == PoolKind::Waa {
                            contract.increment_waa_tribute(DAY, address).unwrap();
                        } else {
                            contract.increment_sra_tribute(DAY, address).unwrap();
                        }
                    }
                }
                let excess = distribute_and_credit(ctx, DAY, &[(kind, U256::from(pool))]).unwrap();
                assert_eq!(excess, U256::from(expected_excess));
                let (counts, address_count, addresses) = if kind == PoolKind::Waa {
                    (
                        &contract.waa_tribute_counts,
                        &contract.waa_address_count,
                        &contract.waa_addresses,
                    )
                } else {
                    (
                        &contract.sra_tribute_counts,
                        &contract.sra_address_count,
                        &contract.sra_addresses,
                    )
                };
                let mut credited = U256::ZERO;
                for &(byte, _, expected) in recipients {
                    let address = Address::repeat_byte(byte);
                    let reward = contract
                        .get_pool_claimable_reward(reward_pool, address)
                        .unwrap();
                    assert_eq!(reward, U256::from(expected));
                    credited += reward;
                    assert_eq!(
                        counts
                            .read(&AgentRewardContract::tribute_count_key(DAY, address))
                            .unwrap(),
                        0
                    );
                }
                let remaining = if kind == PoolKind::Waa {
                    contract.get_all_waa_counts(DAY).unwrap()
                } else {
                    contract.get_all_sra_counts(DAY).unwrap()
                };
                assert!(remaining.is_empty());
                assert_eq!(address_count.read(&DAY).unwrap(), 0);
                for index in 0..u32::try_from(recipients.len()).unwrap() {
                    assert_eq!(
                        addresses
                            .read(&AgentRewardContract::address_index_key(DAY, index))
                            .unwrap(),
                        Address::ZERO
                    );
                }
                assert_eq!(
                    ctx.storage
                        .balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)
                        .unwrap(),
                    U256::ZERO
                );
                assert_eq!(credited + U256::from(expected_excess), U256::from(pool));
            });
        }
    }

    #[test]
    fn waa_no_tribute_returns_full_pool_without_minting() {
        run(|ctx| {
            let excess =
                distribute_and_credit(ctx, DAY, &[(PoolKind::Waa, U256::from(500u64))]).unwrap();
            assert_eq!(excess, U256::from(500u64));
            assert_eq!(
                ctx.storage
                    .balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)
                    .unwrap(),
                U256::ZERO
            );
        });
    }

    #[test]
    fn sra_no_tribute_returns_full_pool_without_minting() {
        run(|ctx| {
            let excess =
                distribute_and_credit(ctx, DAY, &[(PoolKind::Sra, U256::from(700u64))]).unwrap();
            assert_eq!(excess, U256::from(700u64));
            assert_eq!(
                ctx.storage
                    .balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)
                    .unwrap(),
                U256::ZERO
            );
        });
    }

    #[test]
    fn cca_without_eligible_weight_returns_the_pool() {
        run(|ctx| {
            let excess =
                distribute_and_credit(ctx, DAY, &[(PoolKind::Cca, U256::from(400u64))]).unwrap();
            assert_eq!(excess, U256::from(400));
            assert_eq!(
                ctx.storage
                    .balance(outbe_primitives::addresses::CCA_REGISTRY_ADDRESS)
                    .unwrap(),
                U256::ZERO
            );
            assert_eq!(
                ctx.storage
                    .balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)
                    .unwrap(),
                U256::ZERO
            );
        });
    }

    #[test]
    fn cca_accrues_claimable_rewards_across_days() {
        run(|ctx| {
            let cca = address!("00000000000000000000000000000000000000a1");
            let bond = outbe_ccaregistry::constants::BOND_REQUIREMENT;
            ctx.storage
                .increase_balance(outbe_primitives::addresses::CCA_REGISTRY_ADDRESS, bond)
                .unwrap();
            outbe_ccaregistry::runtime::bond(ctx.storage.clone(), cca, bond, "Test CCA".into())
                .unwrap();
            outbe_ccaregistry::api::credis_issued(&ctx.storage, cca, DAY.value(), U256::ONE)
                .unwrap();
            distribute_and_credit(ctx, DAY, &[(PoolKind::Cca, U256::from(100u64))]).unwrap();
            outbe_ccaregistry::api::credis_issued(&ctx.storage, cca, 20240102, U256::ONE).unwrap();
            distribute_and_credit(ctx, 20240102.into(), &[(PoolKind::Cca, U256::from(50u64))])
                .unwrap();
            assert_eq!(
                ctx.storage
                    .balance(outbe_primitives::addresses::CCA_REGISTRY_ADDRESS)
                    .unwrap(),
                bond
            );
            assert_eq!(
                AgentRewardContract::new(ctx.storage.clone())
                    .get_pool_claimable_reward(RewardPool::Cca, cca)
                    .unwrap(),
                U256::from(150u64)
            );
        });
    }

    #[test]
    fn full_three_pool_dispatch_sums_excesses() {
        run(|ctx| {
            // Seed only WAA. SRA and CCA stay empty.
            let alice = address!("0x1111111111111111111111111111111111111111");
            let mut c = AgentRewardContract::new(ctx.storage.clone());
            c.increment_waa_tribute(DAY, alice).unwrap();

            let excess = distribute_and_credit(
                ctx,
                DAY,
                &[
                    (PoolKind::Waa, U256::from(1000u64)), // alice receives the full pool
                    (PoolKind::Sra, U256::from(500u64)),  // no tribute -> excess 500
                    (PoolKind::Cca, U256::from(100u64)),  // no eligible CCA
                ],
            )
            .unwrap();

            assert_eq!(excess, U256::from(600u64)); // 500 + 100
            let c2 = AgentRewardContract::new(ctx.storage.clone());
            assert_eq!(c2.get_claimable_reward(alice).unwrap(), U256::from(1000u64));
            assert_eq!(
                ctx.storage
                    .balance(outbe_primitives::addresses::CCA_REGISTRY_ADDRESS)
                    .unwrap(),
                U256::ZERO
            );
            assert_eq!(
                ctx.storage
                    .balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)
                    .unwrap(),
                U256::ZERO
            );
        });
    }

    #[test]
    fn zero_amount_pool_is_noop() {
        run(|ctx| {
            let excess = distribute_and_credit(ctx, DAY, &[(PoolKind::Waa, U256::ZERO)]).unwrap();
            assert_eq!(excess, U256::ZERO);
            assert_eq!(
                ctx.storage
                    .balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)
                    .unwrap(),
                U256::ZERO
            );
        });
    }

    #[test]
    fn distribution_conserves_the_pools_and_mints_no_coen() {
        run(|ctx| {
            let alice = address!("0x1111111111111111111111111111111111111111");
            let bob = address!("0x2222222222222222222222222222222222222222");
            let signer = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
            let mut c = AgentRewardContract::new(ctx.storage.clone());
            c.increment_waa_tribute(DAY, alice).unwrap();
            c.increment_waa_tribute(DAY, alice).unwrap();
            c.increment_waa_tribute(DAY, bob).unwrap();
            c.increment_sra_tribute(DAY, signer).unwrap();

            let excess = distribute_and_credit(
                ctx,
                DAY,
                &[
                    (PoolKind::Waa, U256::from(1000u64)),
                    (PoolKind::Sra, U256::from(1000u64)),
                ],
            )
            .unwrap();

            let c2 = AgentRewardContract::new(ctx.storage.clone());
            let claimable_total = c2.get_claimable_reward(alice).unwrap()
                + c2.get_claimable_reward(bob).unwrap()
                + c2.get_claimable_reward(signer).unwrap();
            assert_eq!(claimable_total + excess, U256::from(2000u64));
            assert_eq!(
                ctx.storage
                    .balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)
                    .unwrap(),
                U256::ZERO
            );
        });
    }
}

/// Drift guard between `contracts/precompiles/src/IAgentReward.sol` and the
/// `#[contract_public(...)]` annotations on `AgentRewardContract`. The
/// AgentReward module uses macro-driven dispatch, so the .sol file is a
/// documentation/abi-export mirror rather than the dispatch source.
/// This test fails if either side changes without the other.
#[test]
fn iagentreward_sol_matches_contract_public_annotations() {
    const SOL: &str = include_str!("../../../../contracts/precompiles/src/IAgentReward.sol");
    let expected = [
        ("getClaimableBalance", "address", true, "uint256"),
        ("getPoolClaimableBalance", "address,uint8", true, "uint256"),
        (
            "claimReward",
            "uint8,uint256,uint16,uint16",
            false,
            "uint256",
        ),
    ];
    for (name, args_types, is_view, ret_types) in expected {
        let canon = sol_function_canonical(SOL, name)
            .unwrap_or_else(|| panic!("IAgentReward.sol is missing `function {name}(...)`"));
        assert_eq!(canon.arg_types, args_types, "{name}: arg types differ");
        assert_eq!(canon.is_view, is_view, "{name}: view-modifier differs");
        assert_eq!(canon.ret_types, ret_types, "{name}: return types differ");
    }
}

#[test]
fn distribution_failure_restores_prior_pools_and_counts() {
    with_contract_mut(|storage, contract| {
        let alice = Address::repeat_byte(1);
        let day = WorldwideDay::new(20231113);
        contract.increment_waa_tribute(day, alice).unwrap();
        contract.increment_sra_tribute(day, alice).unwrap();
        contract
            .add_claimable_reward(RewardPool::Sra, alice, U256::MAX)
            .unwrap();
        let ctx = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::empty_for_tests(1, T_NOW, CHAIN_ID),
            storage.clone(),
        );
        assert!(distribute_and_credit(
            &ctx,
            day,
            &[
                (PoolKind::Waa, U256::from(1000)),
                (PoolKind::Sra, U256::from(1000))
            ]
        )
        .is_err());
        assert_eq!(
            contract
                .get_pool_claimable_reward(RewardPool::Waa, alice)
                .unwrap(),
            U256::ZERO
        );
        assert_eq!(
            contract
                .get_pool_claimable_reward(RewardPool::Sra, alice)
                .unwrap(),
            U256::MAX
        );
        assert_eq!(contract.get_all_waa_counts(day).unwrap(), vec![(alice, 1)]);
        assert_eq!(contract.get_all_sra_counts(day).unwrap(), vec![(alice, 1)]);
    });
}

#[test]
fn reward_timestamp_narrowing_rejects_overflow() {
    with_contract_mut(|storage, _| {
        seed_oracle(&storage, ONE_COEN);
        storage.set_block_timestamp(U256::MAX).unwrap();
        let error = crate::runtime::issue_reward_gem(
            &storage,
            (Address::repeat_byte(1), U256::from(100)),
            (USD, USD),
        )
        .unwrap_err();
        assert!(
            matches!(error, outbe_primitives::error::PrecompileError::Revert(message)
            if message == "timestamp exceeds u64")
        );
    });
}

mod cca;

#[test]
fn accrual_and_claim_emit_their_events_for_any_pool() {
    use crate::precompile::IAgentReward;
    use alloy_sol_types::SolEvent;

    let alice = address!("0x1111111111111111111111111111111111111111");
    let day = WorldwideDay::new(timestamp_to_date_key(T_NOW));
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_timestamp(U256::from(T_NOW));
    let gem_id = StorageHandle::enter(&mut provider, |storage| {
        seed_oracle(&storage, ONE_COEN);
        let mut contract = AgentRewardContract::new(storage.clone());
        contract.increment_waa_tribute(day, alice).unwrap();
        let ctx = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::empty_for_tests(1, T_NOW, CHAIN_ID),
            storage.clone(),
        );
        distribute_and_credit(&ctx, day, &[(PoolKind::Waa, U256::from(500u64))]).unwrap();
        contract
            .claim_reward(RewardPool::Waa, alice, U256::ZERO, (USD, USD))
            .unwrap()
    });
    let logs = provider.get_events(outbe_primitives::addresses::AGENT_REWARD_ADDRESS);
    let accrued: Vec<_> = logs
        .iter()
        .filter_map(|log| IAgentReward::RewardAccrued::decode_log_data(log).ok())
        .collect();
    assert_eq!(
        accrued,
        vec![IAgentReward::RewardAccrued {
            account: alice,
            pool: RewardPool::Waa as u8,
            utcDay: day.value(),
            promisLoadMinor: U256::from(500u64),
        }]
    );
    let claimed: Vec<_> = logs
        .iter()
        .filter_map(|log| IAgentReward::RewardsClaimed::decode_log_data(log).ok())
        .collect();
    assert_eq!(
        claimed,
        vec![IAgentReward::RewardsClaimed {
            account: alice,
            pool: RewardPool::Waa as u8,
            promisLoadMinor: U256::from(500u64),
            gemId: gem_id,
        }]
    );
}

/// One day with recipients in every pool: WAA weights 2 and 1, one SRA, one CCA
/// and two validators.
fn every_pool_day() -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut provider, |storage| {
        let day = WorldwideDay::new(timestamp_to_date_key(T_NOW));
        let mut contract = AgentRewardContract::new(storage.clone());
        contract
            .increment_waa_tribute(day, Address::repeat_byte(1))
            .unwrap();
        contract
            .increment_waa_tribute(day, Address::repeat_byte(1))
            .unwrap();
        contract
            .increment_waa_tribute(day, Address::repeat_byte(2))
            .unwrap();
        contract
            .increment_sra_tribute(day, Address::repeat_byte(3))
            .unwrap();

        let cca = Address::repeat_byte(4);
        let bond = outbe_ccaregistry::constants::BOND_REQUIREMENT;
        storage
            .increase_balance(outbe_primitives::addresses::CCA_REGISTRY_ADDRESS, bond)
            .unwrap();
        outbe_ccaregistry::runtime::bond(storage.clone(), cca, bond, "Test CCA".into()).unwrap();
        outbe_ccaregistry::api::credis_issued(&storage, cca, day.value(), U256::ONE).unwrap();

        let rewards = outbe_rewards::schema::Rewards::new(storage);
        rewards.daily_voter_count.write(&day.value(), 2).unwrap();
        for (index, voter) in [Address::repeat_byte(5), Address::repeat_byte(6)]
            .into_iter()
            .enumerate()
        {
            rewards
                .daily_voter_at
                .get_nested(&day.value())
                .write(&(index as u32), voter)
                .unwrap();
            rewards
                .daily_participation
                .get_nested(&day.value())
                .write(&voter, 1)
                .unwrap();
        }
    });
    provider
}

#[test]
fn a_distribution_that_fails_after_any_write_reverts_and_credits_once_on_retry() {
    let day = WorldwideDay::new(timestamp_to_date_key(T_NOW));
    let pool = U256::from(1_000u64);
    let distribute = |provider: &mut HashMapStorageProvider| {
        StorageHandle::enter(provider, |storage| {
            let ctx = outbe_primitives::block::BlockRuntimeContext::new(
                outbe_primitives::block::BlockContext::empty_for_tests(1, T_NOW, CHAIN_ID),
                storage,
            );
            distribute_and_credit(
                &ctx,
                day,
                &[
                    (PoolKind::Waa, pool),
                    (PoolKind::Sra, pool),
                    (PoolKind::Cca, pool),
                    (PoolKind::Validator, pool),
                ],
            )
        })
    };
    let expected = [
        (RewardPool::Waa, Address::repeat_byte(1), 666u64),
        (RewardPool::Waa, Address::repeat_byte(2), 333),
        (RewardPool::Sra, Address::repeat_byte(3), 1_000),
        (RewardPool::Cca, Address::repeat_byte(4), 1_000),
        (RewardPool::Validator, Address::repeat_byte(5), 500),
        (RewardPool::Validator, Address::repeat_byte(6), 500),
    ];

    let mut provider = every_pool_day();
    provider.clear_mutation_failure();
    assert_eq!(distribute(&mut provider).unwrap(), U256::ONE);
    let writes = provider.clear_mutation_failure();
    assert!(writes > 0);

    for failure_at in 0..writes {
        let mut provider = every_pool_day();
        let slots = provider.storage.clone();
        let events = provider
            .get_events(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)
            .clone();
        provider.fail_after_mutation_at(failure_at);
        assert!(distribute(&mut provider).is_err(), "failure {failure_at}");
        assert_eq!(
            provider.storage, slots,
            "a write survived failure {failure_at}"
        );
        assert_eq!(
            provider.get_events(outbe_primitives::addresses::AGENT_REWARD_ADDRESS),
            &events,
            "failure {failure_at}"
        );

        provider.clear_mutation_failure();
        assert_eq!(distribute(&mut provider).unwrap(), U256::ONE);
        StorageHandle::enter(&mut provider, |storage| {
            let contract = AgentRewardContract::new(storage);
            for (reward_pool, address, credit) in expected {
                assert_eq!(
                    contract
                        .get_pool_claimable_reward(reward_pool, address)
                        .unwrap(),
                    U256::from(credit),
                    "failure {failure_at}"
                );
            }
        });
        assert_eq!(
            provider
                .get_events(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)
                .len(),
            expected.len(),
            "failure {failure_at}: one accrual event per credit"
        );
    }
}
