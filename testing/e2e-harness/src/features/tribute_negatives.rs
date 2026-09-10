//! Exact rejection evidence for real CLI-produced encrypted offers. CE state is
//! replayed at the receipt block and observed live. An unavailable historical
//! CE context is a failure, never replaced by the later live observation.

use std::time::{Duration, Instant};

use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{Revert, SolCall, SolError};
use outbe_primitives::addresses::TRIBUTE_FACTORY_ADDRESS;

use crate::{
    internal::{
        addresses::TRIBUTE_ADDR,
        eth::{self, ITribute, ITributeFactory},
    },
    world::World,
};

pub(super) enum Rejection {
    Duplicate,
    MissingSignature,
    InvalidProof,
}

impl Rejection {
    fn reason(&self) -> &'static str {
        match self {
            Self::Duplicate => "tribute already exists for this combination of parameters",
            Self::MissingSignature => "invalid BLS signature over zkMerkleRoot",
            Self::InvalidProof => "ZK proof verification failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ObservedHeader {
    height: u64,
    hash: B256,
    state_root: B256,
}

fn quantity(object: &serde_json::Value, field: &str) -> U256 {
    U256::from_str_radix(
        object[field]
            .as_str()
            .expect("RPC quantity")
            .trim_start_matches("0x"),
        16,
    )
    .expect("valid RPC quantity")
}

fn latest_header(world: &World, port: u16) -> ObservedHeader {
    let block = eth::raw_json_result(
        &world.rpc.url(port),
        "eth_getBlockByNumber",
        serde_json::json!(["latest", false]),
    )
    .expect("live observation header");
    ObservedHeader {
        height: quantity(&block, "number").try_into().expect("u64 height"),
        hash: block["hash"]
            .as_str()
            .expect("block hash")
            .parse()
            .expect("B256 hash"),
        state_root: block["stateRoot"]
            .as_str()
            .expect("state root")
            .parse()
            .expect("B256 root"),
    }
}

pub(super) fn assert_rejection(world: &World, tx_hash: &str, key: &str, rejection: Rejection) {
    assert!(
        world.rpc.wait_receipt_status(tx_hash, false, 120),
        "negative offer must be mined and reverted"
    );
    let ports = world.validators.committee_ports();
    assert!(!ports.is_empty());
    let url = world.rpc.url(world.validators.primary_port());
    let receipt = eth::receipt_json(&url, tx_hash).expect("negative offer receipt");
    let transaction = eth::raw_json_result(
        &url,
        "eth_getTransactionByHash",
        serde_json::json!([tx_hash]),
    )
    .expect("negative offer transaction");
    let caller = eth::address_of(key).expect("offer signer");
    assert_eq!(
        transaction["from"]
            .as_str()
            .unwrap()
            .parse::<Address>()
            .unwrap(),
        caller
    );
    assert_eq!(
        transaction["to"]
            .as_str()
            .unwrap()
            .parse::<Address>()
            .unwrap(),
        TRIBUTE_FACTORY_ADDRESS
    );
    assert_eq!(quantity(&transaction, "value"), U256::ZERO);
    let gas: u64 = quantity(&transaction, "gas").try_into().expect("gas limit");
    assert_eq!(gas, 8_000_000, "production CLI fixed gas limit");
    assert!(
        quantity(&receipt, "gasUsed") < U256::from(gas),
        "out-of-gas is not guard evidence"
    );
    let data = hex::decode(
        transaction["input"]
            .as_str()
            .expect("calldata")
            .trim_start_matches("0x"),
    )
    .expect("hex calldata");
    let call =
        ITributeFactory::offerTributeCall::abi_decode_validate(&data).expect("canonical offer ABI");
    assert_eq!(call.abi_encode(), data);
    assert_eq!(
        call.worldwideDay,
        world
            .state
            .wwd
            .as_ref()
            .expect("fixture day")
            .parse::<u32>()
            .unwrap()
    );
    assert!(!call.cipherText.is_empty());
    assert_eq!(call.nonce.len(), 12);
    assert!(!call.ephemeralPubkey.is_zero());
    match rejection {
        Rejection::Duplicate => assert!(call.excludeFromIntexIssuance),
        Rejection::MissingSignature => {
            assert_eq!(call.zkMerkleRoot.len(), 32, "must reach signature guard");
            assert!(call.signature.is_empty());
        }
        Rejection::InvalidProof => {
            assert_eq!(call.zkMerkleRoot.len(), 32);
            assert_eq!(call.signature.len(), 48);
            assert_eq!(
                call.zkProof.len(),
                outbe_zk_canonical::full_proof::COMBINED_LEN
            );
        }
    }

    let height: u64 = quantity(&receipt, "blockNumber")
        .try_into()
        .expect("receipt height");
    world
        .rpc
        .wait_finalized_checkpoint(&ports, height, 60)
        .expect("negative receipt finality");
    let checkpoint = world
        .rpc
        .checkpoint_at(ports[0], height)
        .expect("negative receipt checkpoint");
    let expected = Revert::from(rejection.reason().to_owned()).abi_encode();
    assert_eq!(
        receipt["blockHash"]
            .as_str()
            .unwrap()
            .parse::<B256>()
            .unwrap(),
        checkpoint.block_hash
    );
    for &port in &ports {
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, height)
                .expect("receipt header parity"),
            checkpoint
        );
        let other = eth::receipt_json(&world.rpc.url(port), tx_hash)
            .expect("negative receipt on every node");
        for field in [
            "transactionHash",
            "blockHash",
            "blockNumber",
            "status",
            "gasUsed",
            "effectiveGasPrice",
            "logs",
        ] {
            assert!(other.get(field).is_some());
            assert_eq!(
                other[field], receipt[field],
                "receipt {field} on port {port}"
            );
        }
        assert_eq!(other["status"], "0x0");
        assert_eq!(other["logs"], serde_json::json!([]));
        let at_receipt = eth::read_call_revert_data_at_block(
            &world.rpc.url(port),
            TRIBUTE_FACTORY_ADDRESS,
            caller,
            &call,
            U256::ZERO,
            BlockId::number(height),
            gas,
        )
        .expect("receipt-pinned guard reason; unavailable historical CE is not a passing negative");
        assert_eq!(
            at_receipt.as_ref(),
            expected,
            "wrong guard at receipt height on RPC {port}"
        );
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, height)
                .expect("post-replay receipt checkpoint"),
            checkpoint
        );
    }

    let mut observations = Vec::new();
    for &port in &ports {
        let started = Instant::now();
        let mut unstable = 0;
        loop {
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "no stable live guard observation on RPC {port}"
            );
            let before = latest_header(world, port);
            let actual = eth::read_call_revert_data_at_block(
                &world.rpc.url(port),
                TRIBUTE_FACTORY_ADDRESS,
                caller,
                &call,
                U256::ZERO,
                BlockId::Number(BlockNumberOrTag::Latest),
                gas,
            )
            .expect("live EVM guard observation; RPC/TEE/CE failures are not expected reverts");
            let after = latest_header(world, port);
            if before != after {
                unstable += 1;
                continue;
            }
            assert_eq!(
                actual.as_ref(),
                expected,
                "wrong Tribute guard on RPC {port}"
            );
            observations.push((port, before, unstable));
            break;
        }
    }
    let terminal = observations
        .iter()
        .map(|(_, h, _)| h.height)
        .max()
        .expect("guard observations");
    world
        .rpc
        .wait_finalized_checkpoint(&ports, terminal, 60)
        .expect("live guard headers become finalized");
    for (port, observed, unstable) in observations {
        for &verifier in &ports {
            let verified = world
                .rpc
                .checkpoint_at(verifier, observed.height)
                .expect("independent guard header");
            assert_eq!(verified.block_hash, observed.hash);
            assert_eq!(verified.state_root, observed.state_root);
        }
        println!(
            "TRIBUTE_REJECTION {}",
            serde_json::json!({
                "transaction_hash": tx_hash, "receipt_height": height, "port": port,
                "observation_height": observed.height, "block_hash": format!("{:#x}", observed.hash),
                "state_root": format!("{:#x}", observed.state_root), "unstable_attempts": unstable,
            "layer": "evm", "reason": rejection.reason(), "state_selection": "receipt_and_live_then_finalized",
            })
        );
    }
}

pub(super) fn assert_supply(world: &World, expected: u64) {
    let ports = world.validators.committee_ports();
    let head = world
        .rpc
        .head(world.validators.primary_port())
        .expect("supply head");
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, head, 60)
        .expect("supply finality");
    for port in ports {
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, checkpoint.height)
                .expect("before supply read"),
            checkpoint
        );
        assert_eq!(
            eth::read_call_at_result(
                &world.rpc.url(port),
                TRIBUTE_ADDR,
                &ITribute::totalSupplyCall {},
                checkpoint.height
            )
            .expect("finalized Tribute supply"),
            U256::from(expected)
        );
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, checkpoint.height)
                .expect("after supply read"),
            checkpoint
        );
    }
}
