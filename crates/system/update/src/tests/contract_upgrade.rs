use super::{block_ctx, TEST_CHAIN_ID};
use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{SolCall, SolError};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::addresses::{CROSSCHAIN_BRIDGE_ADDRESS, UPDATE_ADDRESS};
use outbe_system_common::vote_target::{TargetExecutionOutcome, VoteTarget, VoteTargetContext};

use crate::precompile::{dispatch, IUpdate};
use crate::schema::Update;
use crate::sol_ext::{ICrosschainBridge, IUUPSUpgradeable};
use crate::vote_target::UpdateVoteTarget;

const APPROVED_AT: u64 = 30;
const DELAY: u64 = 100;
const UNLOCK: u64 = APPROVED_AT + DELAY;
const SEPOLIA: u32 = 11_155_111;
const BSC: u32 = 97;

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
        r#"{{"operation":"upgradeContracts","executionDelayBlocks":{DELAY},"upgrades":[{}]}}"#,
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

fn approve(p: &mut HashMapStorageProvider, payload: &str) {
    StorageHandle::enter(p, |storage| {
        let context = VoteTargetContext {
            proposer: Address::repeat_byte(1),
            attached_value: U256::ZERO,
            block_number: APPROVED_AT,
            chain_id: TEST_CHAIN_ID,
        };
        let outcome = UpdateVoteTarget
            .handle_approved(
                &block_ctx(storage, APPROVED_AT),
                U256::ONE,
                payload.as_bytes(),
                context,
            )
            .unwrap();
        assert_eq!(outcome, TargetExecutionOutcome::Applied);
    });
    p.set_block_number(APPROVED_AT);
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
    approve(&mut p, &payload);
    p.set_block_number(UNLOCK);
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
    approve(&mut p, &payload);

    let err = apply(&mut p, &upgrades(&[(u64::from(SEPOLIA), 0xb1)])).unwrap_err();
    assert_eq!(
        revert_selector(err),
        IUpdate::ApprovedPayloadMismatch::SELECTOR
    );

    p.set_block_number(UNLOCK + outbe_chain_constants::get_governance_voting_window_blocks() + 1);
    apply(&mut p, &payload).unwrap();
    assert!(p.sub_call_inputs().is_empty());
    assert!(apply(&mut p, &payload).is_err());
}

#[test]
fn apply_approved_waits_for_the_execution_delay_of_the_payload() {
    let mut p = HashMapStorageProvider::new(TEST_CHAIN_ID);
    p.enable_sub_call_stub();
    let payload = upgrades(&[(TEST_CHAIN_ID, 0xb1)]);
    approve(&mut p, &payload);

    p.set_block_number(UNLOCK - 1);
    let err = apply(&mut p, &payload).unwrap_err();
    let expected = IUpdate::ContractUpgradeLocked {
        proposalId: U256::ONE,
        unlockHeight: UNLOCK,
    }
    .abi_encode();
    assert!(matches!(err, PrecompileError::RevertBytes(ref bytes) if bytes[..] == expected[..]));
    assert!(p.sub_call_inputs().is_empty());

    p.set_block_number(UNLOCK);
    apply(&mut p, &payload).unwrap();
    assert_eq!(p.sub_call_inputs().len(), 1);
}

#[test]
fn vote_step_applies_a_queued_upgrade_at_unlock_and_retries_a_reverted_one() {
    let payload = upgrades(&[(TEST_CHAIN_ID, 0xb1)]);
    let queued = |p: &mut HashMapStorageProvider| {
        StorageHandle::enter(p, |storage| {
            Update::new(storage).queued_contract_upgrades.len().unwrap()
        })
    };
    let run = |p: &mut HashMapStorageProvider, block| {
        StorageHandle::enter(p, |storage| {
            crate::apply_due_contract_upgrades(&storage, block).unwrap()
        })
    };
    let expected = IUpdate::applyApprovedCall {
        proposalId: U256::ONE,
        payload: payload.as_bytes().to_vec().into(),
    }
    .abi_encode();

    // Before the unlock block nothing runs. A revert keeps the batch queued.
    let mut p = HashMapStorageProvider::new(TEST_CHAIN_ID);
    approve(&mut p, &payload);
    write_vote_proposal(&mut p, &payload);
    run(&mut p, UNLOCK - 1);
    assert!(p.sub_call_inputs().is_empty());
    p.stub_sub_call_revert_at(UPDATE_ADDRESS, Bytes::new());
    run(&mut p, UNLOCK);
    assert_eq!(p.sub_call_inputs().len(), 1);
    assert_eq!(p.sub_call_inputs()[0].target, UPDATE_ADDRESS);
    assert_eq!(p.sub_call_inputs()[0].calldata.to_vec(), expected);
    assert_eq!(queued(&mut p), 1);

    // A successful applyApproved removes the batch from the queue.
    let mut p = HashMapStorageProvider::new(TEST_CHAIN_ID);
    p.enable_sub_call_stub();
    approve(&mut p, &payload);
    write_vote_proposal(&mut p, &payload);
    run(&mut p, UNLOCK);
    assert_eq!(queued(&mut p), 0);

    // A batch that another caller applied leaves the queue without a call.
    let mut p = HashMapStorageProvider::new(TEST_CHAIN_ID);
    p.enable_sub_call_stub();
    approve(&mut p, &payload);
    p.set_block_number(UNLOCK);
    apply(&mut p, &payload).unwrap();
    let calls = p.sub_call_inputs().len();
    run(&mut p, UNLOCK);
    assert_eq!(p.sub_call_inputs().len(), calls);
    assert_eq!(queued(&mut p), 0);
}

fn write_vote_proposal(p: &mut HashMapStorageProvider, payload: &str) {
    StorageHandle::enter(p, |storage| {
        outbe_system_common::vote_target::vote_proposals(storage)
            .create(&outbe_system_common::vote_target::ProposalRecord {
                id: U256::ONE,
                proposer: Address::repeat_byte(1),
                target_module: UPDATE_ADDRESS,
                payload: payload.to_string(),
                created_height: 0,
                voting_deadline_height: 0,
                status: outbe_system_common::vote_target::ProposalStatus::Approved.to_u8(),
            })
            .unwrap();
    });
}
