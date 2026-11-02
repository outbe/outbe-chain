//! Unit tests for the CrosschainGov precompile.
//!
//! Cross-contract interaction goes through `HashMapStorageProvider`'s
//! sub-call stubs. `stub_sub_call_at_selector` pins a target's reply per
//! selector. `enable_sub_call_stub()` makes every other sub-call succeed
//! with empty returndata. The Hyperlane registry comes from the
//! HyperlaneController precompile (`hub_dispatch`).

use alloy_primitives::{address, Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolError, SolEvent, SolValue};
use outbe_hyperlanecontroller::precompile::{dispatch as hub_dispatch, IHyperlaneController};
use outbe_hyperlanecontroller::sol_ext::{IInterchainAccountRouter, IOwnable, IStorageMultisigIsm};
use outbe_hyperlanecontroller::HyperlaneControllerContract;

use crate::calls::RemoteCall;
use outbe_primitives::addresses::{CROSSCHAIN_GOV_ADDRESS, HYPERLANE_CONTROLLER_ADDRESS};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::executor::Executor;
use crate::precompile::{dispatch, ICrosschainGov};

const CHAIN_ID: u64 = 54_322_345;
const LOCAL: u32 = CHAIN_ID as u32;
const SEPOLIA: u32 = 11_155_111;
const BSC: u32 = 97;

fn deployer() -> Address {
    address!("0x00000000000000000000000000000000000000d1")
}
fn stranger() -> Address {
    address!("0x00000000000000000000000000000000000000b0")
}
fn ica_router() -> Address {
    address!("0x0000000000000000000000000000000000000626")
}
fn validator_announce() -> Address {
    address!("0x0000000000000000000000000000000000000627")
}
fn local_ism() -> Address {
    address!("0x0000000000000000000000000000000000000151")
}
fn sepolia_ism() -> Address {
    address!("0x0000000000000000000000000000000000000152")
}
fn bsc_ism() -> Address {
    address!("0x0000000000000000000000000000000000000153")
}
fn v(n: u8) -> Address {
    Address::repeat_byte(n)
}

fn word_addr(value: Address) -> Bytes {
    Bytes::from(value.abi_encode())
}

fn provider() -> HashMapStorageProvider {
    HashMapStorageProvider::new(CHAIN_ID)
}

/// Deployer still owns the local ISM, controller is its pending owner and
/// already owns the ICA router: the state right after `mise run owner:transfer`.
/// The router quotes a zero fee so `initialize` can dispatch the remote
/// `acceptOwnership()` calls without a balance.
fn stub_pre_initialize(p: &mut HashMapStorageProvider) {
    StorageHandle::enter(p, |storage| {
        HyperlaneControllerContract::new(storage)
            .bootstrap_admin
            .write(deployer())
            .unwrap();
    });
    p.stub_sub_call_at_selector(
        ica_router(),
        IInterchainAccountRouter::quoteGasPaymentCall::SELECTOR,
        Bytes::from(U256::ZERO.abi_encode()),
    );
    p.stub_sub_call_at_selector(
        ica_router(),
        IInterchainAccountRouter::callRemoteCall::SELECTOR,
        Bytes::from(B256::repeat_byte(0xaa).abi_encode()),
    );
    p.stub_sub_call_at_selector(
        local_ism(),
        IOwnable::ownerCall::SELECTOR,
        word_addr(deployer()),
    );
    p.stub_sub_call_at_selector(
        local_ism(),
        IStorageMultisigIsm::pendingOwnerCall::SELECTOR,
        word_addr(CROSSCHAIN_GOV_ADDRESS),
    );
    p.stub_sub_call_at_selector(
        ica_router(),
        IOwnable::ownerCall::SELECTOR,
        word_addr(CROSSCHAIN_GOV_ADDRESS),
    );
    p.enable_sub_call_stub();
}

fn hook(n: u8) -> Address {
    let mut bytes = [0u8; 20];
    bytes[18] = 0x0a;
    bytes[19] = n;
    Address::from(bytes)
}

fn initialize_call() -> Bytes {
    IHyperlaneController::initializeCall {
        icaRouter: ica_router(),
        validatorAnnounce: validator_announce(),
        domains: vec![LOCAL, SEPOLIA, BSC],
        isms: vec![local_ism(), sepolia_ism(), bsc_ism()],
        hooks: vec![hook(1), hook(2), hook(3)],
    }
    .abi_encode()
    .into()
}

fn initialized_provider() -> HashMapStorageProvider {
    let mut p = provider();
    stub_pre_initialize(&mut p);
    StorageHandle::enter(&mut p, |storage| {
        hub_dispatch(storage, &initialize_call(), deployer(), U256::ZERO).unwrap();
    });
    p
}

