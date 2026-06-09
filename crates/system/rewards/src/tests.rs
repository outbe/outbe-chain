use alloy_primitives::{address, U256};
use outbe_primitives::{
    addresses::REWARDS_ADDRESS, storage::hashmap::HashMapStorageProvider, storage::StorageHandle,
};

use crate::schema::Rewards;

const CHAIN_ID: u64 = 1;

fn with_rewards<R>(f: impl FnOnce(StorageHandle, &mut Rewards) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|storage| {
        let mut rewards = storage.contract::<Rewards>();
        f(storage, &mut rewards)
    })
}

// The 7 settle_from_execution_summary tests (settle_splits_fee_payouts_*,
// settle_pays_only_fees_*, settle_tracks_fee_and_emission_dust_separately,
// settle_preserves_unsolicited_external_balance_*,
// settlement_integer_properties_*, settle_zero_summary_*,
// settle_rejects_zero_voters_*) are deleted as part of step 13 — the
// underlying function is gone (replaced by the per-block hook in
// crates/system/rewards/src/finalized_metadata_hook.rs and the daily
// Cycle orchestrator, both covered by their own tests).

#[test]
fn claim_rewards_transfers_only_pending_emission_balance() {
    let alice = address!("0x1111111111111111111111111111111111111111");

    with_rewards(|storage, rewards| {
        storage.set_balance(alice, U256::from(250u64)).unwrap();
        storage
            .set_balance(REWARDS_ADDRESS, U256::from(1000u64))
            .unwrap();
        rewards
            .pending_rewards
            .write(&alice, U256::from(500u64))
            .unwrap();

        let claimed = rewards.claim_rewards(alice).unwrap();
        assert_eq!(claimed, U256::from(500u64));
        assert_eq!(rewards.pending_rewards.read(&alice).unwrap(), U256::ZERO);
        assert_eq!(storage.balance(alice).unwrap(), U256::from(750u64));

        let claimed_again = rewards.claim_rewards(alice).unwrap();
        assert_eq!(claimed_again, U256::ZERO);
    });
}

#[test]
fn pending_rewards_of_reads_claimable_emission_balance() {
    let alice = address!("0x1111111111111111111111111111111111111111");

    with_rewards(|_storage, rewards| {
        rewards
            .pending_rewards
            .write(&alice, U256::from(123u64))
            .unwrap();
        assert_eq!(
            rewards.pending_rewards_of(alice).unwrap(),
            U256::from(123u64)
        );
    });
}
