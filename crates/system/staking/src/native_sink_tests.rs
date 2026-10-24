//! Slashed stake goes to the Promis Limit (OIP-00351). The slash burns the
//! whole protocol units from the staking escrow and credits them to the Promis
//! Limit. The rest below one unit stays in the escrow for the next slash.

use alloy_primitives::{address, Address, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::{
    METADOSIS_ADDRESS, PROMIS_LIMIT_ADDRESS, REWARDS_ADDRESS, STAKING_ADDRESS,
};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_primitives::units::NATIVE_UNITS_PER_PROTOCOL_UNIT;
use outbe_promislimit::PromisLimitContract;

use crate::contract::Staking;
use crate::precompile::IStaking;
use crate::tests::register_validator;

const VALIDATOR: Address = address!("0x00000000000000000000000000000000000000a1");
const OTHER_VALIDATOR: Address = address!("0x00000000000000000000000000000000000000a2");
const OWNER: Address = address!("0xffffffffffffffffffffffffffffffffffffffff");

fn units(count: u64) -> U256 {
    U256::from(count) * NATIVE_UNITS_PER_PROTOCOL_UNIT
}

fn promis_limit(storage: &StorageHandle<'_>) -> U256 {
    PromisLimitContract::new(storage.clone())
        .promis_limit()
        .unwrap()
}

/// Runs `f` after each validator in `stakes` staked its amount into the escrow.
fn with_staked(
    provider: &mut HashMapStorageProvider,
    stakes: &[(Address, U256)],
    f: impl FnOnce(StorageHandle<'_>, &mut Staking<'_>),
) {
    provider.set_block_number(1);
    StorageHandle::enter(provider, |storage| {
        let mut staking = Staking::new(storage.clone());
        staking.config_min_stake.write(U256::from(1u64)).unwrap();
        let mut escrow = U256::ZERO;
        for &(validator, amount) in stakes {
            register_validator(storage.clone(), validator);
            escrow += amount;
            // The EVM moves msg.value into the staking escrow before `stake` runs.
            storage.set_balance(STAKING_ADDRESS, escrow).unwrap();
            staking.stake(validator, validator, amount).unwrap();
        }
        f(storage, &mut staking)
    });
}

fn credit_events(provider: &HashMapStorageProvider) -> Vec<IStaking::SlashCreditedToPromisLimit> {
    provider
        .get_events(STAKING_ADDRESS)
        .iter()
        .filter(|log| {
            log.topics().first() == Some(&IStaking::SlashCreditedToPromisLimit::SIGNATURE_HASH)
        })
        .map(|log| IStaking::SlashCreditedToPromisLimit::decode_log_data(log).unwrap())
        .collect()
}

#[test]
fn slashing_burns_the_slashed_stake_and_credits_it_to_the_promis_limit() {
    let mut provider = HashMapStorageProvider::new(1);
    with_staked(
        &mut provider,
        &[(VALIDATOR, units(10_000))],
        |storage, staking| {
            let balances = |storage: &StorageHandle<'_>| {
                [
                    STAKING_ADDRESS,
                    VALIDATOR,
                    OWNER,
                    REWARDS_ADDRESS,
                    METADOSIS_ADDRESS,
                    PROMIS_LIMIT_ADDRESS,
                    Address::ZERO,
                ]
                .map(|account| storage.balance(account).unwrap())
            };
            let before = balances(&storage);
            let promis_limit_before = promis_limit(&storage);

            let slashed = staking.slash_stake(VALIDATOR, 20).unwrap();
            assert_eq!(slashed, units(2_000));

            let after = balances(&storage);
            assert_eq!(
                after[0],
                before[0] - slashed,
                "the slashed amount leaves the staking escrow"
            );
            assert_eq!(
                &after[1..],
                &before[1..],
                "no account receives the slashed stake"
            );
            assert_eq!(
                after.iter().sum::<U256>(),
                before.iter().sum::<U256>() - slashed,
                "the slashed stake is removed from native supply"
            );
            assert_eq!(
                promis_limit(&storage),
                promis_limit_before + U256::from(2_000u64),
                "slashing credits the whole units of the slash to the Promis Limit"
            );
            assert_eq!(staking.slash_dust_native.read().unwrap(), U256::ZERO);
            assert_eq!(staking.get_stake(VALIDATOR).unwrap(), units(8_000));
            assert_eq!(staking.get_total_staked().unwrap(), units(8_000));
        },
    );

    let events = credit_events(&provider);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].validator, VALIDATOR);
    assert_eq!(events[0].burnedNative, units(2_000));
    assert_eq!(events[0].creditedUnits, U256::from(2_000u64));
}

