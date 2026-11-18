use crate::{
    founding::assert_no_sealed_files,
    joining::{fund, JoinIdentity, JoinProcess, Role},
    operator,
    rpc::Rpc,
    tee_cluster::Cluster,
    upstream::{same_finalized_block, FUNDS},
};
use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_sol_types::SolEvent as _;
use outbe_primitives::{
    addresses::TEE_REGISTRY_ADDRESS,
    tee_registry_abi_v1::{ITeeRegistryV1, NodeEnclaveBindingV1View},
};
use serde_json::json;
use std::time::{Duration, Instant};

pub const LEASE: u64 = 7_200;
pub const CLOCK_STEP: i64 = 4_000;
pub const REBOOTED: usize = 3;

#[tokio::test]
async fn restarted_founder_recovers_key_and_keyed_follower_renews_at_zero_balance() {
    let keyed = JoinIdentity::generate(B256::repeat_byte(32));
    let keyless = JoinIdentity::generate(B256::repeat_byte(33));
    let expiring = JoinIdentity::generate(B256::repeat_byte(34));
    let mut cluster = Cluster::start_with_lease(
        json!({"chain_id":424242, "epoch_length_blocks":1200}),
        Some(LEASE),
        |genesis, founders| {
            for founder in founders {
                genesis["alloc"][format!("{founder:x}")] = json!({"balance": U256::from(FUNDS)});
            }
            fund(genesis, &keyed, U256::from(FUNDS));
            fund(genesis, &expiring, U256::from(FUNDS));
        },
    );
    let baseline = cluster.wait_finalized_on(2, 4).await;
    let upstream = cluster.rpcs[0].clone();
    let mut prior = Vec::new();
    for addr in &cluster.addresses {
        let binding = validator_binding(&upstream, *addr).await;
        assert!(binding.exists);
        prior.push(binding);
    }
    assert!(!keyed.binding(&upstream).await.exists);
    let mut keyed_node = keyed.start(&cluster, Role::KeyedFullNode);
    let mut keyless_node = keyless.start(&cluster, Role::KeylessFollower);
    let expired_at = take_initial_validator_offline(&expiring, &cluster).await;
    let keyed_pid = keyed_node.process.id();
    assert_follower(&mut keyed_node, &upstream, baseline, "ready").await;
    assert_follower(&mut keyless_node, &upstream, baseline, "disabled").await;
    let keyed_before = keyed.binding(&upstream).await;
    assert!(keyed_before.exists);
    assert!(!keyless.binding(&upstream).await.exists);
    operator::drain_balance(&upstream, &keyed.evm, cluster.addresses[0]).await;

    reboot_founder_and_wait(&mut cluster, &prior[REBOOTED], CLOCK_STEP).await;

    let renew = Instant::now();
    wait_for_renewal(
        &mut cluster,
        &mut keyed_node,
        &keyed,
        &keyed_before,
        baseline,
    )
    .await;
    assert!(upstream
        .account("eth_getBalance", keyed.evm.address())
        .await
        .is_zero());
    assert_eq!(keyed_node.process.id(), keyed_pid);
    assert_follower(&mut keyed_node, &upstream, baseline, "ready").await;
    assert_follower(&mut keyless_node, &upstream, baseline, "disabled").await;
    wait_for_local_renewals(&mut cluster, &prior, baseline).await;
    println!(
        "Keyed follower and three original no-upstream founder renewals observed in {:?}",
        renew.elapsed()
    );

    prove_paid_expired_rejoin(&mut cluster, &expiring, &expired_at).await;
    assert_no_sealed_files(&keyed.datadir());
    assert_no_sealed_files(&keyless.datadir());
    assert_no_sealed_files(&expiring.datadir());
}

async fn take_initial_validator_offline(
    identity: &JoinIdentity,
    cluster: &Cluster,
) -> NodeEnclaveBindingV1View {
    let mut node = identity.start(cluster, Role::Validator);
    node.wait_status("ready enclave before the downtime window", |status| {
        status["enclave"]["ready"] == true
    })
    .await;
    let binding = identity.binding(&cluster.rpcs[0]).await;
    assert!(binding.exists);
    node.process.stop();
    binding
}

