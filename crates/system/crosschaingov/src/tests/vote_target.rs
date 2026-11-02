//! The vote target: validation, dispatch to the module, and `applyApproved`.

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolError, SolValue};
use outbe_hyperlanecontroller::sol_ext::IHyperlaneRouter;
use outbe_hyperlanecontroller::test_support::*;
use outbe_hyperlanecontroller::HyperlaneAction;
use outbe_hyperlanecontroller::HyperlaneControllerContract;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_vote::handlers::{TargetExecutionOutcome, VoteTarget, VoteTargetContext};

use super::{dispatch, remote_calls, sub_calls_from};
use crate::actions::Action;
use crate::precompile::ICrosschainGov;
use crate::sol_ext::{IERC7786Bridge, IUUPSUpgradeable};
use crate::vote_target::Route;
use crate::CrosschainVoteTarget;

const APPROVED_AT: u64 = 30;
const ARBITRUM: u32 = 42_161;

fn bridge() -> Address {
    v(0xb1)
}

fn context() -> VoteTargetContext {
    VoteTargetContext {
        proposer: v(1),
        attached_value: U256::ZERO,
        block_number: APPROVED_AT,
        chain_id: CHAIN_ID,
    }
}

fn pause_on(chain_id: u32) -> String {
    format!(
        r#"{{"operation":"setBridgePaused","chainId":{chain_id},"bridge":"{:#x}","paused":true}}"#,
        bridge()
    )
}

fn link_arbitrum() -> String {
    format!(
        r#"{{"operation":"linkBridges","remoteChainId":{ARBITRUM},"bridge":"{:#x}","adapter":"{:#x}","remoteBridge":"{:#x}","remoteAdapter":"{:#x}"}}"#,
        bridge(),
        v(0xa1),
        v(0xb2),
        v(0xa2)
    )
}

fn add_arbitrum() -> String {
    format!(
        r#"{{"operation":"addHyperlaneChain","chainId":{ARBITRUM},"icaRouter":"{:#x}","ism":"{:#x}","hook":"{:#x}"}}"#,
        v(0xe1),
        v(0xd1),
        hook(9)
    )
}

fn upgrade_on(chain_id: u32) -> String {
    format!(
        r#"{{"operation":"upgradeContract","chainId":{chain_id},"proxy":"{:#x}","implementation":"{:#x}","callData":"0x"}}"#,
        v(0x70),
        v(0x71)
    )
}

fn approve(p: &mut HashMapStorageProvider, payload: &str) {
    StorageHandle::enter(p, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(APPROVED_AT, 1_700_000_000, CHAIN_ID),
            storage,
        );
        assert_eq!(
            CrosschainVoteTarget
                .handle_approved(&ctx, U256::ONE, payload.as_bytes(), context())
                .unwrap(),
            TargetExecutionOutcome::Applied
        );
    });
    p.set_block_number(APPROVED_AT);
}

fn apply(p: &mut HashMapStorageProvider, payload: &str) -> Result<Bytes, PrecompileError> {
    let call = ICrosschainGov::applyApprovedCall {
        proposalId: U256::ONE,
        payload: payload.as_bytes().to_vec().into(),
    };
    StorageHandle::enter(p, |storage| {
        dispatch(storage, &call.abi_encode(), stranger())
    })
}

#[test]
fn target_module_is_the_precompile() {
    assert_eq!(
        CrosschainVoteTarget.target_module(),
        outbe_primitives::addresses::CROSSCHAIN_GOV_ADDRESS
    );
}

#[test]
fn hyperlane_operations_go_to_the_hyperlane_controller_and_the_rest_here() {
    let route = |payload: &str| Route::decode(payload.as_bytes(), CHAIN_ID).unwrap();
    assert!(matches!(
        route(&add_arbitrum()),
        Route::Hyperlane(HyperlaneAction::AddHyperlaneChain { .. })
    ));
    assert!(matches!(
        route(&format!(
            r#"{{"operation":"removeHyperlaneChain","chainId":{ARBITRUM}}}"#
        )),
        Route::Hyperlane(HyperlaneAction::RemoveHyperlaneChain { .. })
    ));
    assert!(matches!(
        route(&upgrade_on(SEPOLIA)),
        Route::Gov(Action::UpgradeContract(_))
    ));
    assert!(matches!(
        route(&pause_on(LOCAL)),
        Route::Gov(Action::SetBridgePaused(_))
    ));
}

