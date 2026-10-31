//! Check historical TypeScript MCP client vectors against the enclave.
//! The fixture preserves the JSON from commit 6c6cb705 before MCP moved to outbe-mcp.
//! NOD issuance outputs were regenerated with enclave encryption for Keccak256 NOD IDs.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::tee_signatures::recover_signer;
use outbe_tee::protocol::{
    derive_account_keys_message, eip191_hash, fidelity_query_auth_message, GratisOp, Ledger,
    PromisOp,
};
use outbe_tee_enclave::crypto::{decrypt_share, encrypt_share_deterministic, EncryptedShare};
use outbe_tee_enclave::{gratis, promis};
use serde_json::Value;
use x25519_dalek::{PublicKey, StaticSecret};

const VECTORS: &str = include_str!("fixtures/mcp_client_vectors.json");

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
fn mcp_signs_the_fidelity_query_the_enclave_accepts() {
    let v = &vectors()["fidelityQuery"];
    let account: Address = v["account"].as_str().unwrap().parse().unwrap();
    let chain_id = U256::from(v["chainId"].as_str().unwrap().parse::<u64>().unwrap());
    let message = fidelity_query_auth_message(
        B256::from(chain_id),
        account,
        v["expiry"].as_str().unwrap().parse().unwrap(),
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
    let sealed =
        encrypt_share_deterministic(&word(&v["enclaveSecret"]), &client_pub, &plaintext).unwrap();
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
    use outbe_tee::gratis_decrypt::decrypt_gratis_balance;
    use outbe_tee::promis_decrypt::decrypt_promis_balance;
    use outbe_tee_enclave::gratis_cipher::{
        read_amount, read_promis_amount, write_amount, write_promis_amount, BalanceTransition,
    };

    let all = vectors();
    let v = &all["balances"];
    let view = word(&v["viewKey"]);
    let account: Address = v["account"].as_str().unwrap().parse().unwrap();
    let decimal = |value: &Value| U256::from_str_radix(value.as_str().unwrap(), 10).unwrap();
    let version = |value: &Value| value.as_str().unwrap().parse::<u64>().unwrap();

    let promis = &v["promis"];
    let promis_field = promis["field"].as_u64().unwrap() as u8;
    let blob = write_promis_amount(
        &view,
        BalanceTransition {
            account,
            field: promis_field,
            previous_version: version(&promis["previousVersion"]),
            amount: decimal(&promis["amount"]),
            input_hash: B256::from(word(&promis["inputHash"])),
        },
    )
    .unwrap();
    assert_eq!(blob, bytes(&promis["blob"]));
    assert_eq!(
        read_promis_amount(&view, account, promis_field, &blob)
            .unwrap()
            .1,
        decimal(&promis["amount"])
    );
    assert_eq!(
        decrypt_promis_balance(&view, account, &blob).unwrap(),
        decimal(&promis["amount"])
    );

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
        if field == 0 {
            assert_eq!(
                decrypt_gratis_balance(&view, account, &blob).unwrap(),
                decimal(&gratis["amount"])
            );
        }
    }
}

#[test]
fn mcp_opens_the_nod_amount_the_enclave_encrypts() {
    use outbe_primitives::nod_encryption::NodTermsV1;
    use outbe_tee::nod_decrypt::decrypt_nod_for_owner;
    use outbe_tee_enclave::nod_encryption::{derive_view_key, encrypt_nod};

    let all = vectors();
    let v = &all["nod"];
    let decimal = |value: &Value| U256::from_str_radix(value.as_str().unwrap(), 10).unwrap();
    let nod_state_key = word(&v["nodStateKey"]);
    let terms = NodTermsV1 {
        chain_id: v["chainId"].as_str().unwrap().parse().unwrap(),
        nod_id: outbe_primitives::nod_rules::derive_nod_id(
            outbe_primitives::series_id::SeriesId::for_pair(
                v["emissionDay"].as_u64().unwrap() as u32,
                v["issuanceCurrency"].as_u64().unwrap() as u16,
                v["referenceCurrency"].as_u64().unwrap() as u16,
            )
            .unwrap(),
            v["owner"].as_str().unwrap().parse().unwrap(),
        ),
        owner: v["owner"].as_str().unwrap().parse().unwrap(),
        emission_day: v["emissionDay"].as_u64().unwrap() as u32,
        league_id: v["leagueId"].as_u64().unwrap() as u16,
        entry_price_minor: decimal(&v["entryPriceMinor"]),
        issuance_currency: v["issuanceCurrency"].as_u64().unwrap() as u16,
        reference_currency: v["referenceCurrency"].as_u64().unwrap() as u16,
    };
    let view_key = derive_view_key(&nod_state_key, terms.owner).unwrap();
    assert_eq!(terms.nod_id, decimal(&v["nodId"]));
    assert_eq!(*view_key, word(&v["viewKey"]));
    let nod = encrypt_nod(&nod_state_key, terms, decimal(&v["gratisAmount"])).unwrap();
    assert_eq!(
        nod.encryption_binding,
        B256::from(word(&v["encryptionBinding"]))
    );
    assert_eq!(
        nod.encrypted_gratis_amount,
        bytes(&v["encryptedGratisAmount"])
    );
    assert_eq!(
        decrypt_nod_for_owner(&view_key, &nod).unwrap(),
        decimal(&v["gratisAmount"])
    );
}