fn stub_router_and_ism(
    p: &mut HashMapStorageProvider,
    fee: U256,
    validators: &[Address],
    threshold: u8,
) {
    p.stub_sub_call_at_selector(
        ica_router(),
        IInterchainAccountRouter::quoteGasPaymentCall::SELECTOR,
        Bytes::from(fee.abi_encode()),
    );
    p.stub_sub_call_at_selector(
        ica_router(),
        IInterchainAccountRouter::callRemoteCall::SELECTOR,
        Bytes::from(B256::repeat_byte(0xaa).abi_encode()),
    );
    p.stub_sub_call_at_selector(
        local_ism(),
        IStorageMultisigIsm::validatorsAndThresholdCall::SELECTOR,
        Bytes::from(
            IStorageMultisigIsm::validatorsAndThresholdCall::abi_encode_returns(
                &IStorageMultisigIsm::validatorsAndThresholdReturn {
                    _0: validators.to_vec(),
                    _1: threshold,
                },
            ),
        ),
    );
}

fn topics(p: &HashMapStorageProvider) -> Vec<B256> {
    p.get_events(CROSSCHAIN_GOV_ADDRESS)
        .iter()
        .map(|log| log.topics()[0])
        .collect()
}

fn revert_selector(err: outbe_primitives::error::PrecompileError) -> [u8; 4] {
    match err {
        outbe_primitives::error::PrecompileError::RevertBytes(bytes) if bytes.len() >= 4 => {
            bytes[..4].try_into().expect("selector")
        }
        other => panic!("expected typed revert, got {other:?}"),
    }
}

#[test]
fn fund_is_the_only_payable_selector() {
    let mut p = initialized_provider();
    StorageHandle::enter(&mut p, |storage| {
        let fund: Bytes = ICrosschainGov::fundCall {}.abi_encode().into();
        dispatch(storage.clone(), &fund, stranger(), U256::from(5)).unwrap();
        assert!(dispatch(storage.clone(), &fund, stranger(), U256::ZERO).is_err());
        let view: Bytes = ICrosschainGov::remoteAccountCall { domain: SEPOLIA }
            .abi_encode()
            .into();
        assert!(dispatch(storage, &view, stranger(), U256::from(1)).is_err());
    });
    assert_eq!(
        *topics(&p).last().unwrap(),
        ICrosschainGov::Funded::SIGNATURE_HASH
    );
}

#[test]
fn a_remote_call_without_fee_balance_changes_nothing() {
    let mut p = initialized_provider();
    stub_router_and_ism(&mut p, U256::from(7), &[v(1)], 1);
    p.clear_events(CROSSCHAIN_GOV_ADDRESS);
    StorageHandle::enter(&mut p, |storage| {
        let err = Executor::new(storage)
            .call_remote(SEPOLIA, &[RemoteCall::new(sepolia_ism(), Bytes::new())])
            .unwrap_err();
        assert_eq!(
            revert_selector(err),
            ICrosschainGov::InsufficientBalance::SELECTOR
        );
    });
    assert!(topics(&p).is_empty());
}

#[test]
fn remote_and_local_generic_calls() {
    let mut p = initialized_provider();
    stub_router_and_ism(&mut p, U256::ZERO, &[v(1)], 1);
    StorageHandle::enter(&mut p, |storage| {
        let mut executor = Executor::new(storage);
        let calls = [RemoteCall {
            to: sepolia_ism(),
            value: U256::ZERO,
            data: IStorageMultisigIsm::acceptOwnershipCall {}
                .abi_encode()
                .into(),
        }];
        assert_eq!(
            executor.call_remote(SEPOLIA, &calls).unwrap(),
            B256::repeat_byte(0xaa)
        );
        assert!(executor.call_remote(LOCAL, &calls).is_err());
        assert!(executor.call_remote(SEPOLIA, &[]).is_err());

        executor
            .call_local(local_ism(), U256::ZERO, Bytes::from_static(&[1, 2, 3, 4]))
            .unwrap();
        assert!(executor
            .call_local(local_ism(), U256::from(1), Bytes::new())
            .is_err());
    });
}

