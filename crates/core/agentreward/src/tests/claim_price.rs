use super::*;

const EUR: u16 = 978;
const MYR: u16 = 458;
const LOAD: u128 = 500;

/// Registers COEN/840 with `spot` as its spot rate and no VWAP.
fn register_usd(storage: &StorageHandle<'_>, spot: U256) {
    outbe_oracle::test_support::publish_day_type_rate(storage, spot, 1, T_NOW).unwrap();
}

/// Covers the 24-hour COEN/`iso` window of the current hour at `vwap`.
fn seed_hour_vwap(storage: &StorageHandle<'_>, iso: u16, vwap: U256) {
    outbe_oracle::test_support::seed_covered_trailing_window(
        storage,
        outbe_oracle::api::AddressPair::new_coen_to(iso),
        vwap,
        U256::from(1_000_000u64),
    )
    .unwrap();
}

/// Registers COEN/`iso` with `vwap` as the 24-hour VWAP of the current hour.
fn price_coen_in(storage: &StorageHandle<'_>, iso: u16, vwap: U256) {
    outbe_oracle::api::register_pair(
        storage.clone(),
        outbe_oracle::api::AddressPair::new_coen_to(iso),
    )
    .unwrap();
    seed_hour_vwap(storage, iso, vwap);
}

/// `price` less the agent discount, the entry of an agent reward Gem.
fn discounted(price: U256) -> U256 {
    outbe_core_common::markup::marked_down(price, crate::constants::AGENT_DISCOUNT_PCT).unwrap()
}

fn gem_of(storage: &StorageHandle<'_>, owner: Address) -> outbe_gem::GemView {
    let mut owned = outbe_gem::api::list_by_owner(storage, owner).unwrap();
    assert_eq!(owned.len(), 1, "the owner holds one gem");
    let item = owned.pop().unwrap();
    outbe_gem::api::get_view(storage, item.gem_id)
        .unwrap()
        .unwrap()
}

/// A WAA pool of `day` whose only receiver `owner` is due `load`.
fn sole_waa(storage: &StorageHandle<'_>, day: u32, owner: Address, load: u128) {
    record_day(storage, day, load, emission_day_state::PROCESSING);
    let mut contract = AgentRewardContract::new(storage.clone());
    offers(&mut contract, day, &[(owner, 1)]);
    contract
        .set_agent_reward_rate(RewardPool::Waa, day)
        .unwrap();
}

fn claim(
    storage: &StorageHandle<'_>,
    owner: Address,
    currencies: (u16, u16),
) -> outbe_primitives::error::Result<U256> {
    AgentRewardContract::new(storage.clone()).redeem_agent_reward(
        RewardPool::Waa,
        owner,
        &[DAY],
        currencies,
    )
}

fn assert_claim_fails_and_keeps_the_weight(
    storage: &StorageHandle<'_>,
    owner: Address,
    currencies: (u16, u16),
    expected: outbe_primitives::error::PrecompileError,
) {
    let err = claim(storage, owner, currencies).unwrap_err();
    assert_eq!(err.to_string(), expected.to_string());
    assert_eq!(
        AgentRewardContract::new(storage.clone())
            .weight(RewardPool::Waa, DAY, owner)
            .unwrap(),
        U256::ONE
    );
}

#[test]
fn reward_gem_takes_the_price_of_its_claim_hour() {
    let hour_vwap = U256::from(2u64) * ONE_COEN;
    with_storage(|storage| {
        set_time(&storage, T_NOW - T_NOW % 86_400 + 9 * 3_600 + 20 * 60);
        register_usd(&storage, ONE_COEN);
        seed_hour_vwap(&storage, USD, hour_vwap);
        sole_waa(&storage, DAY, agent(1), LOAD);
        claim(&storage, agent(1), (USD, USD)).unwrap();
        let gem = gem_of(&storage, agent(1));
        assert_eq!(gem.item.promis_load_minor, U256::from(LOAD));
        assert_eq!(gem.item.entry_price_minor, discounted(hour_vwap));
        let call_rate = outbe_gem::config::read(&storage).unwrap().call_rate;
        assert_eq!(
            gem.terms.call_price_minor,
            outbe_core_common::markup::marked_up(hour_vwap, call_rate).unwrap()
        );
        assert_eq!(gem.item.sub_series_id, 9);
    });
}