async fn prove_paid_expired_rejoin(
    cluster: &mut Cluster,
    identity: &JoinIdentity,
    prior: &NodeEnclaveBindingV1View,
) {
    let upstream = cluster.rpcs[0].clone();
    let clock_driver = validator_binding(&upstream, cluster.addresses[REBOOTED]).await;
    reboot_founder_and_wait(cluster, &clock_driver, CLOCK_STEP * 2).await;
    let deadline = Instant::now() + Duration::from_secs(180);
    let expires_at = loop {
        cluster.assert_running(4);
        let block = upstream
            .request("eth_getBlockByNumber", json!(["finalized", false]))
            .await
            .expect("finalized block");
        if hex_u64(&block["timestamp"]) >= prior.validUntil {
            break hex_u64(&block["number"]);
        }
        assert!(
            Instant::now() < deadline,
            "consensus did not advance beyond the lease"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let expired = identity.binding(&upstream).await;
    assert_eq!(expired.bindingId, prior.bindingId);
    assert_eq!(expired.renewalNonce, prior.renewalNonce);

    let started = Instant::now();
    let mut node = identity.start(cluster, Role::Validator);
    node.wait_status("admitted after paid expired rejoin", |status| {
        status["enclave"]["ready"] == true
    })
    .await;
    let renewed = identity.binding(&upstream).await;
    assert_ne!(renewed.enclaveId, expired.enclaveId);
    assert_eq!(renewed.bindingVersion, expired.bindingVersion + 1);
    assert_eq!(
        renewed.nodeHostAuthorizationHash,
        expired.nodeHostAuthorizationHash
    );
    let receipt = registry_receipt(
        &upstream,
        expires_at,
        ITeeRegistryV1::EnclaveRegisteredV1::SIGNATURE_HASH,
        renewed.nodeIdHash,
    )
    .await;
    let (before, after) = receipt_balances(&upstream, &receipt).await;
    assert!(before > after, "expired rejoin did not debit gas");
    println!("Paid expired rejoin observed in {:?}", started.elapsed());
}

async fn assert_follower(node: &mut JoinProcess, upstream: &Rpc, height: u64, state: &str) {
    let status = node
        .wait_status(state, |status| {
            status["enclave"]["state"] == state
                && status["lastFinalizedBlock"].as_u64().unwrap_or_default() >= height
        })
        .await;
    assert_eq!(status["mode"], "follower");
    assert_eq!(status["isValidator"], false);
    assert_eq!(status["hasThresholdShares"], false);
    same_finalized_block(upstream, &node.rpc).await;
}

pub async fn reboot_founder_and_wait(
    cluster: &mut Cluster,
    prior: &NodeEnclaveBindingV1View,
    clock_offset: i64,
) {
    let upstream = cluster.rpcs[0].clone();
    let offer = offer_key(&upstream).await;
    assert!(!offer.is_zero());
    let rejoin = Instant::now();
    let old_pid = cluster.processes[REBOOTED].id();
    assert_ne!(cluster.reboot_founder(REBOOTED, clock_offset), old_pid);
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        cluster.assert_running(4);
        let current = validator_binding(&upstream, cluster.addresses[REBOOTED]).await;
        let ready = cluster.rpcs[REBOOTED]
            .request("outbe_consensusStatus", json!([]))
            .await
            .is_some_and(|value| {
                value["enclave"]["ready"] == true
                    && value["mode"] == "signer"
                    && value["hasThresholdShares"] == true
            });
        if current.enclaveId != prior.enclaveId && ready {
            assert!(current.exists);
            assert_eq!(current.nodeIdHash, prior.nodeIdHash);
            assert_eq!(
                current.nodeHostAuthorizationHash,
                prior.nodeHostAuthorizationHash
            );
            assert_eq!(current.bindingVersion, prior.bindingVersion + 1);
            assert!(is_revoked(&upstream, prior.enclaveId).await);
            assert_eq!(offer_key(&cluster.rpcs[REBOOTED]).await, offer);
            same_finalized_block(&upstream, &cluster.rpcs[REBOOTED]).await;
            assert_no_sealed_files(&cluster.root.join(format!("validator-{REBOOTED}/data")));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "founder active rejoin stalled: {}",
            cluster.logs()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!(
        "Active rejoin and recovered key observed in {:?}",
        rejoin.elapsed()
    );
}

pub async fn wait_for_renewal(
    cluster: &mut Cluster,
    process: &mut JoinProcess,
    identity: &JoinIdentity,
    previous: &NodeEnclaveBindingV1View,
    baseline: u64,
) {
    let upstream = cluster.rpcs[0].clone();
    let renew = Instant::now();
    let deadline = renew + Duration::from_secs(180);
    loop {
        cluster.assert_running(4);
        process.assert_running();
        let latest = identity.binding(&upstream).await;
        if latest.renewalNonce > previous.renewalNonce {
            assert_eq!(latest.enclaveId, previous.enclaveId);
            assert_eq!(latest.bindingId, previous.bindingId);
            assert_eq!(latest.bindingVersion, previous.bindingVersion);
            assert_eq!(latest.registrationVersion, previous.registrationVersion + 1);
            assert_eq!(latest.renewalNonce, previous.renewalNonce + 1);
            assert_eq!(latest.validUntil, previous.validUntil + LEASE);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "upstream-driven renewal stalled: {}",
            cluster.logs()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_zero_fee_renewal(&upstream, baseline, previous.nodeIdHash).await;
}

async fn wait_for_local_renewals(
    cluster: &mut Cluster,
    prior: &[NodeEnclaveBindingV1View],
    baseline: u64,
) {
    let upstream = cluster.rpcs[0].clone();
    let deadline = Instant::now() + Duration::from_secs(180);
    for (index, before) in prior.iter().enumerate().take(REBOOTED) {
        let addr = cluster.addresses[index];
        loop {
            if validator_binding(&upstream, addr).await.renewalNonce > before.renewalNonce {
                break;
            }
            cluster.assert_running(4);
            assert!(
                Instant::now() < deadline,
                "local founder renewal stalled: {addr}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_zero_fee_renewal(&upstream, baseline, before.nodeIdHash).await;
    }
}

pub async fn validator_binding(rpc: &Rpc, addr: Address) -> NodeEnclaveBindingV1View {
    rpc.call_at(
        TEE_REGISTRY_ADDRESS,
        ITeeRegistryV1::validatorEnclaveBindingCall { validator: addr },
        json!("finalized"),
    )
    .await
}

async fn offer_key(rpc: &Rpc) -> U256 {
    rpc.call_at(
        TEE_REGISTRY_ADDRESS,
        ITeeRegistryV1::tributeOfferPublicKeyCall {},
        json!("finalized"),
    )
    .await
}

async fn is_revoked(rpc: &Rpc, enclave: B256) -> bool {
    let mut mapping = [0_u8; 64];
    mapping[..32].copy_from_slice(enclave.as_slice());
    // Solidity mapping base slot from the canonical TeeRegistry storage layout.
    mapping[63] = 59;
    let value = rpc
        .request(
            "eth_getStorageAt",
            json!([TEE_REGISTRY_ADDRESS, keccak256(mapping), "finalized"]),
        )
        .await
        .expect("TEE revocation storage");
    let word: B256 = serde_json::from_value(value).unwrap();
    word == B256::with_last_byte(1)
}

async fn assert_zero_fee_renewal(rpc: &Rpc, start: u64, node: B256) {
    let receipt = registry_receipt(
        rpc,
        start,
        ITeeRegistryV1::EnclaveRenewedV1::SIGNATURE_HASH,
        node,
    )
    .await;
    // The receipt's envelope-based effectiveGasPrice does not measure Outbe fee settlement.
    let (before, after) = receipt_balances(rpc, &receipt).await;
    assert_eq!(before, after, "renewEnclave debited the operator");
}

async fn receipt_balances(rpc: &Rpc, receipt: &serde_json::Value) -> (U256, U256) {
    let block = hex_u64(&receipt["blockNumber"]);
    assert!(block > 0);
    let account = &receipt["from"];
    (
        balance_at(rpc, account, block - 1).await,
        balance_at(rpc, account, block).await,
    )
}

async fn balance_at(rpc: &Rpc, account: &serde_json::Value, block: u64) -> U256 {
    let value = rpc
        .request("eth_getBalance", json!([account, format!("{block:#x}")]))
        .await
        .expect("historical operator balance");
    U256::from_str_radix(value.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}

async fn registry_receipt(rpc: &Rpc, start: u64, event: B256, node: B256) -> serde_json::Value {
    let response = rpc
        .request(
            "eth_getLogs",
            json!([{
                "address": TEE_REGISTRY_ADDRESS,
                "fromBlock": format!("{start:#x}"),
                "toBlock": "finalized",
                "topics": [event, node],
            }]),
        )
        .await
        .expect("TEE registry logs");
    let log = response
        .as_array()
        .unwrap()
        .last()
        .expect("finalized registry event");
    let receipt = rpc
        .request("eth_getTransactionReceipt", json!([log["transactionHash"]]))
        .await
        .expect("TEE registry receipt");
    assert_eq!(receipt["blockHash"], log["blockHash"]);
    assert_eq!(receipt["status"], "0x1");
    receipt
}

fn hex_u64(value: &serde_json::Value) -> u64 {
    u64::from_str_radix(value.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}
