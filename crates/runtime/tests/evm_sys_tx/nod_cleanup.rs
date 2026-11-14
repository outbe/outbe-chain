use super::fixtures::GENESIS_HASH;
use super::*;
use alloy_sol_types::{SolCall, SolEvent};
use outbe_nod::precompile::INod;
use outbe_nod::NodContract;
use outbe_primitives::time::SECONDS_PER_DAY;
use outbe_system_common::addresses::NOD_ADDRESS;

const ISSUED_AT: u64 = 1_752_534_000;
const EMISSION_DAY: u32 = 20_260_715;

fn cleanup_now() -> u64 {
    ISSUED_AT + 2 * SECONDS_PER_DAY
}

fn provider_at_cleanup() -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, GENESIS_HASH);
    provider.set_block_number(2);
    provider.set_timestamp(U256::from(cleanup_now()));
    provider.set_beneficiary(VALIDATOR);
    provider
}

fn issue(storage: &StorageHandle<'_>, owner: Address) -> U256 {
    NodContract::new(storage.clone())
        .config_profile
        .write(outbe_nod::config::PROFILE_PROD)
        .unwrap();
    let item = outbe_nod::test_support::sample_item(owner, EMISSION_DAY);
    assert_eq!(item.issued_at, ISSUED_AT);
    outbe_nod::test_support::add(storage, &item).unwrap();
    item.nod_id
}

fn queue_forfeited(storage: &StorageHandle<'_>, nod_id: U256) {
    let item = outbe_nod::api::load_item(storage, nod_id).unwrap().unwrap();
    let group_key = item.body().group_key;
    let mut nod = NodContract::new(storage.clone());
    nod.group_is_forfeited.write(&group_key, true).unwrap();
    nod.called_deadline
        .write(&group_key, item.body().issued_at)
        .unwrap();
    nod.hand_to_cleanup(group_key).unwrap();
}

fn reserve(storage: &StorageHandle<'_>) -> U256 {
    outbe_promislimit::PromisLimitContract::new(storage.clone())
        .promis_limit()
        .unwrap()
}

fn run_cleanup(storage: StorageHandle<'_>) {
    let input = SystemTx::EndBlock { step_index: 0 }.encode().unwrap();
    dispatch(storage, &input, SYSTEM_ADDRESS, U256::ZERO).unwrap();
}

fn burn_count(provider: &HashMapStorageProvider) -> usize {
    provider
        .get_events(NOD_ADDRESS)
        .iter()
        .filter_map(|log| INod::Transfer::decode_log_data(log).ok())
        .filter(|event| event.to == Address::ZERO)
        .count()
}

#[test]
fn nod_factory_end_block_deletes_a_forfeited_nod() {
    let mut provider = provider_at_cleanup();
    let nod_id = provider.enter(|storage| {
        let nod_id = issue(&storage, Address::repeat_byte(0x41));
        queue_forfeited(&storage, nod_id);
        outbe_promislimit::PromisLimitContract::new(storage.clone())
            .checked_credit(U256::from(7u64))
            .unwrap();
        assert_eq!(reserve(&storage), U256::from(7u64));
        nod_id
    });

    provider.enter(|storage| {
        run_cleanup(storage.clone());
        assert!(
            outbe_nod::api::get_item(&storage, nod_id)
                .unwrap()
                .is_none(),
            "NodFactory end-block deletes a forfeited nod"
        );
        assert_eq!(reserve(&storage), U256::from(7u64));
    });
    assert_eq!(burn_count(&provider), 1);
}

#[test]
fn user_clean_then_cleanup_processing_deletes_the_rest() {
    let mut provider = provider_at_cleanup();
    let (left, right) = provider.enter(|storage| {
        let left = issue(&storage, Address::repeat_byte(0x41));
        let right = issue(&storage, Address::repeat_byte(0x42));
        queue_forfeited(&storage, left);
        let data = INod::cleanCall { nodId: left }.abi_encode();
        outbe_nod::precompile::dispatch(
            storage.clone(),
            &data,
            Address::repeat_byte(0x41),
            U256::ZERO,
        )
        .unwrap();
        assert!(outbe_nod::api::get_item(&storage, left).unwrap().is_none());
        assert!(outbe_nod::api::get_item(&storage, right).unwrap().is_some());
        (left, right)
    });

    provider.enter(|storage| {
        run_cleanup(storage.clone());
        assert!(outbe_nod::api::get_item(&storage, left).unwrap().is_none());
        assert!(outbe_nod::api::get_item(&storage, right).unwrap().is_none());
        assert_eq!(reserve(&storage), U256::ZERO);
    });
    assert_eq!(burn_count(&provider), 2);
}

#[test]
fn idle_cleanup_processing_leaves_the_nod_queue_empty() {
    let mut provider = provider_at_cleanup();
    provider.enter(|storage| {
        run_cleanup(storage.clone());
        assert!(NodContract::new(storage)
            .first_cleanup_day()
            .unwrap()
            .is_none());
    });
    assert_eq!(burn_count(&provider), 0);
}