#[test]
fn a_claim_ignores_a_spot_above_the_vwap_of_its_hour() {
    let hour_vwap = U256::from(2u64) * ONE_COEN;
    with_storage(|storage| {
        register_usd(&storage, U256::from(3u64) * ONE_COEN);
        seed_hour_vwap(&storage, USD, hour_vwap);
        sole_waa(&storage, DAY, agent(1), LOAD);
        claim(&storage, agent(1), (USD, USD)).unwrap();
        assert_eq!(
            gem_of(&storage, agent(1)).item.entry_price_minor,
            discounted(hour_vwap)
        );
    });
}

#[test]
fn a_claim_without_a_price_of_its_hour_keeps_the_weight() {
    for spot in [false, true] {
        with_storage(|storage| {
            if spot {
                register_usd(&storage, ONE_COEN);
            }
            sole_waa(&storage, DAY, agent(1), LOAD);
            assert_claim_fails_and_keeps_the_weight(
                &storage,
                agent(1),
                (USD, USD),
                AgentRewardError::RedeemPriceUnavailable(USD).into(),
            );
        });
    }
}

#[test]
fn a_claim_issues_the_gem_in_the_callers_currencies() {
    let eur_vwap = U256::from(3u64) * ONE_COEN;
    with_storage(|storage| {
        seed_oracle(&storage, ONE_COEN);
        price_coen_in(&storage, EUR, eur_vwap);
        sole_waa(&storage, DAY, agent(1), LOAD);
        claim(&storage, agent(1), (MYR, EUR)).unwrap();
        let gem = gem_of(&storage, agent(1));
        assert_eq!(gem.item.issuance_currency, MYR);
        assert_eq!(gem.item.reference_currency, EUR);
        assert_eq!(gem.item.entry_price_minor, discounted(eur_vwap));
    });
}

#[test]
fn a_claim_refuses_a_reference_outside_the_reference_set() {
    for priced in [false, true] {
        with_storage(|storage| {
            seed_oracle(&storage, ONE_COEN);
            if priced {
                price_coen_in(&storage, MYR, ONE_COEN);
            }
            sole_waa(&storage, DAY, agent(1), LOAD);
            assert_claim_fails_and_keeps_the_weight(
                &storage,
                agent(1),
                (USD, MYR),
                outbe_oracle::errors::OracleError::NotReferenceCurrency { iso_code: MYR }.into(),
            );
        });
    }
}

#[test]
fn a_claim_in_an_unpriced_reference_keeps_the_weight() {
    with_storage(|storage| {
        seed_oracle(&storage, ONE_COEN);
        sole_waa(&storage, DAY, agent(1), LOAD);
        assert_claim_fails_and_keeps_the_weight(
            &storage,
            agent(1),
            (USD, EUR),
            AgentRewardError::RedeemPriceUnavailable(EUR).into(),
        );
    });
}

#[test]
fn a_claim_refuses_an_issuance_that_is_not_an_iso_code() {
    for issuance in [0, 1000] {
        with_storage(|storage| {
            seed_oracle(&storage, ONE_COEN);
            sole_waa(&storage, DAY, agent(1), LOAD);
            assert_claim_fails_and_keeps_the_weight(
                &storage,
                agent(1),
                (issuance, USD),
                outbe_gemfactory::errors::GemFactoryError::InvalidCurrency { currency: issuance }
                    .into(),
            );
        });
    }
}

#[test]
fn a_load_whose_cost_rounds_to_zero_still_claims() {
    with_storage(|storage| {
        // Entry 1 minor unit x load 1 minor unit floors below one minor unit.
        seed_oracle(&storage, U256::ONE);
        sole_waa(&storage, DAY, agent(1), 1);
        claim(&storage, agent(1), (USD, USD)).unwrap();
        let gem = gem_of(&storage, agent(1));
        assert_eq!(gem.item.entry_price_minor, U256::ONE);
        assert_eq!(gem.item.promis_load_minor, U256::ONE);
    });
}