#[test]
fn validate_accepts_every_action_and_rejects_bad_fields() {
    let valid = [
        pause_on(LOCAL),
        pause_on(SEPOLIA).replace("true", "false"),
        format!(
            r#"{{"operation":"setBridgeGateway","chainId":{LOCAL},"bridge":"{:#x}","destinationChainId":{ARBITRUM},"adapter":"{:#x}"}}"#,
            bridge(),
            Address::ZERO
        ),
        link_arbitrum(),
        format!(
            r#"{{"operation":"linkTokenBridges","remoteChainId":{ARBITRUM},"tokenBridge":"{:#x}","remoteTokenBridge":"{:#x}"}}"#,
            v(0xc1),
            v(0xc2)
        ),
        add_arbitrum(),
        format!(r#"{{"operation":"removeHyperlaneChain","chainId":{ARBITRUM}}}"#),
        upgrade_on(LOCAL),
        upgrade_on(SEPOLIA).replace(r#""0x""#, r#""0x8129fc1c""#),
    ];
    for payload in &valid {
        CrosschainVoteTarget
            .validate(payload.as_bytes(), context())
            .unwrap_or_else(|e| panic!("{payload}: {e:?}"));
    }
    let invalid = [
        "{}".to_string(),
        "[]".to_string(),
        pause_on(LOCAL).replace("setBridgePaused", "callLocal"),
        pause_on(LOCAL).replace(
            &format!("{:#x}", bridge()),
            &format!("{:#x}", Address::ZERO),
        ),
        pause_on(LOCAL).replace(r#""paused":true"#, r#""paused":true,"paused":false"#),
        pause_on(LOCAL).replace('}', r#","value":1}"#),
        pause_on(LOCAL).replace('}', r#","operation":"setBridgePaused"}"#),
        link_arbitrum().replace(&ARBITRUM.to_string(), &LOCAL.to_string()),
        link_arbitrum().replace('}', r#","transport":"layerZero"}"#),
        add_arbitrum().replace('}', r#","proxy":"0x01"}"#),
        format!(r#"{{"operation":"removeHyperlaneChain","chainId":{LOCAL}}}"#),
        upgrade_on(0),
        upgrade_on(LOCAL).replace(&format!("{:#x}", v(0x71)), &format!("{:#x}", Address::ZERO)),
    ];
    for payload in &invalid {
        assert!(
            CrosschainVoteTarget
                .validate(payload.as_bytes(), context())
                .is_err(),
            "{payload}"
        );
    }
}

#[test]
fn link_bridges_wires_outbe_then_sends_the_mirror_wiring_once() {
    let mut p = initialized_provider();
    let payload = link_arbitrum();
    approve(&mut p, &payload);
    let start = p.sub_call_inputs().len();
    apply(&mut p, &payload).unwrap();

    let sent = sub_calls_from(&p, start);
    assert_eq!(sent.len(), 4);
    let targets: Vec<Address> = sent[..3].iter().map(|(to, _)| *to).collect();
    assert_eq!(targets, [v(0xa1), bridge(), bridge()]);
    assert_eq!(sent[3].0, ica_router());
    let (destination, mirror) = remote_calls(&sent[3].1);
    assert_eq!(destination, ARBITRUM);
    let targets: Vec<Address> = mirror.iter().map(|(to, _)| *to).collect();
    assert_eq!(targets, [v(0xa2), v(0xb2), v(0xb2)]);

    let err = apply(&mut p, &payload).unwrap_err();
    assert_eq!(
        revert_selector(err),
        ICrosschainGov::UnknownApprovedAction::SELECTOR
    );
}

#[test]
fn single_chain_action_runs_locally_or_as_one_remote_message() {
    let mut p = initialized_provider();
    approve(&mut p, &pause_on(LOCAL));
    let start = p.sub_call_inputs().len();
    apply(&mut p, &pause_on(LOCAL)).unwrap();
    let pause = Bytes::from(IERC7786Bridge::pauseCall {}.abi_encode());
    assert_eq!(sub_calls_from(&p, start), [(bridge(), pause.clone())]);

    let mut p = initialized_provider();
    approve(&mut p, &pause_on(SEPOLIA));
    let start = p.sub_call_inputs().len();
    apply(&mut p, &pause_on(SEPOLIA)).unwrap();
    let sent = sub_calls_from(&p, start);
    assert_eq!(sent.len(), 1);
    assert_eq!(remote_calls(&sent[0].1), (SEPOLIA, vec![(bridge(), pause)]));
}

#[test]
fn upgrade_contract_to_a_remote_chain_is_one_call_remote_with_one_upgrade() {
    let mut p = initialized_provider();
    approve(&mut p, &upgrade_on(SEPOLIA));
    let start = p.sub_call_inputs().len();
    apply(&mut p, &upgrade_on(SEPOLIA)).unwrap();
    let sent = sub_calls_from(&p, start);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, ica_router());
    let upgrade = Bytes::from(
        IUUPSUpgradeable::upgradeToAndCallCall {
            newImplementation: v(0x71),
            data: Bytes::new(),
        }
        .abi_encode(),
    );
    assert_eq!(
        remote_calls(&sent[0].1),
        (SEPOLIA, vec![(v(0x70), upgrade)])
    );
}

#[test]
fn upgrade_contract_on_outbe_calls_the_proxy_from_the_precompile() {
    let mut p = initialized_provider();
    approve(&mut p, &upgrade_on(LOCAL));
    let start = p.sub_call_inputs().len();
    apply(&mut p, &upgrade_on(LOCAL)).unwrap();
    let input = &p.sub_call_inputs()[start..]
        .iter()
        .find(|input| !input.is_static)
        .unwrap()
        .clone();
    assert_eq!(input.target, v(0x70));
    assert_eq!(
        IUUPSUpgradeable::upgradeToAndCallCall::abi_decode(&input.calldata)
            .unwrap()
            .newImplementation,
        v(0x71)
    );
}

#[test]
fn add_hyperlane_chain_enrolls_the_ica_router_once_then_adds_the_domain() {
    let add = add_arbitrum();
    let enrolled = |p: &mut HashMapStorageProvider, router: B256| {
        p.stub_sub_call_at_selector(
            ica_router(),
            IHyperlaneRouter::routersCall::SELECTOR,
            Bytes::from(router.abi_encode()),
        );
    };
    let enroll = Bytes::from(
        IHyperlaneRouter::enrollRemoteRouterCall {
            _domain: ARBITRUM,
            _router: v(0xe1).into_word(),
        }
        .abi_encode(),
    );

    // A new chain: enroll, then accept the remote ISM through the ICA.
    let mut p = initialized_provider();
    enrolled(&mut p, B256::ZERO);
    approve(&mut p, &add);
    let start = p.sub_call_inputs().len();
    apply(&mut p, &add).unwrap();
    let sent = sub_calls_from(&p, start);
    assert_eq!(sent[0], (ica_router(), enroll));
    assert_eq!(remote_calls(&sent[1].1).0, ARBITRUM);
    StorageHandle::enter(&mut p, |storage| {
        let c = HyperlaneControllerContract::new(storage);
        assert_eq!(c.ism_by_domain.read(&ARBITRUM).unwrap(), v(0xd1));
    });

    // The deployer enrolled the same router: no second enrollment.
    let mut p = initialized_provider();
    enrolled(&mut p, v(0xe1).into_word());
    approve(&mut p, &add);
    let start = p.sub_call_inputs().len();
    apply(&mut p, &add).unwrap();
    assert_eq!(sub_calls_from(&p, start).len(), 1);

    // Another router is enrolled: nothing applies.
    let mut p = initialized_provider();
    enrolled(&mut p, v(0xe2).into_word());
    approve(&mut p, &add);
    let err = apply(&mut p, &add).unwrap_err();
    assert_eq!(
        revert_selector(err),
        ICrosschainGov::RemoteRouterMismatch::SELECTOR
    );
}

#[test]
fn apply_rejects_another_payload_and_drops_an_expired_action() {
    let mut p = initialized_provider();
    let payload = pause_on(LOCAL);
    approve(&mut p, &payload);

    let err = apply(&mut p, &pause_on(SEPOLIA)).unwrap_err();
    assert_eq!(
        revert_selector(err),
        ICrosschainGov::ApprovedPayloadMismatch::SELECTOR
    );

    p.set_block_number(
        APPROVED_AT + outbe_chain_constants::get_governance_voting_window_blocks() + 1,
    );
    let start = p.sub_call_inputs().len();
    apply(&mut p, &payload).unwrap();
    assert!(sub_calls_from(&p, start).is_empty());
    assert!(apply(&mut p, &payload).is_err());
}
