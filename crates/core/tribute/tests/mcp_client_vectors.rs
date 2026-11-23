//! Check historical TypeScript MCP client vectors against the enclave.
//! The fixture preserves the JSON from commit 6c6cb705 before MCP moved to outbe-mcp.
//! NOD issuance outputs were regenerated with enclave encryption for Keccak256 NOD IDs.

use alloy_primitives::{Address, B256, U256};
use outbe_core_common::confidential_tee::{
    chacha20poly1305_decrypt, chacha20poly1305_encrypt, decrypt_gratis_balance,
    decrypt_nod_for_owner, decrypt_promis_balance, decrypt_share, derive_account_keys_message,
    eip191_hash, hkdf_sha256, BalanceTransition, EncryptedShare, Ledger,
};
use outbe_fidelity::fidelity_query_auth_message;
use outbe_gratis::cipher::{read_amount, write_amount};
use outbe_gratis::{modify_mac as gratis_modify_mac, GratisOp, ModifyOperation as GratisModify};
use outbe_nod::nod_encryption::encrypt_nod;
use outbe_primitives::tee_signatures::recover_signer;
use outbe_promis::cipher::{
    read_amount as read_promis_amount, write_amount as write_promis_amount,
};
use outbe_promis::enclave_client::{derive_promis_keys, DerivePromisKeysRequest};
use outbe_promis::{
    derive_account_keys, derive_promis_state_key, modify_mac as promis_modify_mac,
    ModifyOperation as PromisModify, PromisOp,
};
use outbe_tee::resident::{AccountKeysHandle, StateKeyHandle};
use serde_json::Value;
use x25519_dalek::{PublicKey, StaticSecret};

#[path = "support_share.rs"]
mod support_share;
use support_share::encrypt_share_deterministic;

