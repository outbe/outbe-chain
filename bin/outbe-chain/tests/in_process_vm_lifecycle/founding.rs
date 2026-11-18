use crate::{rpc::Rpc, tee_cluster::Cluster};
use alloy_primitives::{eip191_hash_message, Address, Bytes, B256};
use outbe_primitives::{
    addresses::TEE_REGISTRY_ADDRESS,
    reshare_artifact::decode_boundary_artifact,
    signer::OutbeEvmSigner,
    system_tx::{
        binding::validate_tee_bootstrap_payload, SystemTx, CONSENSUS_BOUNDARY_OUTCOME_STEP_INDEX,
        CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX, OUTBE_SYSTEM_TX_ADDRESS,
    },
    tee_attestation_v1::AttestationMode,
    tee_registry_abi_v1::ITeeRegistryV1,
};
use outbe_tee::finalized_admission::TEE_REGISTRY_OFFER_PUBLIC_SLOT_V1;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs,
    io::Read as _,
    path::Path,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SealedKeys {
    sealed: Bytes,
    nonce: Bytes,
    enclave_ephemeral_pubkey: B256,
}

#[tokio::test]
async fn four_founders_finalize_both_ceremonies_and_serve_owner_only_derive_keys() {
    let mut cluster = Cluster::start_with_genesis(
        json!({"chain_id":424242,"epoch_length_blocks":1200}),
        |_, _| {},
    );
    let pids: Vec<_> = cluster
        .processes
        .iter()
        .map(|process| process.id())
        .collect();
    assert_eq!(pids.iter().collect::<BTreeSet<_>>().len(), 4);
    cluster.wait_finalized_on(1, pids.len()).await;
    let (hash, offer_public) = assert_founding_block(&cluster).await;
    println!("Founding finalized block 1 {hash}: VRF and TEE bootstrap; registry offer key {offer_public}");
    wait_for_resident_canaries(&mut cluster).await;
    println!("All four resident canaries encrypted and decrypted their known-plaintext offers");
    assert_owner_only_exports(&cluster).await;
    cluster.assert_running(pids.len());
    assert_eq!(
        cluster
            .processes
            .iter()
            .map(|process| process.id())
            .collect::<Vec<_>>(),
        pids,
        "founding acceptance must use the four original processes"
    );
    println!("All four founders exported the sealed-key envelope only to the owner request");
    for index in 0..pids.len() {
        assert_no_sealed_files(&cluster.root.join(format!("validator-{index}/data")));
    }
}

pub fn assert_no_sealed_files(path: &Path) {
    for entry in fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            assert_no_sealed_files(&path);
        } else if kind.is_file() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            assert!(
                !["tseal", "tsgx1", "seed", "offer-key", "offer_key"]
                    .iter()
                    .any(|marker| name.contains(marker)),
                "unexpected TEE secret file: {}",
                path.display()
            );
            let mut prefix = Vec::with_capacity(5);
            fs::File::open(&path)
                .unwrap()
                .take(5)
                .read_to_end(&mut prefix)
                .unwrap();
            assert!(
                prefix != b"TSEAL" && prefix != b"TSGX1",
                "legacy sealing format persisted to {}",
                path.display()
            );
        }
    }
}

async fn assert_founding_block(cluster: &Cluster) -> (B256, B256) {
    let producer = &cluster.rpcs[0];
    let block = producer
        .request("eth_getBlockByNumber", json!(["0x1", true]))
        .await
        .expect("finalized block 1 body");
    let hash: B256 = serde_json::from_value(block["hash"].clone()).unwrap();
    let extra_data: Bytes = serde_json::from_value(block["extraData"].clone()).unwrap();
    let vrf = decode_boundary_artifact(&extra_data)
        .unwrap()
        .expect("genesis VRF boundary outcome");
    let expected_founders: BTreeSet<_> = cluster.addresses.iter().copied().collect();
    assert_eq!(vrf.epoch, 0);
    assert_eq!(block["number"], "0x1");
    assert!(vrf.is_full_dkg && !vrf.outcome.is_empty());
    assert!(!vrf.vrf_group_public_key_bytes.is_empty());
    assert_eq!(
        vrf.reshare
            .new_active_set
            .iter()
            .copied()
            .collect::<BTreeSet<_>>(),
        expected_founders
    );

    let mut boundary = None;
    let mut bootstrap = None;
    for transaction in block["transactions"].as_array().expect("full block body") {
        if transaction["to"] != json!(OUTBE_SYSTEM_TX_ADDRESS) {
            continue;
        }
        let calldata: Bytes = serde_json::from_value(transaction["input"].clone()).unwrap();
        let SystemTx::Consensus {
            step_index,
            payload,
        } = SystemTx::decode(&calldata).unwrap()
        else {
            continue;
        };
        let slot = match step_index {
            CONSENSUS_BOUNDARY_OUTCOME_STEP_INDEX => &mut boundary,
            CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX => &mut bootstrap,
            _ => continue,
        };
        assert!(
            slot.replace(payload).is_none(),
            "duplicate founding system tx"
        );
        let receipt = producer
            .request("eth_getTransactionReceipt", json!([transaction["hash"]]))
            .await
            .expect("founding system transaction receipt");
        assert_eq!(receipt["blockHash"], json!(hash));
        assert_eq!(receipt["status"], "0x1");
    }
    assert_eq!(
        decode_boundary_artifact(&boundary.expect("VRF boundary system transaction")).unwrap(),
        Some(vrf.clone()),
        "block-1 system transaction must carry the VRF outcome in its own header"
    );
    let bootstrap = validate_tee_bootstrap_payload(
        &bootstrap.expect("block-1 TEE bootstrap system transaction"),
        1,
    )
    .unwrap();
    assert_eq!(
        bootstrap.authority.policy.attestation_mode,
        AttestationMode::NoAttestationDev
    );
    assert_eq!(
        bootstrap.authority.committee_snapshot_hash,
        vrf.committee_set_hash
    );
    assert_eq!(bootstrap.authority.committee_snapshot_block, 1);
    assert!(!bootstrap
        .authority
        .network_encryption_group_public_key
        .is_empty());
    assert_eq!(
        bootstrap
            .participants
            .iter()
            .map(|entry| Address::from(entry.validator_binding.validator))
            .collect::<BTreeSet<_>>(),
        expected_founders
    );

    let offer_public = bootstrap.authority.network_encryption_public_key;
    assert_ne!(offer_public, B256::ZERO);
    let slot = format!("0x{TEE_REGISTRY_OFFER_PUBLIC_SLOT_V1:x}");
    let unseeded = producer
        .request(
            "eth_getStorageAt",
            json!([TEE_REGISTRY_ADDRESS, slot, "0x0"]),
        )
        .await
        .expect("genesis registry offer slot");
    assert_eq!(unseeded, json!(B256::ZERO));

    for (rpc, validator) in cluster.rpcs.iter().zip(&cluster.addresses) {
        let local = rpc
            .request("eth_getBlockByNumber", json!(["0x1", false]))
            .await
            .expect("founder's finalized block 1");
        assert_eq!(local["hash"], json!(hash), "founder {validator}");
        let registered = rpc
            .request(
                "eth_getStorageAt",
                json!([TEE_REGISTRY_ADDRESS, slot, "0x1"]),
            )
            .await
            .expect("block-1 registry offer key");
        assert_eq!(registered, json!(offer_public), "founder {validator}");
        let binding = rpc
            .call_at(
                TEE_REGISTRY_ADDRESS,
                ITeeRegistryV1::validatorEnclaveBindingCall {
                    validator: *validator,
                },
                json!("0x1"),
            )
            .await;
        let submitted = bootstrap
            .participants
            .iter()
            .find(|entry| Address::from(entry.validator_binding.validator) == *validator)
            .unwrap();
        assert!(binding.exists);
        assert_eq!(binding.enclaveId, submitted.intent.enclave_id);
    }
    (hash, offer_public)
}