#[test]
fn two_slashes_of_one_and_a_half_units_credit_one_then_two_units() {
    // 10 % of 15 units is 1.5 units for each validator.
    let stake = units(15);
    let half_unit = NATIVE_UNITS_PER_PROTOCOL_UNIT / U256::from(2u64);
    let mut provider = HashMapStorageProvider::new(1);
    with_staked(
        &mut provider,
        &[(VALIDATOR, stake), (OTHER_VALIDATOR, stake)],
        |storage, staking| {
            let escrow_before = storage.balance(STAKING_ADDRESS).unwrap();
            let promis_limit_before = promis_limit(&storage);

            let first = staking.slash_stake(VALIDATOR, 10).unwrap();
            assert_eq!(first, units(1) + half_unit);
            assert_eq!(promis_limit(&storage), promis_limit_before + U256::ONE);
            assert_eq!(staking.slash_dust_native.read().unwrap(), half_unit);
            assert_eq!(
                storage.balance(STAKING_ADDRESS).unwrap(),
                escrow_before - units(1),
                "the rest below one unit stays in the escrow"
            );

            let second = staking.slash_stake(OTHER_VALIDATOR, 10).unwrap();
            assert_eq!(second, units(1) + half_unit);
            assert_eq!(
                promis_limit(&storage),
                promis_limit_before + U256::from(3u64)
            );
            assert_eq!(staking.slash_dust_native.read().unwrap(), U256::ZERO);
            assert_eq!(
                storage.balance(STAKING_ADDRESS).unwrap(),
                escrow_before - first - second,
                "every slashed wei left the escrow exactly once"
            );
        },
    );

    let events = credit_events(&provider);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].validator, VALIDATOR);
    assert_eq!(events[0].burnedNative, units(1));
    assert_eq!(events[0].creditedUnits, U256::ONE);
    assert_eq!(events[1].validator, OTHER_VALIDATOR);
    assert_eq!(events[1].burnedNative, units(2));
    assert_eq!(events[1].creditedUnits, U256::from(2u64));
}

#[test]
fn a_slash_below_one_unit_only_carries_the_dust() {
    let mut provider = HashMapStorageProvider::new(1);
    with_staked(
        &mut provider,
        &[(VALIDATOR, U256::from(10_000u64))],
        |storage, staking| {
            let promis_limit_before = promis_limit(&storage);
            let slashed = staking.slash_stake(VALIDATOR, 20).unwrap();
            assert_eq!(slashed, U256::from(2_000u64));
            assert_eq!(promis_limit(&storage), promis_limit_before);
            assert_eq!(staking.slash_dust_native.read().unwrap(), slashed);
            assert_eq!(
                storage.balance(STAKING_ADDRESS).unwrap(),
                U256::from(10_000u64),
                "the dust stays in the escrow"
            );
        },
    );
    assert!(credit_events(&provider).is_empty());
}

#[test]
fn a_promis_limit_overflow_fails_the_slash() {
    let mut provider = HashMapStorageProvider::new(1);
    with_staked(
        &mut provider,
        &[(VALIDATOR, units(100))],
        |storage, staking| {
            PromisLimitContract::new(storage.clone())
                .checked_credit(U256::MAX)
                .unwrap();
            assert!(staking.slash_stake(VALIDATOR, 10).is_err());
        },
    );
}
