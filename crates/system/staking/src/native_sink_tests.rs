//! Slashed stake is a native burn: it leaves the staking escrow, reaches no
//! account, and credits no Promis Limit capacity.

use alloy_primitives::{address, Address, U256};
use outbe_primitives::addresses::{
    METADOSIS_ADDRESS, PROMIS_LIMIT_ADDRESS, REWARDS_ADDRESS, STAKING_ADDRESS,
};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_promislimit::PromisLimitContract;
use outbe_validatorset::contract::ValidatorSet;

use crate::contract::Staking;

const CHAIN_ID: u64 = 1;
const MIN_STAKE: u64 = 1_000;
const STAKE: u64 = 10_000;
const VALIDATOR: Address = address!("0x00000000000000000000000000000000000000a1");
const OWNER: Address = address!("0xffffffffffffffffffffffffffffffffffffffff");

fn with_staked_validator<R>(f: impl FnOnce(StorageHandle<'_>, &mut Staking<'_>) -> R) -> R {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_block_number(1);
    StorageHandle::enter(&mut provider, |storage| {
        let mut staking = Staking::new(storage.clone());
        staking
            .config_min_stake
            .write(U256::from(MIN_STAKE))
            .unwrap();
        staking.config_unbonding_period.write(3_600).unwrap();

        let mut validators = ValidatorSet::new(storage.clone());
        validators.config_owner.write(OWNER).unwrap();
        validators.set_config_max_validators(100).unwrap();
        let mut consensus_pubkey = [0u8; 48];
        consensus_pubkey[..20].copy_from_slice(VALIDATOR.as_slice());
        validators
            .test_register_validator_without_pop(VALIDATOR, &consensus_pubkey)
            .unwrap();

        // The EVM moves msg.value into the staking escrow before `stake` runs.
        storage
            .set_balance(VALIDATOR, U256::from(1_000_000u64))
            .unwrap();
        storage
            .set_balance(STAKING_ADDRESS, U256::from(STAKE))
            .unwrap();
        staking
            .stake(VALIDATOR, VALIDATOR, U256::from(STAKE))
            .unwrap();
        f(storage, &mut staking)
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