const VECTORS: &str = r###"{
  "deriveKeys": {
    "signerKey": "0x0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c",
    "account": "0x63467B02a7382408A845a5EB85b5238b8a4dD0eD",
    "ledger": "Promis",
    "ephemeralSecret": "0x0202020202020202020202020202020202020202020202020202020202020202",
    "ephemeralPubkey": "0xce8d3ad1ccb633ec7b70c17814a5c76ecd029685050d344745ba05870e587d59",
    "message": "0x6f757462652f70726f6d69732f6465726976652d6b6579732f763163467b02a7382408a845a5eb85b5238b8a4dd0edce8d3ad1ccb633ec7b70c17814a5c76ecd029685050d344745ba05870e587d59",
    "signature": "0x0b20b5b12d996a78a3f0e5d3983e33a43754e534e4650843cfaf6beb164618ab00b87e14da6daa9fb5c70ebea37cdd2f758f5d85a23957915042f5d3dfaec5991b"
  },
  "sealedKeys": {
    "enclaveSecret": "0x0101010101010101010101010101010101010101010101010101010101010101",
    "enclaveEphemeralPubkey": "0xa4e09292b651c278b9772c569f5fa9bb13d906b46ab68c9df9dc2b4409f8a209",
    "nonce": "0xab02a2e3c93cbf650bbcb1e5",
    "sealed": "0x80a9b32b46568b2ea28136735fa781f6b793f6bbfcb79229b25819b263cc0a00bbc312b2adc6c8993e00454d31a0e22016eb63d08f4116e4160c86db327e0ba2e3cec9b2ebb2720d0e5f3fa104c71c4e",
    "view": "0x5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a",
    "modify": "0x6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b"
  },
  "macs": [
    {
      "ledger": "Gratis",
      "account": "0x1111111111111111111111111111111111111111",
      "op": 0,
      "amount": "1000",
      "opNonce": "0",
      "chainId": "0x0000000000000000000000000000000000000000000000000000000000000001",
      "modifyKey": "0x5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a",
      "mac": "0xec11651be45952198f63a0c982bbe045c3f3a31131c8afe655f76b243dc50aa1"
    },
    {
      "ledger": "Gratis",
      "account": "0x1111111111111111111111111111111111111111",
      "op": 2,
      "amount": "10000000",
      "opNonce": "3",
      "chainId": "0x0000000000000000000000000000000000000000000000000000000000000001",
      "modifyKey": "0x5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a",
      "mac": "0xf7d19e989e00dc99df7168cbca111ac11c7328fa0e12c4b0e7873b3ce4e84046"
    },
    {
      "ledger": "Promis",
      "account": "0x1111111111111111111111111111111111111111",
      "op": 0,
      "amount": "1000",
      "opNonce": "0",
      "chainId": "0x0000000000000000000000000000000000000000000000000000000000000001",
      "modifyKey": "0x5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a",
      "mac": "0x3d33ac4e239f595c5d87e9db9bd4e180ff4f080811f22150cfc0b7b7eba2467b"
    },
    {
      "ledger": "Promis",
      "account": "0xabcdef0123456789abcdef0123456789abcdef01",
      "op": 1,
      "amount": "123456789012345678901234567890",
      "opNonce": "7",
      "chainId": "0xc2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2",
      "modifyKey": "0x5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a",
      "mac": "0x94bbfa7708394cf45466594357d2dc942f65e92ef0aa2509e03dfec5a5861f84"
    },
    {
      "ledger": "Gratis",
      "account": "0x63467B02a7382408A845a5EB85b5238b8a4dD0eD",
      "op": 1,
      "amount": "2500000",
      "opNonce": "4",
      "chainId": "0x00000000000000000000000000000000000000000000000000000000033ce4a9",
      "modifyKey": "0x6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b",
      "mac": "0x3eb5525660b7f79650a2a6ab2b25b67644e72e5d56dd6ef10454badb322f3d6b"
    },
    {
      "ledger": "Promis",
      "account": "0x63467B02a7382408A845a5EB85b5238b8a4dD0eD",
      "op": 0,
      "amount": "7000000",
      "opNonce": "9",
      "chainId": "0x00000000000000000000000000000000000000000000000000000000033ce4a9",
      "modifyKey": "0x6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b",
      "mac": "0x7b3938033a1bb7b1ef93540f3fb86d01122bc8772b2ef4a8d1f6d3377ee1e02d"
    }
  ],
  "balances": {
    "viewKey": "0x5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a",
    "account": "0x1111111111111111111111111111111111111111",
    "promis": {
      "field": 0,
      "previousVersion": "4",
      "inputHash": "0x0606060606060606060606060606060606060606060606060606060606060606",
      "amount": "123456789",
      "blob": "0x000000000000000550524f328ab66079822a11c7a53c086d827f104690251044a05eb599c2abd3c3d3ad40d11677d683be6a5cfec2681a4184c710ded588be72b9e95f7fae6935d40221b7fcbaac0060ab0604ebf88c867bcb9ba40d"
    },
    "gratis": [
      {
        "field": 0,
        "previousVersion": "2",
        "inputHash": "0x0404040404040404040404040404040404040404040404040404040404040404",
        "amount": "2500000",
        "blob": "0x000000000000000347524132bdd4691da65390638406af421d6e89f2ce3a4d71e5b5f2f3fc767ca2ab18e06a690188274592e8e970304a3ee88ff9283e3cef1444b0b2816f4a2629d84fc51bdccef2c8c0b2af2901404c556259922a"
      },
      {
        "field": 1,
        "previousVersion": "6",
        "inputHash": "0x0505050505050505050505050505050505050505050505050505050505050505",
        "amount": "900000",
        "blob": "0x000000000000000747524132278f700859eac2c84eda4d4b77fe2a09881f54030247d3c8e4b4e4cd7752664ec69d725721dba1c36af0eed950af5a1501cb8cf53aaaea3715f73b03a829b4b921f8aa7ab7c368f9bef2adfb82787207"
      }
    ]
  },
  "nod": {
    "nodStateKey": "0x2121212121212121212121212121212121212121212121212121212121212121",
    "viewKey": "0xf30b00cecebc908a4a057065865a758dfcf4c223a0efcdadd313d56abd044c01",
    "chainId": "54322345",
    "nodId": "95917739879757436541213720947316054566636781989754332168412108264760970689905",
    "owner": "0x0303030303030303030303030303030303030303",
    "emissionDay": 20261010,
    "leagueId": 2,
    "entryPriceMinor": "2000000",
    "issuanceCurrency": 840,
    "referenceCurrency": 978,
    "encryptionBinding": "0x9734139402ec86cd949d7ec3e598a0e4bec2fe3b167af52864452ef2353011cc",
    "encryptedGratisAmount": "0x0000000000000001c5d6afcc3d45d380a334832f7ed18a071210668dc4e26b4b0fbd7b4d6dbf0a13a5376fdb67c15f9dd757f5d67d9a94cb",
    "gratisAmount": "4500000"
  },
  "fidelityQuery": {
    "chainId": "54322345",
    "account": "0x63467B02a7382408A845a5EB85b5238b8a4dD0eD",
    "expiry": "1760000000",
    "message": "0x6f757462652f666964656c6974792f71756572792d617574682f763100000000000000000000000000000000000000000000000000000000033ce4a963467b02a7382408a845a5eb85b5238b8a4dd0ed0000000068e77800",
    "signature": "0x5a9325cf2242871f2b92d6f2df94625f20d2374aace1a823f85bababe0fdb85c7ea4fa728b3b87692ce7de12a031e586c73da1a94c49ce7a91112249b00561351c"
  }
}
"###;

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
            "Gratis" => gratis_modify_mac(
                &AccountKeysHandle::for_test(account, [0; 32], key),
                &GratisModify {
                    account,
                    op: [GratisOp::Mint, GratisOp::Burn, GratisOp::Pledge][op as usize],
                    amount,
                    op_nonce,
                    chain_id,
                },
            ),
            "Promis" => promis_modify_mac(
                &AccountKeysHandle::for_test(account, [0; 32], key),
                &PromisModify {
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
    let all = vectors();
    let v = &all["balances"];
    let view = word(&v["viewKey"]);
    let view_handle = StateKeyHandle::for_test(view);
    let account: Address = v["account"].as_str().unwrap().parse().unwrap();
    let decimal = |value: &Value| U256::from_str_radix(value.as_str().unwrap(), 10).unwrap();
    let version = |value: &Value| value.as_str().unwrap().parse::<u64>().unwrap();

    let promis = &v["promis"];
    let promis_field = promis["field"].as_u64().unwrap() as u8;
    let blob = write_promis_amount(
        &view_handle,
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
        read_promis_amount(&view_handle, account, promis_field, &blob)
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
            &view_handle,
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
            read_amount(&view_handle, account, field, &blob).unwrap().1,
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
    use outbe_core_common::nod_encryption::NodTermsV1;

    let all = vectors();
    let v = &all["nod"];
    let decimal = |value: &Value| U256::from_str_radix(value.as_str().unwrap(), 10).unwrap();
    let nod_state_key = word(&v["nodStateKey"]);
    let terms = NodTermsV1 {
        chain_id: v["chainId"].as_str().unwrap().parse().unwrap(),
        nod_id: outbe_core_common::nod_rules::derive_nod_id(
            outbe_core_common::series_id::SeriesId::for_pair(
                v["emissionDay"].as_u64().unwrap() as u32,
                v["issuanceCurrency"].as_u64().unwrap() as u16,
                v["referenceCurrency"].as_u64().unwrap() as u16,
            )
            .unwrap(),
            v["owner"].as_str().unwrap().parse().unwrap(),
        ),
        owner: v["owner"].as_str().unwrap().parse().unwrap(),
        emission_day: v["emissionDay"].as_u64().unwrap() as u32,
        entry_price_minor: decimal(&v["entryPriceMinor"]),
        issuance_currency: v["issuanceCurrency"].as_u64().unwrap() as u16,
        reference_currency: v["referenceCurrency"].as_u64().unwrap() as u16,
    };
    let view_key = hkdf_sha256(
        &nod_state_key,
        terms.owner.as_slice(),
        b"outbe/nod/view-key/v1",
    )
    .unwrap();
    assert_eq!(terms.nod_id, decimal(&v["nodId"]));
    assert_eq!(view_key, word(&v["viewKey"]));
    let nod = encrypt_nod(
        &StateKeyHandle::for_test(nod_state_key),
        terms,
        decimal(&v["gratisAmount"]),
        decimal(&v["gratisAmount"]),
    )
    .unwrap();
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

#[path = "encrypted_tribute/resident.rs"]
mod resident;

#[test]
fn historical_mcp_signature_authorizes_resident_account_key_export() {
    use std::sync::Arc;

    use outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test;

    let context = Arc::new(resident::context(54322345));
    let _guard = set_in_process_tee_context_for_test(Some(Arc::clone(&context)));
    let all = vectors();
    let v = &all["deriveKeys"];
    let account = v["account"].as_str().unwrap().parse().unwrap();
    let requester = word(&v["ephemeralPubkey"]);
    let owner_sig = bytes(&v["signature"]);
    let sealed = derive_promis_keys(DerivePromisKeysRequest {
        account,
        requester_ephemeral_pubkey: requester,
        owner_sig: owner_sig.clone(),
    })
    .unwrap();
    assert_eq!(sealed.account, account);
    let secret = StaticSecret::from(word(&v["ephemeralSecret"]));
    let shared = secret.diffie_hellman(&PublicKey::from(sealed.enclave_ephemeral_pubkey));
    let key = hkdf_sha256(&requester, shared.as_bytes(), b"outbe/tee/dkg-share/v1").unwrap();
    let opened = chacha20poly1305_decrypt(&key, &sealed.nonce, &sealed.sealed).unwrap();
    assert_eq!(opened.len(), 64);
    let state = resident::state(&context, derive_promis_state_key);
    let keys = derive_account_keys(&state, account).unwrap();
    let raw_view: [u8; 32] = opened[..32].try_into().unwrap();
    let nonce = [1; 12];
    assert_eq!(
        keys.view_key_handle()
            .aead_encrypt(&nonce, b"client view")
            .unwrap(),
        chacha20poly1305_encrypt(&raw_view, &nonce, b"client view").unwrap()
    );
    let mut invalid = owner_sig;
    invalid[0] ^= 1;
    assert!(derive_promis_keys(DerivePromisKeysRequest {
        account,
        requester_ephemeral_pubkey: requester,
        owner_sig: invalid,
    })
    .is_err());
}