#[test]
fn only_an_owner_caller_may_make_owner_calls() {
    let mut p = initialized_provider();
    let start = p.sub_call_inputs().len();
    let call: Bytes = ICrosschainGov::callLocalCall {
        call: ICrosschainGov::Call {
            to: local_ism(),
            value: U256::ZERO,
            data: Bytes::from_static(&[1, 2, 3, 4]),
        },
    }
    .abi_encode()
    .into();
    StorageHandle::enter(&mut p, |storage| {
        let err = dispatch(storage.clone(), &call, stranger(), U256::ZERO).unwrap_err();
        assert_eq!(
            revert_selector(err),
            ICrosschainGov::NotOwnerCaller::SELECTOR
        );
        dispatch(storage, &call, HYPERLANE_CONTROLLER_ADDRESS, U256::ZERO).unwrap();
    });
    let sent: Vec<_> = p.sub_call_inputs()[start..]
        .iter()
        .filter(|input| !input.is_static)
        .map(|input| (input.target, input.calldata.clone()))
        .collect();
    assert_eq!(sent, [(local_ism(), Bytes::from_static(&[1, 2, 3, 4]))]);
}

#[test]
fn remote_account_is_the_router_derived_account() {
    let mut p = initialized_provider();
    p.stub_sub_call_at_selector(
        ica_router(),
        IInterchainAccountRouter::getRemoteInterchainAccountCall::SELECTOR,
        word_addr(v(0x1c)),
    );
    StorageHandle::enter(&mut p, |storage| {
        let call = ICrosschainGov::remoteAccountCall { domain: SEPOLIA };
        let ret = dispatch(storage, &call.abi_encode(), stranger(), U256::ZERO).unwrap();
        let account = ICrosschainGov::remoteAccountCall::abi_decode_returns(&ret).unwrap();
        assert_eq!(account, v(0x1c));
    });
}

mod vote_target {
    use super::*;
    use crate::vote_target::crosschain::{IERC7786Bridge, IHyperlaneGatewayAdapter};
    use crate::CrosschainVoteTarget;
    use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
    use outbe_vote::handlers::{TargetExecutionOutcome, VoteTarget, VoteTargetContext};

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

