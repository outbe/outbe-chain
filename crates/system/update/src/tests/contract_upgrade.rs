use super::{block_ctx, TEST_CHAIN_ID};
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolError, SolEvent};
use outbe_primitives::addresses::{CROSSCHAIN_BRIDGE_ADDRESS, UPDATE_ADDRESS};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::vote_target::{
    vote_proposals, ProposalRecord, ProposalStatus, TargetExecutionOutcome, VoteTarget,
    VoteTargetContext,
};

use crate::precompile::{dispatch, IUpdate};
use crate::schema::Update;
use crate::sol_ext::{ICrosschainBridge, IUUPSUpgradeable};
use crate::vote_target::UpdateVoteTarget;

const SEPOLIA: u32 = 11_155_111;
const BSC: u32 = 97;

/// The governance voting window `W`.
fn window() -> u64 {
    outbe_chain_constants::get_governance_voting_window_blocks()
}

/// The approval block. It is after one full window, so a standard proposal
/// fits before it.
fn approved_at() -> u64 {
    window() + 30
}

/// A proposal with the standard window: its tally is `created + W + 1`.
fn standard_created() -> u64 {
    approved_at() - window() - 1
}

/// A proposal with a 1-block window: its tally is `created + 2`.
fn short_created() -> u64 {
    approved_at() - 2
}

fn upgrade(chain_id: u64, proxy: u8) -> String {
    format!(
        r#"{{"chainId":{chain_id},"proxy":"{:#x}","implementation":"{:#x}","callData":"0x8129fc1c"}}"#,
        Address::repeat_byte(proxy),
        Address::repeat_byte(0x9a)
    )
}

fn upgrades(list: &[(u64, u8)]) -> String {
    let list: Vec<String> = list.iter().map(|&(c, p)| upgrade(c, p)).collect();
    format!(
        r#"{{"operation":"upgradeContracts","upgrades":[{}]}}"#,
        list.join(",")
    )
}

fn upgrade_call(proxy: u8) -> ICrosschainBridge::Call {
    ICrosschainBridge::Call {
        to: Address::repeat_byte(proxy),
        value: U256::ZERO,
        data: IUUPSUpgradeable::upgradeToAndCallCall {
            newImplementation: Address::repeat_byte(0x9a),
            data: Bytes::from_static(&[0x81, 0x29, 0xfc, 0x1c]),
        }
        .abi_encode()
        .into(),
    }
}

/// Stores the Vote proposal record of proposal 1, created at `created_height`.
fn propose(p: &mut HashMapStorageProvider, payload: &str, created_height: u64) {
    StorageHandle::enter(p, |storage| {
        let record = ProposalRecord {
            id: U256::ONE,
            proposer: Address::repeat_byte(1),
            target_module: UPDATE_ADDRESS,
            payload: payload.to_string(),
            created_height,
            voting_deadline_height: approved_at() - 1,
            status: ProposalStatus::Approved.to_u8(),
        };
        vote_proposals(storage).create(&record).unwrap();
    });
}

/// Approves proposal 1 at `approved_at()` without a Vote proposal record.
fn approve_without_record(p: &mut HashMapStorageProvider, payload: &str) {
    StorageHandle::enter(p, |storage| {
        let context = VoteTargetContext {
            proposer: Address::repeat_byte(1),
            attached_value: U256::ZERO,
            block_number: approved_at(),
            chain_id: TEST_CHAIN_ID,
        };
        let outcome = UpdateVoteTarget
            .handle_approved(
                &block_ctx(storage, approved_at()),
                U256::ONE,
                payload.as_bytes(),
                context,
            )
            .unwrap();
        assert_eq!(outcome, TargetExecutionOutcome::Applied);
    });
    p.set_block_number(approved_at());
}

fn approve(p: &mut HashMapStorageProvider, payload: &str, created_height: u64) {
    propose(p, payload, created_height);
    approve_without_record(p, payload);
}

fn stored_approval(p: &mut HashMapStorageProvider) -> B256 {
    StorageHandle::enter(p, |storage| {
        Update::new(storage)
            .approved_upgrades
            .read(&U256::ONE)
            .unwrap()
    })
}

fn has_event(p: &HashMapStorageProvider, topic0: B256) -> bool {
    p.get_events(UPDATE_ADDRESS)
        .iter()
        .any(|log| log.topics().first() == Some(&topic0))
}

fn apply(p: &mut HashMapStorageProvider, payload: &str) -> Result<Bytes, PrecompileError> {
    let call = IUpdate::applyApprovedCall {
        proposalId: U256::ONE,
        payload: payload.as_bytes().to_vec().into(),
    };
    StorageHandle::enter(p, |storage| {
        dispatch(
            storage,
            &call.abi_encode(),
            Address::repeat_byte(0xee),
            U256::ZERO,
        )
    })
}

fn revert_selector(err: PrecompileError) -> [u8; 4] {
    match err {
        PrecompileError::RevertBytes(bytes) => bytes[..4].try_into().unwrap(),
        other => panic!("expected a revert, got {other:?}"),
    }
}

