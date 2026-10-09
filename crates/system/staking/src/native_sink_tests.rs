//! Slashed stake is a native burn: it leaves the staking escrow, reaches no
//! account, and credits no Promis Limit capacity.

use alloy_primitives::{address, Address, U256};
use outbe_primitives::addresses::{
    METADOSIS_ADDRESS, PROMIS_LIMIT_ADDRESS, REWARDS_ADDRESS, STAKING_ADDRESS,
};
use outbe_primitives::storage::StorageHandle;
use outbe_promislimit::PromisLimitContract;

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
            .stake(VALIDATOR, VALIDATOR, U256::from(STAKE))
            .unwrap();
        f(storage, staking)
    })
}

#[test]
fn slashing_burns_exactly_the_slashed_stake_from_the_escrow_and_credits_nothing() {
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
        let promis_limit_before = PromisLimitContract::new(storage.clone())
            .get_total_unallocated()
            .unwrap();

        let slashed = staking.slash_stake(VALIDATOR, 20).unwrap();
        assert_eq!(slashed, U256::from(2_000u64));

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
            PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
                .unwrap(),
            promis_limit_before,
            "slashing must not credit Promis Limit capacity"
        );
        assert_eq!(staking.get_stake(VALIDATOR).unwrap(), U256::from(8_000u64));
        assert_eq!(staking.get_total_staked().unwrap(), U256::from(8_000u64));
    });
}
