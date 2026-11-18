use alloy_consensus::{transaction::RlpEcdsaEncodableTx as _, SignableTransaction as _, TxLegacy};
use alloy_primitives::{address, Address, Bytes, Signature, B256, U256};
use alloy_sol_types::{sol, SolCall as _};
use commonware_codec::{DecodeExt as _, Encode as _};
use commonware_cryptography::{
    bls12381::{
        primitives::{group::Private, ops, variant::MinPk},
        PrivateKey,
    },
    Signer as _,
};
use outbe_primitives::{
    addresses::VALIDATOR_SET_ADDRESS,
    consensus_p2p::{encode_v1, P2pAddress, P2P_ADDRESS_VERSION_V1},
    signer::OutbeEvmSigner,
    validators::{validator_registration_message, VALIDATOR_REGISTRATION_DST},
};
use outbe_validatorset::precompile::IValidatorSet;
use serde_json::{json, Value};
use std::{net::SocketAddr, time::Duration};

use crate::rpc::Rpc;

sol!("../../contracts/precompiles/src/IStaking.sol");

const STAKING: Address = address!("000000000000000000000000000000000000EE02");

pub async fn stake(
    rpc: &Rpc,
    evm: &OutbeEvmSigner,
    bls: &PrivateKey,
    node_id: B256,
    listen: SocketAddr,
) {
    let amount = rpc.call(STAKING, IStaking::getMinStakeCall {}).await;
    let chain = quantity(rpc, "eth_chainId", json!([])).await.to();
    let private = Private::decode(bls.encode()).unwrap();
    let proof = ops::sign::<MinPk>(
        &private,
        VALIDATOR_REGISTRATION_DST,
        &validator_registration_message(chain, evm.address(), node_id),
    );
    let call = IStaking::stakeCall {
        validatorAddress: evm.address(),
        amount,
        consensusPubkey: bls.public_key().encode().to_vec().into(),
        radicleNodeId: node_id,
        blsRegistrationSignature: proof.encode().to_vec().into(),
        p2pVersion: P2P_ADDRESS_VERSION_V1,
        p2pEncoded: encode_v1(&P2pAddress::Symmetric(listen)).into(),
    };
    send(rpc, evm, STAKING, call.abi_encode().into(), amount).await;
}

pub async fn confirm_ready(rpc: &Rpc, evm: &OutbeEvmSigner) {
    let call = IValidatorSet::confirmValidatorReadyCall {}.abi_encode();
    send(rpc, evm, VALIDATOR_SET_ADDRESS, call.into(), U256::ZERO).await;
}

pub async fn transfer(rpc: &Rpc, evm: &OutbeEvmSigner, to: Address, amount: U256) {
    send(rpc, evm, to, Bytes::new(), amount).await;
}

pub async fn add_slash_margin(rpc: &Rpc, evm: &OutbeEvmSigner) {
    let minimum = rpc.call(STAKING, IStaking::getMinStakeCall {}).await;
    let details = rpc
        .request("outbe_getValidator", json!([evm.address()]))
        .await
        .expect("bonded validator");
    let target = minimum + minimum / U256::from(4);
    let amount = target.saturating_sub(as_quantity(&details["stake"]));
    if amount.is_zero() {
        return;
    }
    let call = IStaking::topupCall {
        validatorAddress: evm.address(),
        amount,
    };
    send(rpc, evm, STAKING, call.abi_encode().into(), amount).await;
}

pub async fn unjail(rpc: &Rpc, evm: &OutbeEvmSigner) {
    let call = IStaking::unjailValidatorCall {}.abi_encode();
    send(rpc, evm, STAKING, call.into(), U256::ZERO).await;
}

pub async fn drain_balance(rpc: &Rpc, evm: &OutbeEvmSigner, sink: Address) {
    let balance = quantity(rpc, "eth_getBalance", json!([evm.address(), "latest"])).await;
    let gas_price = quantity(rpc, "eth_gasPrice", json!([])).await.to::<u128>();
    let gas_limit = 21_000;
    let tx = TxLegacy {
        chain_id: Some(quantity(rpc, "eth_chainId", json!([])).await.to()),
        nonce: quantity(
            rpc,
            "eth_getTransactionCount",
            json!([evm.address(), "pending"]),
        )
        .await
        .to(),
        gas_price,
        gas_limit,
        to: sink.into(),
        value: balance - U256::from(gas_price * u128::from(gas_limit)),
        input: Bytes::new(),
    };
    submit(rpc, evm, tx).await;
    assert!(rpc.account("eth_getBalance", evm.address()).await.is_zero());
}

async fn send(rpc: &Rpc, from: &OutbeEvmSigner, to: Address, input: Bytes, value: U256) {
    let estimate = quantity(
        rpc,
        "eth_estimateGas",
        json!([{"from": from.address(), "to": to, "data": input, "value": value}]),
    )
    .await
    .to::<u64>();
    let tx = TxLegacy {
        chain_id: Some(quantity(rpc, "eth_chainId", json!([])).await.to()),
        nonce: quantity(
            rpc,
            "eth_getTransactionCount",
            json!([from.address(), "pending"]),
        )
        .await
        .to(),
        gas_price: quantity(rpc, "eth_gasPrice", json!([])).await.to(),
        gas_limit: estimate + estimate / 5,
        to: to.into(),
        value,
        input,
    };
    submit(rpc, from, tx).await;
}

async fn submit(rpc: &Rpc, from: &OutbeEvmSigner, tx: TxLegacy) {
    let signature = from.sign_hash(&tx.signature_hash()).unwrap();
    let signature = Signature::from_bytes_and_parity(&signature[..64], signature[64] != 0);
    let mut raw = Vec::new();
    tx.rlp_encode_signed(&signature, &mut raw);
    let hash = rpc
        .request("eth_sendRawTransaction", json!([Bytes::from(raw)]))
        .await
        .expect("operator transaction rejected by the upstream");
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            if let Some(receipt) = rpc
                .request("eth_getTransactionReceipt", json!([hash]))
                .await
                .filter(|value| !value.is_null())
            {
                let height = as_quantity(&receipt["blockNumber"]).to::<u64>();
                if rpc
                    .finalized()
                    .await
                    .is_some_and(|finalized| finalized >= height)
                {
                    let block = rpc
                        .request(
                            "eth_getBlockByNumber",
                            json!([receipt["blockNumber"], false]),
                        )
                        .await
                        .expect("finalized receipt block absent");
                    assert_eq!(receipt["blockHash"], block["hash"]);
                    assert_eq!(receipt["status"], "0x1", "operator transaction {hash}");
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("operator transaction did not finalize");
}

async fn quantity(rpc: &Rpc, method: &str, params: Value) -> U256 {
    let envelope = rpc.raw(method, params).await.unwrap();
    let value = envelope
        .get("result")
        .unwrap_or_else(|| panic!("{method} failed: {}", envelope["error"]));
    as_quantity(value)
}

fn as_quantity(value: &Value) -> U256 {
    U256::from_str_radix(value.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}