#[test]
fn apply_approved_upgrades_locally_and_sends_one_message_to_each_other_chain() {
    let mut p = HashMapStorageProvider::new(TEST_CHAIN_ID);
    p.enable_sub_call_stub();
    let payload = upgrades(&[
        (u64::from(SEPOLIA), 0xb1),
        (TEST_CHAIN_ID, 0xb1),
        (u64::from(BSC), 0xb1),
        (u64::from(SEPOLIA), 0xb2),
    ]);
    approve(&mut p, &payload, standard_created());
    apply(&mut p, &payload).unwrap();

    let sent: Vec<_> = p
        .sub_call_inputs()
        .iter()
        .map(|input| (input.target, input.calldata.to_vec()))
        .collect();
    let expected = [
        ICrosschainBridge::callLocalCall {
            call: upgrade_call(0xb1),
        }
        .abi_encode(),
        ICrosschainBridge::callRemoteCall {
            domain: SEPOLIA,
            calls: vec![upgrade_call(0xb1), upgrade_call(0xb2)],
        }
        .abi_encode(),
        ICrosschainBridge::callRemoteCall {
            domain: BSC,
            calls: vec![upgrade_call(0xb1)],
        }
        .abi_encode(),
    ]
    .map(|data| (CROSSCHAIN_BRIDGE_ADDRESS, data));
    assert_eq!(sent, expected);

    let err = apply(&mut p, &payload).unwrap_err();
    assert_eq!(
        revert_selector(err),
        IUpdate::UnknownApprovedUpgrade::SELECTOR
    );
}

#[test]
fn apply_approved_rejects_another_payload_and_drops_an_expired_upgrade() {
    let mut p = HashMapStorageProvider::new(TEST_CHAIN_ID);
    p.enable_sub_call_stub();
    let payload = upgrades(&[(TEST_CHAIN_ID, 0xb1)]);
    approve(&mut p, &payload, standard_created());

    let err = apply(&mut p, &upgrades(&[(u64::from(SEPOLIA), 0xb1)])).unwrap_err();
    assert_eq!(
        revert_selector(err),
        IUpdate::ApprovedPayloadMismatch::SELECTOR
    );

    p.set_block_number(approved_at() + window() + 1);
    apply(&mut p, &payload).unwrap();
    assert!(p.sub_call_inputs().is_empty());
    assert!(apply(&mut p, &payload).is_err());
}

#[test]
fn short_window_upgrade_is_locked_until_one_window_after_the_proposal() {
    let mut p = HashMapStorageProvider::new(TEST_CHAIN_ID);
    p.enable_sub_call_stub();
    let payload = upgrades(&[(TEST_CHAIN_ID, 0xb1)]);
    approve(&mut p, &payload, short_created());
    let approval = stored_approval(&mut p);
    let unlock_height = short_created() + window();

    for block in [approved_at(), unlock_height - 1] {
        p.set_block_number(block);
        let PrecompileError::RevertBytes(bytes) = apply(&mut p, &payload).unwrap_err() else {
            panic!("expected a typed revert");
        };
        let locked = IUpdate::ContractUpgradeLocked::abi_decode(&bytes).unwrap();
        assert_eq!(locked.proposalId, U256::ONE);
        assert_eq!(locked.unlockHeight, unlock_height);
        assert_eq!(stored_approval(&mut p), approval);
        assert!(p.sub_call_inputs().is_empty());
    }

    p.set_block_number(unlock_height);
    apply(&mut p, &payload).unwrap();
    assert_eq!(p.sub_call_inputs().len(), 1);
    assert!(has_event(
        &p,
        IUpdate::ContractUpgradeApplied::SIGNATURE_HASH
    ));
}

#[test]
fn standard_window_upgrade_runs_right_after_the_approval() {
    let mut p = HashMapStorageProvider::new(TEST_CHAIN_ID);
    p.enable_sub_call_stub();
    let payload = upgrades(&[(TEST_CHAIN_ID, 0xb1)]);
    approve(&mut p, &payload, standard_created());

    apply(&mut p, &payload).unwrap();
    assert_eq!(p.sub_call_inputs().len(), 1);
    assert!(has_event(
        &p,
        IUpdate::ContractUpgradeApplied::SIGNATURE_HASH
    ));
}

#[test]
fn upgrade_expiry_counts_one_window_from_the_unlock_height() {
    let payload = upgrades(&[(TEST_CHAIN_ID, 0xb1)]);
    let unlock_height = short_created() + window();

    let mut last_block = HashMapStorageProvider::new(TEST_CHAIN_ID);
    last_block.enable_sub_call_stub();
    approve(&mut last_block, &payload, short_created());
    last_block.set_block_number(unlock_height + window());
    apply(&mut last_block, &payload).unwrap();
    assert_eq!(last_block.sub_call_inputs().len(), 1);

    let mut expired = HashMapStorageProvider::new(TEST_CHAIN_ID);
    expired.enable_sub_call_stub();
    approve(&mut expired, &payload, short_created());
    expired.set_block_number(unlock_height + window() + 1);
    apply(&mut expired, &payload).unwrap();
    assert!(expired.sub_call_inputs().is_empty());
    assert!(has_event(
        &expired,
        IUpdate::ContractUpgradeExpired::SIGNATURE_HASH
    ));
    assert!(!has_event(
        &expired,
        IUpdate::ContractUpgradeApplied::SIGNATURE_HASH
    ));
    assert_eq!(stored_approval(&mut expired), B256::ZERO);
}

#[test]
fn upgrade_without_a_vote_proposal_record_reverts_and_keeps_the_approval() {
    let mut p = HashMapStorageProvider::new(TEST_CHAIN_ID);
    p.enable_sub_call_stub();
    let payload = upgrades(&[(TEST_CHAIN_ID, 0xb1)]);
    approve_without_record(&mut p, &payload);
    let approval = stored_approval(&mut p);

    let err = apply(&mut p, &payload).unwrap_err();
    assert_eq!(
        revert_selector(err),
        IUpdate::VoteTargetInvariantFailure::SELECTOR
    );
    assert_eq!(stored_approval(&mut p), approval);
    assert!(p.sub_call_inputs().is_empty());
}
