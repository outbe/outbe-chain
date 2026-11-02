//! Each action: JSON in, the expected owner calls out.

use alloy_primitives::{hex, Address, Bytes, U256};
use alloy_sol_types::SolCall;
use outbe_hyperlanecontroller::test_support::{v, RecordingOwner, CHAIN_ID, LOCAL, SEPOLIA};
use outbe_hyperlanecontroller::RemoteCall;

use crate::actions::{Action, Calls};
use crate::sol_ext::{IERC7786Bridge, IHyperlaneGatewayAdapter, ITokenBridge, IUUPSUpgradeable};

const ARBITRUM: u32 = 42_161;

fn calls(json: &str) -> Calls {
    Action::decode(json.as_bytes(), CHAIN_ID)
        .unwrap_or_else(|e| panic!("{json}: {e:?}"))
        .calls(LOCAL)
}

fn call(to: Address, data: Vec<u8>) -> RemoteCall {
    RemoteCall::new(to, data)
}

fn upgrade_json(chain_id: u32, call_data: &str) -> String {
    format!(
        r#"{{"operation":"upgradeContract","chainId":{chain_id},"proxy":"{:#x}","implementation":"{:#x}","callData":"{call_data}"}}"#,
        v(0x70),
        v(0x71)
    )
}

fn upgrade_call(call_data: Vec<u8>) -> RemoteCall {
    call(
        v(0x70),
        IUUPSUpgradeable::upgradeToAndCallCall {
            newImplementation: v(0x71),
            data: call_data.into(),
        }
        .abi_encode(),
    )
}

#[test]
fn upgrade_contract_on_outbe_is_one_local_upgrade_to_and_call() {
    assert_eq!(
        calls(&upgrade_json(LOCAL, "0x")),
        Calls {
            local: vec![upgrade_call(Vec::new())],
            remote: None,
        }
    );
}

#[test]
fn upgrade_contract_elsewhere_is_one_remote_message_with_one_call() {
    let reinitialize = hex!("6c2eb350").to_vec();
    assert_eq!(
        calls(&upgrade_json(SEPOLIA, "0x6c2eb350")),
        Calls {
            local: Vec::new(),
            remote: Some((SEPOLIA, vec![upgrade_call(reinitialize)])),
        }
    );
}

#[test]
fn upgrade_contract_apply_sends_exactly_one_call_remote() {
    let action = Action::decode(upgrade_json(SEPOLIA, "0x").as_bytes(), CHAIN_ID).unwrap();
    let mut owner = RecordingOwner::default();
    action.apply(LOCAL, &mut owner).unwrap();
    assert!(owner.local.is_empty());
    assert_eq!(
        owner.remote,
        vec![(SEPOLIA, vec![upgrade_call(Vec::new())])]
    );

    let action = Action::decode(upgrade_json(LOCAL, "0x").as_bytes(), CHAIN_ID).unwrap();
    let mut owner = RecordingOwner::default();
    action.apply(LOCAL, &mut owner).unwrap();
    let upgrade = upgrade_call(Vec::new());
    assert_eq!(owner.local, vec![(upgrade.to, U256::ZERO, upgrade.data)]);
    assert!(owner.remote.is_empty());
}

#[test]
fn set_bridge_paused_pauses_or_unpauses_on_its_chain() {
    let json = |chain_id: u32, paused: bool| {
        format!(
            r#"{{"operation":"setBridgePaused","chainId":{chain_id},"bridge":"{:#x}","paused":{paused}}}"#,
            v(0xb1)
        )
    };
    let pause = call(v(0xb1), IERC7786Bridge::pauseCall {}.abi_encode());
    let unpause = call(v(0xb1), IERC7786Bridge::unpauseCall {}.abi_encode());
    assert_eq!(calls(&json(LOCAL, true)).local, vec![pause]);
    assert_eq!(
        calls(&json(SEPOLIA, false)).remote,
        Some((SEPOLIA, vec![unpause]))
    );
}

#[test]
fn set_bridge_gateway_sets_the_adapter_for_one_destination() {
    let json = format!(
        r#"{{"operation":"setBridgeGateway","chainId":{SEPOLIA},"bridge":"{:#x}","destinationChainId":{ARBITRUM},"adapter":"{:#x}"}}"#,
        v(0xb1),
        v(0xa1)
    );
    let set = IERC7786Bridge::setGatewayCall {
        chainId: U256::from(ARBITRUM),
        gateway: v(0xa1),
    }
    .abi_encode();
    assert_eq!(
        calls(&json),
        Calls {
            local: Vec::new(),
            remote: Some((SEPOLIA, vec![call(v(0xb1), set)])),
        }
    );
}

