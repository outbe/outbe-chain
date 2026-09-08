//! Contract rejection evidence for guards whose inputs remain stable across a
//! failed transaction. Callers supply the exact expected ABI error independently.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{Revert, SolCall, SolError};

use crate::{internal::eth, world::World};

pub(super) fn assert_mined_revert_reason<C: SolCall>(
    world: &World,
    to: Address,
    key: &str,
    call: &C,
    reason: &str,
) -> crate::world::rpc::TxOutcome {
    assert_mined_revert(
        world,
        to,
        key,
        call,
        U256::ZERO,
        &Revert {
            reason: reason.to_owned(),
        }
        .abi_encode(),
    )
    .into()
}

pub(super) fn assert_registration_revert(
    world: &World,
    key: &str,
    identity: &crate::world::validators::RegistrationIdentity,
    signature: &[u8],
    reason: &str,
) -> crate::world::rpc::TxOutcome {
    assert_mined_revert_reason(
        world,
        crate::internal::addresses::VS_ADDR,
        key,
        &eth::IValidatorSet::registerValidatorCall {
            validatorAddress: identity.address(),
            consensusPubkey: Bytes::copy_from_slice(identity.bls_public_key()),
            radicleNodeId: identity.radicle_node_id(),
            blsRegistrationSignature: Bytes::copy_from_slice(signature),
        },
        reason,
    )
}

pub(super) fn assert_mined_revert<C: SolCall>(
    world: &World,
    to: Address,
    key: &str,
    call: &C,
    value: U256,
    expected: &[u8],
) -> eth::MinedCallOutcome {
    let ports = world.validators.committee_ports();
    assert!(
        !ports.is_empty(),
        "negative requires validator observations"
    );
    let from = eth::address_of(key).expect("negative transaction signer");
    let primary = world.validators.primary_port();
    let head = world.rpc.head(primary).expect("negative precondition head");
    let before = world
        .rpc
        .wait_finalized_checkpoint(&ports, head, 40)
        .expect("negative preconditions finalized on every validator");

    let assert_reason_at = |height| {
        let checkpoint = world
            .rpc
            .checkpoint_at(primary, height)
            .expect("revert checkpoint");
        for &port in &ports {
            assert_eq!(
                world
                    .rpc
                    .checkpoint_at(port, height)
                    .expect("pre-call checkpoint"),
                checkpoint
            );
            let actual =
                eth::read_call_revert_data_at(&world.rpc.url(port), to, from, call, value, height)
                    .expect("exact EVM rejection with valid caller, value and fixed gas");
            assert_eq!(
                actual.as_ref(),
                expected,
                "unexpected contract guard on RPC {port} at h{height}"
            );
            assert_eq!(
                world
                    .rpc
                    .checkpoint_at(port, height)
                    .expect("post-call checkpoint"),
                checkpoint
            );
        }
        checkpoint
    };
    assert_reason_at(before.height);

    let outcome = eth::send_call_outcome(&world.rpc.url(primary), to, key, call, Some(value))
        .expect("fixed-gas negative transaction must be admitted and mined");
    assert!(
        !outcome.success,
        "negative transaction succeeded: {}",
        outcome.transaction_hash
    );
    let transaction = eth::raw_json_result(
        &world.rpc.url(primary),
        "eth_getTransactionByHash",
        serde_json::json!([outcome.transaction_hash]),
    )
    .expect("mined negative transaction body");
    let quantity = |object: &serde_json::Value, field: &str| {
        U256::from_str_radix(
            object[field]
                .as_str()
                .expect("transaction/receipt quantity")
                .trim_start_matches("0x"),
            16,
        )
        .expect("valid transaction/receipt quantity")
    };
    assert!(
        quantity(&outcome.receipt, "gasUsed") < quantity(&transaction, "gas"),
        "negative consumed its entire gas limit; receipt cannot establish the intended guard"
    );
    assert_eq!(
        transaction["from"]
            .as_str()
            .expect("transaction caller")
            .parse::<Address>()
            .expect("caller address"),
        from
    );
    assert_eq!(
        transaction["to"]
            .as_str()
            .expect("transaction target")
            .parse::<Address>()
            .expect("target address"),
        to
    );
    assert_eq!(
        transaction["input"],
        format!("0x{}", hex::encode(call.abi_encode()))
    );
    assert_eq!(quantity(&transaction, "value"), value);
    let block = outcome.receipt["blockNumber"]
        .as_str()
        .expect("mined block number");
    let height = u64::from_str_radix(block.trim_start_matches("0x"), 16).expect("receipt height");
    world
        .rpc
        .wait_finalized_checkpoint(&ports, height, 40)
        .expect("negative receipt finalized on every validator");
    let checkpoint = assert_reason_at(height);
    assert_eq!(
        outcome.receipt["blockHash"]
            .as_str()
            .expect("receipt block hash"),
        format!("{:#x}", checkpoint.block_hash)
    );
    for &port in &ports {
        let receipt = eth::receipt_json(&world.rpc.url(port), &outcome.transaction_hash)
            .expect("negative receipt on every validator");
        for field in [
            "transactionHash",
            "blockHash",
            "blockNumber",
            "status",
            "gasUsed",
            "effectiveGasPrice",
            "logs",
        ] {
            assert!(receipt.get(field).is_some(), "receipt missing {field}");
            assert_eq!(
                receipt[field], outcome.receipt[field],
                "receipt {field} differs on RPC {port}"
            );
        }
        assert_eq!(receipt["status"], "0x0");
        assert_eq!(receipt["logs"], serde_json::json!([]));
        println!(
            "MINED_REJECTION {}",
            serde_json::json!({
                "port": port, "transaction_hash": outcome.transaction_hash,
                "height": height, "block_hash": format!("{:#x}", checkpoint.block_hash),
                "expected_revert": format!("0x{}", hex::encode(expected)),
                "layer": "evm", "precondition_height": before.height,
            })
        );
    }
    outcome
}