    fn upgrade_on(chain_id: u32, call_data: &str) -> String {
        format!(
            r#"{{"operation":"upgradeContract","chainId":{chain_id},"proxy":"{:#x}","implementation":"{:#x}","callData":"{call_data}"}}"#,
            bridge(),
            v(0x9a)
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
            dispatch(storage, &call.abi_encode(), stranger(), U256::ZERO)
        })
    }

    fn sub_calls_from(p: &HashMapStorageProvider, start: usize) -> Vec<(Address, Bytes)> {
        p.sub_call_inputs()[start..]
            .iter()
            .filter(|input| !input.is_static)
            .map(|input| (input.target, input.calldata.clone()))
            .collect()
    }

    fn remote_calls(calldata: &Bytes) -> (u32, Vec<(Address, Bytes)>) {
        let call = IInterchainAccountRouter::callRemoteCall::abi_decode(calldata).unwrap();
        let calls = call
            ._calls
            .into_iter()
            .map(|c| (Address::from_word(c.to), c.data))
            .collect();
        (call._destination, calls)
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
            format!(
                r#"{{"operation":"addHyperlaneChain","chainId":{ARBITRUM},"icaRouter":"{:#x}","ism":"{:#x}","hook":"{:#x}"}}"#,
                v(0xe1),
                v(0xd1),
                hook(9)
            ),
            format!(r#"{{"operation":"removeHyperlaneChain","chainId":{ARBITRUM}}}"#),
            upgrade_on(LOCAL, "0x"),
            upgrade_on(ARBITRUM, "0x8129fc1c"),
        ];
        for payload in &valid {
            CrosschainVoteTarget
                .validate(payload.as_bytes(), context())
                .unwrap_or_else(|e| panic!("{payload}: {e:?}"));
        }
        let invalid = [
            "{}".to_string(),
            pause_on(LOCAL).replace("setBridgePaused", "callLocal"),
            pause_on(LOCAL).replace(
                &format!("{:#x}", bridge()),
                &format!("{:#x}", Address::ZERO),
            ),
            pause_on(LOCAL).replace(r#""paused":true"#, r#""paused":true,"paused":false"#),
            pause_on(LOCAL).replace('}', r#","value":1}"#),
            link_arbitrum().replace(&ARBITRUM.to_string(), &LOCAL.to_string()),
            link_arbitrum().replace('}', r#","transport":"layerZero"}"#),
            format!(r#"{{"operation":"removeHyperlaneChain","chainId":{LOCAL}}}"#),
            upgrade_on(0, "0x"),
            upgrade_on(LOCAL, "0x")
                .replace(&format!("{:#x}", v(0x9a)), &format!("{:#x}", Address::ZERO)),
            upgrade_on(LOCAL, "0x").replace(r#","callData":"0x""#, ""),
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

        // ERC-7930: version 1, chain type 0, chain id 42161 (0xa4b1) in two bytes, the address.
        let mut arbitrum_bridge = alloy_primitives::hex!("0001000002a4b114").to_vec();
        arbitrum_bridge.extend_from_slice(v(0xb2).as_slice());
        let gateway = |chain_id: u32, adapter: Address| {
            Bytes::from(
                IERC7786Bridge::setGatewayCall {
                    chainId: U256::from(chain_id),
                    gateway: adapter,
                }
                .abi_encode(),
            )
        };
        let sent = sub_calls_from(&p, start);
        assert_eq!(sent.len(), 4);
        assert_eq!(
            sent[..3],
            [
                (
                    v(0xa1),
                    Bytes::from(
                        IHyperlaneGatewayAdapter::setRouterWithChainCall {
                            domain: ARBITRUM,
                            router: v(0xa2).into_word(),
                            chainId: U256::from(ARBITRUM),
                        }
                        .abi_encode(),
                    ),
                ),
                (
                    bridge(),
                    Bytes::from(
                        IERC7786Bridge::registerRemoteBridgeCall {
                            bridge: arbitrum_bridge.into(),
                        }
                        .abi_encode(),
                    ),
                ),
                (bridge(), gateway(ARBITRUM, v(0xa1))),
            ]
        );
        assert_eq!(sent[3].0, ica_router());
        let (destination, mirror) = remote_calls(&sent[3].1);
        assert_eq!(destination, ARBITRUM);
        let targets: Vec<Address> = mirror.iter().map(|(to, _)| *to).collect();
        assert_eq!(targets, [v(0xa2), v(0xb2), v(0xb2)]);
        assert_eq!(mirror[2].1, gateway(LOCAL, v(0xa2)));

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
    fn upgrade_contract_upgrades_the_proxy_locally_or_through_the_remote_account() {
        let upgrade = Bytes::from(
            crate::vote_target::upgrade::IUUPSUpgradeable::upgradeToAndCallCall {
                newImplementation: v(0x9a),
                data: Bytes::from_static(&[0x81, 0x29, 0xfc, 0x1c]),
            }
            .abi_encode(),
        );

        let mut p = initialized_provider();
        approve(&mut p, &upgrade_on(LOCAL, "0x8129fc1c"));
        let start = p.sub_call_inputs().len();
        apply(&mut p, &upgrade_on(LOCAL, "0x8129fc1c")).unwrap();
        assert_eq!(sub_calls_from(&p, start), [(bridge(), upgrade.clone())]);

        let mut p = initialized_provider();
        approve(&mut p, &upgrade_on(SEPOLIA, "0x8129fc1c"));
        let start = p.sub_call_inputs().len();
        apply(&mut p, &upgrade_on(SEPOLIA, "0x8129fc1c")).unwrap();
        let sent = sub_calls_from(&p, start);
        assert_eq!(sent.len(), 1);
        assert_eq!(
            remote_calls(&sent[0].1),
            (SEPOLIA, vec![(bridge(), upgrade)])
        );
    }

    #[test]
    fn add_hyperlane_chain_enrolls_the_ica_router_once_then_adds_the_domain() {
        let add = format!(
            r#"{{"operation":"addHyperlaneChain","chainId":{ARBITRUM},"icaRouter":"{:#x}","ism":"{:#x}","hook":"{:#x}"}}"#,
            v(0xe1),
            v(0xd1),
            hook(9)
        );
        let enrolled = |p: &mut HashMapStorageProvider, router: B256| {
            p.stub_sub_call_at_selector(
                ica_router(),
                outbe_hyperlanecontroller::sol_ext::IHyperlaneRouter::routersCall::SELECTOR,
                Bytes::from(router.abi_encode()),
            );
        };
        let enroll = Bytes::from(
            outbe_hyperlanecontroller::sol_ext::IHyperlaneRouter::enrollRemoteRouterCall {
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
        // The hub methods call back into this precompile for each owner call.
        let sent = sub_calls_from(&p, start);
        assert_eq!(sent[0].0, CROSSCHAIN_GOV_ADDRESS);
        let local = ICrosschainGov::callLocalCall::abi_decode(&sent[0].1).unwrap();
        assert_eq!((local.call.to, local.call.data), (ica_router(), enroll));
        assert_eq!(sent[1].0, CROSSCHAIN_GOV_ADDRESS);
        let remote = ICrosschainGov::callRemoteCall::abi_decode(&sent[1].1).unwrap();
        assert_eq!(remote.domain, ARBITRUM);
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
            IHyperlaneController::RemoteRouterMismatch::SELECTOR
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
}
