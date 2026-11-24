use alloy_primitives::U256;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::schema::PromisLimitContract;

const CHAIN_ID: u64 = 1;

fn with_contract<R>(f: impl FnOnce(&mut PromisLimitContract) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut contract = PromisLimitContract::new(storage.clone());
        f(&mut contract)
    })
}

#[test]
fn test_initial_state() {
    with_contract(|c| {
        assert_eq!(c.promis_limit().unwrap(), U256::ZERO);
    });
}

#[test]
fn test_set_promis_limit() {
    with_contract(|c| {
        let amount = U256::from(1_000_000u64);
        c.set_promis_limit(amount).unwrap();
        assert_eq!(c.promis_limit().unwrap(), amount);
    });
}

fn promis_limit_logs(storage: &HashMapStorageProvider) -> Vec<(u8, bool, U256)> {
    use alloy_sol_types::SolEvent;
    use outbe_system_common::addresses::PROMIS_LIMIT_ADDRESS;

    use crate::precompile::IPromisLimit;

    storage
        .get_events(PROMIS_LIMIT_ADDRESS)
        .iter()
        .map(|log| {
            if let Ok(added) =
                IPromisLimit::PromisLimitAdded::decode_raw_log(log.topics(), &log.data)
            {
                (added.source, true, added.amountMinor)
            } else {
                let taken =
                    IPromisLimit::PromisLimitSubtracted::decode_raw_log(log.topics(), &log.data)
                        .unwrap();
                (taken.target, false, taken.amountMinor)
            }
        })
        .collect()
}

#[test]
fn each_add_and_subtract_emits_its_code_from_the_promis_limit() {
    use crate::runtime::{LimitSource, LimitTarget};

    let sources = [
        LimitSource::DayEmissionLimit,
        LimitSource::MetadosisLimit,
        LimitSource::LysisLimit,
        LimitSource::DesisLimit,
        LimitSource::RewardLimit,
        LimitSource::Nod,
        LimitSource::Intex,
        LimitSource::Gem,
        LimitSource::Credis,
        LimitSource::GemFactoryPosition,
        LimitSource::AgentReward,
    ];
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |handle| {
        let mut contract = PromisLimitContract::new(handle);
        for (i, source) in sources.iter().enumerate() {
            contract.add(*source, U256::from(i as u64 + 1)).unwrap();
        }
        assert!(contract
            .subtract(LimitTarget::DesisLimit, U256::from(10u64))
            .unwrap());
        assert_eq!(contract.promis_limit().unwrap(), U256::from(56u64));
    });

    let mut expected: Vec<_> = sources
        .iter()
        .enumerate()
        .map(|(i, source)| (*source as u8, true, U256::from(i as u64 + 1)))
        .collect();
    expected.push((1, false, U256::from(10u64)));
    assert_eq!(promis_limit_logs(&storage), expected);
}

#[test]
fn a_zero_add_or_subtract_changes_nothing_and_emits_nothing() {
    use crate::runtime::{LimitSource, LimitTarget};

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |handle| {
        let mut contract = PromisLimitContract::new(handle);
        contract.add(LimitSource::Nod, U256::ZERO).unwrap();
        assert!(contract
            .subtract(LimitTarget::DesisLimit, U256::ZERO)
            .unwrap());
        assert_eq!(contract.promis_limit().unwrap(), U256::ZERO);
    });
    assert!(promis_limit_logs(&storage).is_empty());
}

#[test]
fn a_subtract_above_the_balance_changes_nothing_and_emits_nothing() {
    use crate::runtime::{LimitSource, LimitTarget};

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |handle| {
        let mut contract = PromisLimitContract::new(handle);
        contract.add(LimitSource::Gem, U256::from(5u64)).unwrap();
        assert!(!contract
            .subtract(LimitTarget::DesisLimit, U256::from(6u64))
            .unwrap());
        assert_eq!(contract.promis_limit().unwrap(), U256::from(5u64));
    });
    assert_eq!(
        promis_limit_logs(&storage),
        vec![(LimitSource::Gem as u8, true, U256::from(5u64))]
    );
}

#[test]
fn an_overflowing_add_fails_the_block() {
    use crate::runtime::LimitSource;

    with_contract(|c| {
        c.set_promis_limit(U256::MAX).unwrap();
        let err = c.add(LimitSource::Nod, U256::ONE).unwrap_err();
        assert!(
            matches!(
                err,
                outbe_primitives::error::PrecompileError::FatalWithCause(_)
            ),
            "{err:?}"
        );
        assert_eq!(c.promis_limit().unwrap(), U256::MAX);
    });
}