#[test]
fn same_block_claims_with_different_currencies_have_distinct_ids() {
    let days = [DAY, NEXT_DAY, 20_231_115, 20_231_116];
    with_storage(|storage| {
        seed_oracle(&storage, ONE_COEN);
        price_coen_in(&storage, EUR, ONE_COEN);
        for day in days {
            sole_waa(&storage, day, agent(1), LOAD);
        }
        let mut contract = AgentRewardContract::new(storage.clone());
        let mut ids = Vec::new();
        for (day, currencies) in days.into_iter().zip([(USD, USD), (EUR, USD), (USD, EUR)]) {
            let gem_id = contract
                .redeem_agent_reward(RewardPool::Waa, agent(1), &[day], currencies)
                .unwrap();
            assert!(!ids.contains(&gem_id));
            ids.push(gem_id);
            let gem = outbe_gem::api::get_item(&storage, gem_id).unwrap().unwrap();
            assert_eq!(gem.owner, agent(1));
            assert_eq!(gem.promis_load_minor, U256::from(LOAD));
            assert_eq!((gem.issuance_currency, gem.reference_currency), currencies);
        }

        let error = contract
            .redeem_agent_reward(RewardPool::Waa, agent(1), &[days[3]], (USD, USD))
            .unwrap_err();
        assert!(matches!(
            error,
            outbe_primitives::error::PrecompileError::RevertBytes(bytes)
                if bytes.as_ref() == alloy_sol_types::SolError::abi_encode(
                    &outbe_gem::precompile::IGem::GemAlreadyExists {}
                ).as_slice()
        ));
        assert_eq!(
            contract.weight(RewardPool::Waa, days[3], agent(1)).unwrap(),
            U256::ONE
        );
        assert_eq!(outbe_gem::api::balance_of(&storage, agent(1)).unwrap(), 3);
    });
}

#[test]
fn a_claim_of_64_days_issues_one_gem_for_their_sum() {
    with_storage(|storage| {
        seed_oracle(&storage, ONE_COEN);
        let mut days = Vec::new();
        let mut day = DAY;
        for _ in 0..MAX_REWARD_DAYS_PER_CALL {
            sole_waa(&storage, day, agent(1), LOAD);
            days.push(day);
            day = outbe_system_common::time::next_date_key(day).unwrap();
        }
        AgentRewardContract::new(storage.clone())
            .redeem_agent_reward(RewardPool::Waa, agent(1), &days, (USD, USD))
            .unwrap();
        assert_eq!(
            gem_of(&storage, agent(1)).item.promis_load_minor,
            U256::from(LOAD * MAX_REWARD_DAYS_PER_CALL as u128)
        );
    });
}

#[test]
fn the_last_claim_of_a_day_sends_the_rounding_rest_to_the_promis_limit() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut provider, |storage| {
        seed_oracle(&storage, ONE_COEN);
        // A limit of 3 over two equal weights pays 1 to each and leaves 1 of rounding.
        record_day(&storage, DAY, 3, emission_day_state::PROCESSING);
        let mut contract = AgentRewardContract::new(storage.clone());
        offers(&mut contract, DAY, &[(agent(1), 1), (agent(2), 1)]);
        contract
            .set_agent_reward_rate(RewardPool::Waa, DAY)
            .unwrap();
        assert_eq!(record_of(&storage, DAY).waa, 3);
        let before = promis_limit(&storage);
        for owner in [agent(1), agent(2)] {
            contract
                .redeem_agent_reward(RewardPool::Waa, owner, &[DAY], (USD, USD))
                .unwrap();
        }
        assert_eq!(promis_limit(&storage), before + U256::ONE);
        assert_eq!(
            contract.agent_reward_rate(RewardPool::Waa, DAY).unwrap(),
            (U256::ZERO, 0, U256::ZERO)
        );
    });
    let returned: Vec<_> = provider
        .get_ordered_events()
        .iter()
        .filter_map(|log| IAgentReward::AgentRewardLimitReturned::decode_log(log).ok())
        .map(|log| log.data.amountMinor)
        .collect();
    assert_eq!(returned, vec![U256::ONE]);
}
