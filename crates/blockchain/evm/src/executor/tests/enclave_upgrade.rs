//! A missed enclave upgrade slashes the admitted felony percent (8 % by
//! default) and credits the slash to the Promis Limit (OIP-00351).

use alloy_primitives::{address, Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::{STAKING_ADDRESS, TEE_REGISTRY_ADDRESS};
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1::EnclaveUpgradeMissedV1;

use super::super::hooks::enforce_enclave_upgrade_deadline;

const OWNER: Address = address!("0x00000000000000000000000000000000000000f1");
const VALIDATOR: Address = address!("0x00000000000000000000000000000000000000a1");
const PROPOSAL: u64 = 7;
const ACTIVATION_HEIGHT: u64 = 50;
/// One whole COEN. 8 % of it is 80_000 whole protocol units.
const STAKE: u64 = 1_000_000_000_000_000_000;
const EIGHT_PERCENT_SLASH: u64 = 80_000_000_000_000_000;

fn due_upgrade_provider() -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_block_number(1);
    provider.set_timestamp(U256::from(1_700_000_000u64));
    provider.enter(|storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_epoch_length_blocks.write(10).unwrap();
        vs.register_validator(OWNER, VALIDATOR, &[7u8; 48]).unwrap();
        vs.mark_pending(VALIDATOR).unwrap();
        vs.confirm_validator_ready(VALIDATOR).unwrap();
        vs.activate_validator_via_boundary_for_test(VALIDATOR)
            .unwrap();

        let staking = outbe_staking::contract::Staking::new(storage.clone());
        staking
            .stake_amount
            .write(&VALIDATOR, U256::from(STAKE))
            .unwrap();
        staking.total_staked.write(U256::from(STAKE)).unwrap();
        storage
            .increase_balance(STAKING_ADDRESS, U256::from(STAKE))
            .unwrap();

        // The validator has no successor binding when the upgrade activates.
        let registry = outbe_teeregistry::TeeRegistry::new(storage);
        registry
            .strict_upgrade_proposal
            .write(U256::from(PROPOSAL))
            .unwrap();
        registry
            .strict_upgrade_height
            .write(ACTIVATION_HEIGHT)
            .unwrap();
        registry
            .strict_upgrade_mrenclave
            .write(B256::repeat_byte(0x11))
            .unwrap();
        registry
            .strict_upgrade_successor
            .write(B256::repeat_byte(0x22))
            .unwrap();
        registry
            .active_v1_policy_proposal_id
            .write(U256::from(PROPOSAL))
            .unwrap();
    });
    provider.set_block_number(ACTIVATION_HEIGHT);
    provider
}

#[test]
fn missed_enclave_upgrade_slashes_eight_percent_and_credits_the_promis_limit() {
    let mut provider = due_upgrade_provider();
    provider.enter(|storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::new(
                ACTIVATION_HEIGHT,
                storage.now_secs().unwrap(),
                1,
                VALIDATOR,
                vec![VALIDATOR],
            ),
            storage.clone(),
        );
        enforce_enclave_upgrade_deadline(&ctx).unwrap();

        let staking = outbe_staking::contract::Staking::new(storage.clone());
        assert_eq!(
            staking.get_stake(VALIDATOR).unwrap(),
            U256::from(STAKE - EIGHT_PERCENT_SLASH)
        );
        assert_eq!(
            storage.balance(STAKING_ADDRESS).unwrap(),
            U256::from(STAKE - EIGHT_PERCENT_SLASH)
        );
        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(storage.clone())
                .promis_limit()
                .unwrap(),
            U256::from(80_000u64)
        );
        assert_eq!(
            outbe_validatorset::contract::ValidatorSet::new(storage.clone())
                .get_validator(VALIDATOR)
                .unwrap()
                .unwrap()
                .status,
            outbe_validatorset::runtime::status::JAILED
        );
        assert!(outbe_teeregistry::TeeRegistry::new(storage)
            .upgrade_penalty_applied_v1(U256::from(PROPOSAL), VALIDATOR)
            .unwrap());
    });

    let missed = provider
        .get_events(TEE_REGISTRY_ADDRESS)
        .iter()
        .filter(|log| log.topics().first() == Some(&EnclaveUpgradeMissedV1::SIGNATURE_HASH))
        .map(|log| EnclaveUpgradeMissedV1::decode_log_data(log).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(missed.len(), 1);
    assert_eq!(missed[0].validator, VALIDATOR);
    assert_eq!(missed[0].slashedAmount, U256::from(EIGHT_PERCENT_SLASH));
}

#[test]
fn missed_enclave_upgrade_follows_the_configured_slash_percent() {
    let mut provider = due_upgrade_provider();
    provider.enter(|storage| {
        outbe_slashindicator::contract::SlashIndicator::new(storage.clone())
            .config_slash_amount_percent
            .write(20)
            .unwrap();
        let ctx = BlockRuntimeContext::new(
            BlockContext::new(
                ACTIVATION_HEIGHT,
                storage.now_secs().unwrap(),
                1,
                VALIDATOR,
                vec![VALIDATOR],
            ),
            storage.clone(),
        );
        enforce_enclave_upgrade_deadline(&ctx).unwrap();
        assert_eq!(
            outbe_staking::contract::Staking::new(storage)
                .get_stake(VALIDATOR)
                .unwrap(),
            U256::from(STAKE / 100 * 80)
        );
    });
}