#[test]
fn test_set_overwrites_previous() {
    with_contract(|c| {
        c.set_promis_limit(U256::from(500u64)).unwrap();
        c.set_promis_limit(U256::from(200u64)).unwrap();
        assert_eq!(c.promis_limit().unwrap(), U256::from(200u64));
    });
}

#[test]
fn test_storage_dsl_layout_is_compatible_with_previous_slots() {
    with_contract(|c| {
        assert_eq!(c.promis_limit.slot(), U256::ZERO);
        assert_eq!(c.burn_rests.base_slot(), U256::ONE);
        assert_eq!(c.base_fee_count.slot(), U256::from(2));
    });
}

const COEN: u128 = 1_000_000_000_000_000_000;
const MINOR: u128 = 1_000_000_000_000;

#[test]
fn two_slashes_burn_whole_minors_and_keep_the_rest_on_staking() {
    use outbe_system_common::addresses::STAKING_ADDRESS;

    use crate::runtime::BurnSource;

    let first = U256::from(2 * COEN + 700_000_000_000 + 300);
    let second = U256::from(COEN + 300_000_000_000);
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_balance(STAKING_ADDRESS, first + second);
    StorageHandle::enter(&mut storage, |handle| {
        let mut c = PromisLimitContract::new(handle.clone());
        c.burn(BurnSource::ValidatorStake, first).unwrap();
        c.burn(BurnSource::ValidatorStake, second).unwrap();
        assert_eq!(c.promis_limit().unwrap(), U256::from(3_000_001));
        assert_eq!(
            c.burn_rest(BurnSource::ValidatorStake).unwrap(),
            U256::from(300)
        );
        assert_eq!(handle.balance(STAKING_ADDRESS).unwrap(), U256::from(300));
    });
    assert_eq!(
        promis_limit_logs(&storage),
        vec![
            (44, true, U256::from(2_000_000)),
            (44, true, U256::from(1_000_001))
        ]
    );
}

#[test]
fn each_burn_code_keeps_its_own_rest_on_its_own_account() {
    use outbe_primitives::addresses::REWARDS_ADDRESS;
    use outbe_system_common::addresses::{
        GRATIS_FACTORY_ADDRESS, INTEX_FACTORY_ADDRESS, STAKING_ADDRESS, VOTE_ADDRESS,
    };

    use crate::runtime::BurnSource;

    let sources = [
        (BurnSource::PriorityFees, REWARDS_ADDRESS),
        (BurnSource::IntexAuctionProceeds, INTEX_FACTORY_ADDRESS),
        (BurnSource::ValidatorStake, STAKING_ADDRESS),
        (BurnSource::ProposalBond, VOTE_ADDRESS),
        (BurnSource::GratisMint, GRATIS_FACTORY_ADDRESS),
    ];
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    for (_, account) in sources {
        storage.set_balance(account, U256::from(MINOR));
    }
    StorageHandle::enter(&mut storage, |handle| {
        let mut c = PromisLimitContract::new(handle.clone());
        for (i, (source, account)) in sources.into_iter().enumerate() {
            assert_eq!(source.account(), account);
            c.burn(source, U256::from(MINOR / 2 + i as u128)).unwrap();
        }
        assert_eq!(c.promis_limit().unwrap(), U256::ZERO);
        for (i, (source, account)) in sources.into_iter().enumerate() {
            assert_eq!(
                c.burn_rest(source).unwrap(),
                U256::from(MINOR / 2 + i as u128)
            );
            assert_eq!(handle.balance(account).unwrap(), U256::from(MINOR));
            c.burn(source, U256::from(MINOR / 2)).unwrap();
            assert_eq!(c.burn_rest(source).unwrap(), U256::from(i));
            assert_eq!(handle.balance(account).unwrap(), U256::ZERO);
        }
        assert_eq!(c.promis_limit().unwrap(), U256::from(5));
    });
    assert_eq!(
        promis_limit_logs(&storage),
        vec![
            (42, true, U256::ONE),
            (43, true, U256::ONE),
            (44, true, U256::ONE),
            (45, true, U256::ONE),
            (47, true, U256::ONE)
        ]
    );
}

#[test]
fn an_overflowing_burn_fails_and_moves_nothing() {
    use outbe_system_common::addresses::VOTE_ADDRESS;

    use crate::runtime::BurnSource;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_balance(VOTE_ADDRESS, U256::from(2 * MINOR));
    StorageHandle::enter(&mut storage, |handle| {
        let mut c = PromisLimitContract::new(handle.clone());
        c.burn(BurnSource::ProposalBond, U256::from(MINOR / 2))
            .unwrap();
        c.set_promis_limit(U256::MAX).unwrap();
        let err = c
            .burn(BurnSource::ProposalBond, U256::from(MINOR))
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::PromisLimitError::Overflow
            )
            .to_string()
        );
        assert_eq!(c.promis_limit().unwrap(), U256::MAX);
        assert_eq!(
            c.burn_rest(BurnSource::ProposalBond).unwrap(),
            U256::from(MINOR / 2)
        );
        assert_eq!(handle.balance(VOTE_ADDRESS).unwrap(), U256::from(2 * MINOR));
    });
    assert!(promis_limit_logs(&storage).is_empty());
}