#[test]
fn link_bridges_wires_outbe_then_sends_the_mirror_wiring() {
    let json = format!(
        r#"{{"operation":"linkBridges","remoteChainId":{ARBITRUM},"bridge":"{:#x}","adapter":"{:#x}","remoteBridge":"{:#x}","remoteAdapter":"{:#x}"}}"#,
        v(0xb1),
        v(0xa1),
        v(0xb2),
        v(0xa2)
    );
    let side = |(bridge, adapter): (Address, Address),
                other_chain_id: u32,
                (other_bridge, other_adapter): (Address, Address),
                interop: Vec<u8>| {
        let mut erc7930 = interop;
        erc7930.extend_from_slice(other_bridge.as_slice());
        vec![
            call(
                adapter,
                IHyperlaneGatewayAdapter::setRouterWithChainCall {
                    domain: other_chain_id,
                    router: other_adapter.into_word(),
                    chainId: U256::from(other_chain_id),
                }
                .abi_encode(),
            ),
            call(
                bridge,
                IERC7786Bridge::registerRemoteBridgeCall {
                    bridge: Bytes::from(erc7930),
                }
                .abi_encode(),
            ),
            call(
                bridge,
                IERC7786Bridge::setGatewayCall {
                    chainId: U256::from(other_chain_id),
                    gateway: adapter,
                }
                .abi_encode(),
            ),
        ]
    };
    // ERC-7930: version 1, chain type 0, the chain id without leading zero
    // bytes, the address length.
    let arbitrum = hex!("0001000002a4b114").to_vec();
    let mut outbe = hex!("00010000").to_vec();
    let id = LOCAL.to_be_bytes();
    let id = &id[id.iter().position(|b| *b != 0).unwrap()..];
    outbe.push(id.len() as u8);
    outbe.extend_from_slice(id);
    outbe.push(20);
    assert_eq!(
        calls(&json),
        Calls {
            local: side((v(0xb1), v(0xa1)), ARBITRUM, (v(0xb2), v(0xa2)), arbitrum),
            remote: Some((
                ARBITRUM,
                side((v(0xb2), v(0xa2)), LOCAL, (v(0xb1), v(0xa1)), outbe)
            )),
        }
    );
}

#[test]
fn link_token_bridges_adds_each_bridge_as_the_peer_of_the_other() {
    let json = format!(
        r#"{{"operation":"linkTokenBridges","remoteChainId":{ARBITRUM},"tokenBridge":"{:#x}","remoteTokenBridge":"{:#x}"}}"#,
        v(0xc1),
        v(0xc2)
    );
    let add_peer = |bridge: Address, domain: u32, peer: Address| {
        call(
            bridge,
            ITokenBridge::addPeerCall {
                domain,
                peerBridge: peer,
            }
            .abi_encode(),
        )
    };
    assert_eq!(
        calls(&json),
        Calls {
            local: vec![add_peer(v(0xc1), ARBITRUM, v(0xc2))],
            remote: Some((ARBITRUM, vec![add_peer(v(0xc2), LOCAL, v(0xc1))])),
        }
    );
}

#[test]
fn upgrade_contract_rejects_zero_fields_and_unknown_fields() {
    let zero = format!("{:#x}", Address::ZERO);
    let proxy = format!("{:#x}", v(0x70));
    let implementation = format!("{:#x}", v(0x71));
    let invalid = [
        upgrade_json(0, "0x"),
        upgrade_json(LOCAL, "0x").replace(&proxy, &zero),
        upgrade_json(LOCAL, "0x").replace(&implementation, &zero),
        upgrade_json(LOCAL, "0x").replace('}', r#","value":1}"#),
        upgrade_json(LOCAL, "0x").replace(r#","callData":"0x""#, ""),
        upgrade_json(LOCAL, "0xzz"),
    ];
    for payload in &invalid {
        assert!(
            Action::decode(payload.as_bytes(), CHAIN_ID).is_err(),
            "{payload}"
        );
    }
}