async fn wait_for_resident_canaries(cluster: &mut Cluster) {
    // Canary RPC is local diagnostic evidence of encrypt/decrypt; it is not block proof.
    let started = Instant::now();
    loop {
        cluster.assert_running(cluster.processes.len());
        let mut all_decrypted = true;
        for rpc in &cluster.rpcs {
            let status = rpc
                .request("outbe_consensusStatus", json!([]))
                .await
                .unwrap_or_default();
            let enclave = &status["enclave"];
            all_decrypted &= [
                enclave["state"] == "ready",
                enclave["ready"] == true,
                enclave["offerKeyReady"] == true,
                enclave["lastOkAgoMillis"].as_u64().is_some(),
                enclave["lastCanaryLatencyMs"].as_u64().is_some(),
                enclave["consecutiveFailures"] == 0,
                enclave["healthProbeSupported"] == false,
            ]
            .into_iter()
            .all(|valid| valid);
        }
        if all_decrypted {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "resident canaries did not decrypt: {}",
            cluster.logs()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn assert_owner_only_exports(cluster: &Cluster) {
    let owner = OutbeEvmSigner::from_secret_bytes([0x55; 32]).unwrap();
    let other = OutbeEvmSigner::from_secret_bytes([0x33; 32]).unwrap();
    assert_ne!(other.address(), owner.address());
    // RFC 7748 section 6.1 public fixture; it is a client key, not a seeded TEE key.
    let ephemeral = "0x8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a"
        .parse::<B256>()
        .unwrap();
    let message = [
        b"outbe/gratis/derive-keys/v1".as_slice(),
        owner.address().as_slice(),
        ephemeral.as_slice(),
    ]
    .concat();
    let digest = eip191_hash_message(message);
    let params = |signer: &OutbeEvmSigner| {
        json!([
            "Gratis",
            owner.address(),
            ephemeral,
            Bytes::copy_from_slice(&signer.sign_hash(&digest).unwrap())
        ])
    };

    let mut encapsulations = BTreeSet::new();
    for rpc in &cluster.rpcs {
        let success = derive(rpc, params(&owner)).await;
        assert!(
            success["error"].is_null(),
            "owner authentication failed at {}: {}",
            rpc.url(),
            success["error"]
        );
        let envelope: SealedKeys = serde_json::from_value(success["result"].clone()).unwrap();
        // Two 32-byte keys plus a Poly1305 tag; no plaintext keys in the response.
        assert_eq!(envelope.sealed.len(), 80);
        assert_eq!(envelope.nonce.len(), 12);
        assert_ne!(envelope.enclave_ephemeral_pubkey, B256::ZERO);
        assert!(encapsulations.insert(envelope.enclave_ephemeral_pubkey));

        let denied = derive(rpc, params(&other)).await;
        assert!(
            denied.get("result").is_none(),
            "wrong signer received a result"
        );
        assert_eq!(denied["error"]["code"], -32602);
        let message = denied["error"]["message"].as_str().unwrap();
        assert!(message.contains("does not control account"), "{message}");
    }
}

async fn derive(rpc: &Rpc, params: Value) -> Value {
    rpc.raw("outbe_deriveKeys", params)
        .await
        .expect("deriveKeys JSON-RPC response")
}