#[test]
fn base_fees_wait_for_the_daily_step_with_the_burn_addresses() {
    use alloy_primitives::Address;

    use crate::{runtime::DEAD_ADDRESS, settle_block_burned_fees};

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_balance(Address::ZERO, U256::from(3 * MINOR + 5));
    storage.set_balance(DEAD_ADDRESS, U256::from(MINOR));
    StorageHandle::enter(&mut storage, |mut handle| {
        settle_block_burned_fees(&mut handle, U256::from(MINOR * 9 / 10)).unwrap();
        settle_block_burned_fees(&mut handle, U256::from(MINOR * 13 / 10)).unwrap();
        let mut c = PromisLimitContract::new(handle.clone());
        assert_eq!(c.promis_limit().unwrap(), U256::ZERO);
        assert_eq!(c.base_fee_count().unwrap(), U256::from(MINOR * 22 / 10));

        c.settle_day_burns().unwrap();
        assert_eq!(c.promis_limit().unwrap(), U256::from(6));
        assert_eq!(c.base_fee_count().unwrap(), U256::from(MINOR * 2 / 10));
        assert_eq!(handle.balance(Address::ZERO).unwrap(), U256::from(5));
        assert_eq!(handle.balance(DEAD_ADDRESS).unwrap(), U256::ZERO);

        c.settle_day_burns().unwrap();
        assert_eq!(c.promis_limit().unwrap(), U256::from(6));
    });
    assert_eq!(
        promis_limit_logs(&storage),
        vec![(41, true, U256::from(2)), (46, true, U256::from(4))]
    );
}

fn wcoen_provider(answer: bool) -> HashMapStorageProvider {
    use alloy_sol_types::SolValue;
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.stub_sub_call_at(
        outbe_core_common::addresses::WCOEN_ADDRESS,
        alloy_primitives::Bytes::from(answer.abi_encode()),
    );
    storage
}

#[test]
fn burn_wcoen_transfers_to_address_zero_and_adds_the_whole_minors() {
    use alloy_primitives::Address;
    use alloy_sol_types::SolCall;
    use outbe_system_common::units::UNITS_PER_PROTOCOL_MINOR;

    use crate::runtime::{BurnSource, IWCOEN};

    let rest = U256::from(5u64);
    let amount = U256::from(2u64) * UNITS_PER_PROTOCOL_MINOR + rest;
    let mut storage = wcoen_provider(true);
    StorageHandle::enter(&mut storage, |s| {
        // The WCOEN transfer to address 0 leaves its native COEN there.
        s.increase_balance(Address::ZERO, amount).unwrap();
        let mut contract = PromisLimitContract::new(s.clone());
        contract
            .burn_wcoen(BurnSource::IntexAuctionProceeds, amount)
            .unwrap();
        assert_eq!(contract.promis_limit().unwrap(), U256::from(2u64));
        assert_eq!(s.balance(Address::ZERO).unwrap(), rest);
        assert_eq!(
            contract
                .burn_rest(BurnSource::IntexAuctionProceeds)
                .unwrap(),
            U256::ZERO
        );
    });
    let transfer = storage
        .sub_call_inputs()
        .iter()
        .find(|input| input.target == outbe_core_common::addresses::WCOEN_ADDRESS)
        .map(|input| IWCOEN::transferCall::abi_decode(&input.calldata).unwrap())
        .unwrap();
    assert_eq!((transfer.dst, transfer.wad), (Address::ZERO, amount));
    assert_eq!(
        promis_limit_logs(&storage),
        vec![(
            BurnSource::IntexAuctionProceeds as u8,
            true,
            U256::from(2u64)
        )]
    );
}

#[test]
fn burn_wcoen_refused_by_wcoen_changes_nothing() {
    use crate::runtime::BurnSource;
    let mut storage = wcoen_provider(false);
    StorageHandle::enter(&mut storage, |s| {
        let mut contract = PromisLimitContract::new(s.clone());
        assert!(contract
            .burn_wcoen(BurnSource::IntexAuctionProceeds, U256::from(1u64))
            .is_err());
        assert_eq!(contract.promis_limit().unwrap(), U256::ZERO);
    });
    assert!(promis_limit_logs(&storage).is_empty());
}
