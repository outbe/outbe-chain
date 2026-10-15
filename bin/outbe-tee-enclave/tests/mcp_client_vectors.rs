//! The MCP server builds deriveKeys requests, opens sealed keys and computes modify MACs in
//! TypeScript. These vectors are the same JSON its tests read, checked here against the enclave.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::tee_signatures::recover_signer;
use outbe_tee::protocol::{
    derive_account_keys_message, eip191_hash, GratisOp, Ledger, PromisOp,
};
use outbe_tee_enclave::crypto::{decrypt_share, encrypt_share_deterministic, EncryptedShare};
use outbe_tee_enclave::{gratis, promis};
use serde_json::Value;
use x25519_dalek::{PublicKey, StaticSecret};

const VECTORS: &str = include_str!("../../../mcp/src/confidential/vectors.json");

fn vectors() -> Value {
    serde_json::from_str(VECTORS).unwrap()
}

fn bytes(value: &Value) -> Vec<u8> {
    hex::decode(value.as_str().unwrap().trim_start_matches("0x")).unwrap()
}

fn word(value: &Value) -> [u8; 32] {
    bytes(value).try_into().unwrap()
}

#[test]
fn mcp_signs_the_derive_keys_request_the_enclave_accepts() {
    let v = &vectors()["deriveKeys"];
    let account: Address = v["account"].as_str().unwrap().parse().unwrap();
    let message = derive_account_keys_message(
        Ledger::Promis,
        account,
        B256::from(word(&v["ephemeralPubkey"])),
    );
    assert_eq!(message, bytes(&v["message"]));
    let signature: [u8; 65] = bytes(&v["signature"]).try_into().unwrap();
    assert_eq!(
        recover_signer(&eip191_hash(&message), &signature).unwrap(),
        account
    );
}

#[test]
fn mcp_opens_the_keys_the_enclave_seals() {
    let all = vectors();
    let v = &all["sealedKeys"];
    let client_secret = word(&all["deriveKeys"]["ephemeralSecret"]);
    let client_pub = PublicKey::from(&StaticSecret::from(client_secret)).to_bytes();
    let plaintext = [bytes(&v["view"]), bytes(&v["modify"])].concat();
    let sealed = encrypt_share_deterministic(&word(&v["enclaveSecret"]), &client_pub, &plaintext)
        .unwrap();
    assert_eq!(sealed.ephemeral_pub, word(&v["enclaveEphemeralPubkey"]));
    assert_eq!(sealed.nonce.to_vec(), bytes(&v["nonce"]));
    assert_eq!(sealed.ciphertext, bytes(&v["sealed"]));
    let opened = decrypt_share(
        &client_secret,
        &EncryptedShare {
            ephemeral_pub: sealed.ephemeral_pub,
            nonce: sealed.nonce,
            ciphertext: sealed.ciphertext,
        },
    )
    .unwrap();
    assert_eq!(opened, plaintext);
}

#[test]
fn mcp_modify_macs_match_the_enclave() {
    for v in vectors()["macs"].as_array().unwrap() {
        let key = word(&v["modifyKey"]);
        let account: Address = v["account"].as_str().unwrap().parse().unwrap();
        let amount = U256::from_str_radix(v["amount"].as_str().unwrap(), 10).unwrap();
        let op_nonce = v["opNonce"].as_str().unwrap().parse().unwrap();
        let chain_id = B256::from(word(&v["chainId"]));
        let op = v["op"].as_u64().unwrap();
        let mac = match v["ledger"].as_str().unwrap() {
            "Gratis" => gratis::modify_mac(
                &key,
                &gratis::ModifyOperation {
                    account,
                    op: [GratisOp::Mint, GratisOp::Burn, GratisOp::Pledge][op as usize],
                    amount,
                    op_nonce,
                    chain_id,
                },
            ),
            "Promis" => promis::modify_mac(
                &key,
                &promis::ModifyOperation {
                    account,
                    op: [PromisOp::Mint, PromisOp::Burn][op as usize],
                    amount,
                    op_nonce,
                    chain_id,
                },
            ),
            ledger => panic!("unknown ledger {ledger}"),
        };
        assert_eq!(mac.to_vec(), bytes(&v["mac"]), "{v}");
    }
}

#[test]
fn mcp_opens_the_balances_the_enclave_writes() {
    use outbe_tee_enclave::confidential::{FIELD_BALANCE, PROMIS};
    use outbe_tee_enclave::gratis_cipher::{read_amount, write_amount, BalanceTransition};

    let all = vectors();
    let v = &all["balances"];
    let view = word(&v["viewKey"]);
    let account: Address = v["account"].as_str().unwrap().parse().unwrap();
    let decimal = |value: &Value| U256::from_str_radix(value.as_str().unwrap(), 10).unwrap();
    let version = |value: &Value| value.as_str().unwrap().parse::<u64>().unwrap();

    let promis = &v["promis"];
    let slot = PROMIS.cipher.slot(&view, account, FIELD_BALANCE);
    let blob = slot
        .write_amount(version(&promis["previousVersion"]), decimal(&promis["amount"]))
        .unwrap();
    assert_eq!(blob, bytes(&promis["blob"]));
    assert_eq!(slot.read_amount(&blob).unwrap().1, decimal(&promis["amount"]));

    for gratis in v["gratis"].as_array().unwrap() {
        let field = gratis["field"].as_u64().unwrap() as u8;
        let blob = write_amount(
            &view,
            BalanceTransition {
                account,
                field,
                previous_version: version(&gratis["previousVersion"]),
                amount: decimal(&gratis["amount"]),
                input_hash: B256::from(word(&gratis["inputHash"])),
            },
        )
        .unwrap();
        assert_eq!(blob, bytes(&gratis["blob"]));
        assert_eq!(
            read_amount(&view, account, field, &blob).unwrap().1,
            decimal(&gratis["amount"])
        );
    }
}
