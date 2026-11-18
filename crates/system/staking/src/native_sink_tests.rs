//! Slashed stake burns from the staking escrow in whole minors of the Promis Limit; the part
//! below one minor stays in the escrow as the rest of code 44.

use alloy_primitives::{address, Address, U256};
use outbe_primitives::storage::StorageHandle;
use outbe_promislimit::{BurnSource, PromisLimitContract};
use outbe_system_common::addresses::{
    METADOSIS_ADDRESS, PROMIS_LIMIT_ADDRESS, REWARDS_ADDRESS, STAKING_ADDRESS,
};
use outbe_system_common::units::UNITS_PER_PROTOCOL_MINOR;

use crate::contract::Staking;
use crate::tests::{register_validator, seed_balance, with_staking};

const STAKE: u64 = 10_000;
const VALIDATOR: Address = address!("0x00000000000000000000000000000000000000a1");
const OWNER: Address = address!("0xffffffffffffffffffffffffffffffffffffffff");

/// A registered validator that staked `STAKE`, with the escrow holding it.
fn with_staked_validator<R>(f: impl FnOnce(StorageHandle<'_>, &mut Staking<'_>) -> R) -> R {
    with_staking(|storage, staking| {
        register_validator(storage.clone(), VALIDATOR);
        // The EVM moves msg.value into the staking escrow before `stake` runs.
        seed_balance(storage.clone(), VALIDATOR, 1_000_000);
        seed_balance(storage.clone(), STAKING_ADDRESS, STAKE);
        staking
            .topup(VALIDATOR, VALIDATOR, U256::from(STAKE))
            .unwrap();
        f(storage, staking)
    })
}

#[test]
fn a_sub_minor_slash_stays_in_the_escrow_as_the_rest_of_its_code() {
    with_staked_validator(|storage, staking| {
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
        let promis_limit = PromisLimitContract::new(storage.clone());
        let promis_limit_before = promis_limit.promis_limit().unwrap();
        let rest_before = promis_limit.burn_rest(BurnSource::ValidatorStake).unwrap();

        let slashed = staking.slash_stake(VALIDATOR, 20).unwrap();
        assert_eq!(slashed, U256::from(2_000u64));

        assert_eq!(
            balances(&storage),
            before,
            "a slash below one minor moves no COEN"
        );
        let promis_limit_after = promis_limit.promis_limit().unwrap();
        let rest_after = promis_limit.burn_rest(BurnSource::ValidatorStake).unwrap();
        assert_eq!(promis_limit_after, promis_limit_before);
        assert_eq!(rest_after, rest_before + slashed);
        assert_eq!(staking.get_stake(VALIDATOR).unwrap(), U256::from(8_000u64));
        assert_eq!(staking.get_total_staked().unwrap(), U256::from(8_000u64));
    });
}

#[test]
fn two_one_point_five_protocol_minor_slashes_credit_one_then_two_minors() {
    with_staking(|storage, staking| {
        register_validator(storage.clone(), VALIDATOR);
        let initial_stake = UNITS_PER_PROTOCOL_MINOR * U256::from(15u64);
        storage.set_balance(STAKING_ADDRESS, initial_stake).unwrap();
        staking.topup(VALIDATOR, VALIDATOR, initial_stake).unwrap();

        let promis_limit = PromisLimitContract::new(storage.clone());
        let one_and_half = UNITS_PER_PROTOCOL_MINOR + (UNITS_PER_PROTOCOL_MINOR / U256::from(2u64));

        // 10% of 15 protocol units = 1.5 protocol units.
        let first_slash = staking.slash_stake(VALIDATOR, 10).unwrap();
        assert_eq!(first_slash, one_and_half);
        assert_eq!(promis_limit.promis_limit().unwrap(), U256::ONE);
        assert_eq!(
            promis_limit.burn_rest(BurnSource::ValidatorStake).unwrap(),
            UNITS_PER_PROTOCOL_MINOR / U256::from(2u64)
        );
        assert_eq!(storage.balance(PROMIS_LIMIT_ADDRESS).unwrap(), U256::ZERO);

        // Top up back to 15 protocol units and slash 10% again (another 1.5 units).
        storage.set_balance(STAKING_ADDRESS, initial_stake).unwrap();
        staking.topup(VALIDATOR, VALIDATOR, first_slash).unwrap();
        let second_slash = staking.slash_stake(VALIDATOR, 10).unwrap();
        assert_eq!(second_slash, one_and_half);
        assert_eq!(promis_limit.promis_limit().unwrap(), U256::from(3u64));
        assert_eq!(
            promis_limit.burn_rest(BurnSource::ValidatorStake).unwrap(),
            U256::ZERO
        );
    });
}
