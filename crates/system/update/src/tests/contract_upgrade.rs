use super::{block_ctx, TEST_CHAIN_ID};
use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{SolCall, SolError};
use outbe_primitives::addresses::CROSSCHAIN_BRIDGE_ADDRESS;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::vote_target::{TargetExecutionOutcome, VoteTarget, VoteTargetContext};

use crate::precompile::{dispatch, IUpdate};
use crate::sol_ext::{ICrosschainBridge, IUUPSUpgradeable};
use crate::vote_target::UpdateVoteTarget;

const APPROVED_AT: u64 = 30;
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

    p.set_block_number(
        APPROVED_AT + outbe_chain_constants::get_governance_voting_window_blocks() + 1,
    );
    apply(&mut p, &payload).unwrap();
    assert!(p.sub_call_inputs().is_empty());
    assert!(apply(&mut p, &payload).is_err());
}
