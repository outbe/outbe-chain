use crate::{
    founding::assert_no_sealed_files,
    joining::{JoinIdentity, Role},
    leases::{
        reboot_founder_and_wait, validator_binding, wait_for_renewal, CLOCK_STEP, LEASE, REBOOTED,
    },
    operator,
    rpc::Rpc,
    tee_cluster::Cluster,
};
use alloy_primitives::{B256, U256};
use outbe_primitives::signer::OutbeEvmSigner;
use serde_json::json;
use std::time::Instant;

pub(crate) const FUNDS: u128 = 1_000_000_000 * 10_u128.pow(18);

#[tokio::test]
async fn validator_acquires_resident_key_before_staking_and_joins_in_one_process() {
    let started = Instant::now();
    let identity = JoinIdentity::generate(B256::repeat_byte(31));
    let funder = OutbeEvmSigner::from_secret_bytes([0x5a; 32]).unwrap();
    let mut cluster = Cluster::start_with_lease(
        json!({"chain_id":424242, "epoch_length_blocks":24}),
        Some(LEASE),
        |genesis, founders| {
            genesis["config"]["minBlockTimeMs"] = json!(250);
            genesis["config"]["dkgPrepareWindowBlocks"] = json!(8);
            genesis["alloc"][format!("{:x}", funder.address())] =
                json!({"balance": U256::from(FUNDS * 2)});
            genesis["alloc"][format!("{:x}", founders[REBOOTED])] =
                json!({"balance": U256::from(FUNDS)});
        },
    );
    let height = cluster.wait_finalized_on(2, 4).await;
    let upstream = cluster.rpcs[0].clone();
    assert!(!identity.binding(&upstream).await.exists);
    assert!(upstream
        .account("eth_getBalance", identity.evm.address())
        .await
        .is_zero());
    let mut node = identity.start(&cluster, Role::Validator);
    let pid = node.process.id();
    cluster.wait_finalized_on(height + 2, 4).await;
    node.assert_running();
    assert_eq!(pid, node.process.id());
    assert!(!identity.binding(&upstream).await.exists);
    operator::transfer(
        &upstream,
        &funder,
        identity.evm.address(),
        U256::from(FUNDS),
    )
    .await;
    let before_stake = node
        .wait_status("enclave-backed bootstrap before staking", |status| {
            status["mode"] == "upstreamBootstrap"
                && status["enclave"]["ready"] == true
                && status["lastFinalizedBlock"].as_u64().unwrap_or_default() >= 2
        })
        .await;
    let registration = identity.binding(&upstream).await;
    assert!(registration.exists && !registration.enclaveId.is_zero());
    assert_ne!(before_stake["hasThresholdShares"], true);
    same_finalized_block(&upstream, &node.rpc).await;

    let clock_driver = validator_binding(&upstream, cluster.addresses[REBOOTED]).await;
    reboot_founder_and_wait(&mut cluster, &clock_driver, CLOCK_STEP).await;
    wait_for_renewal(&mut cluster, &mut node, &identity, &registration, height).await;
    assert!(upstream
        .request("outbe_getValidator", json!([identity.evm.address()]))
        .await
        .expect("validator registry RPC")
        .is_null());

    operator::stake(
        &cluster.rpcs[0],
        &identity.evm,
        &identity.bls,
        identity.node_id,
        node.consensus,
    )
    .await;
    let verifier = node
        .wait_status("staked verifier", |status| status["mode"] == "verifier")
        .await;
    assert_eq!(verifier["hasThresholdShares"], false);
    let previous = verifier["lastDkgActivationHeight"].as_u64().unwrap();
    operator::confirm_ready(&cluster.rpcs[0], &identity.evm).await;
    node.wait_status("voting signer after DKG reshare", |status| {
        status["mode"] == "signer"
            && status["hasThresholdShares"] == true
            && status["lastDkgActivationHeight"]
                .as_u64()
                .unwrap_or_default()
                > previous
    })
    .await;
    same_finalized_block(&cluster.rpcs[0], &node.rpc).await;
    assert_eq!(node.process.id(), pid, "validator process restarted");
    let active = upstream
        .request("outbe_getValidator", json!([identity.evm.address()]))
        .await
        .expect("active validator record");
    assert_eq!(
        active["status"],
        outbe_validatorset::runtime::status::ACTIVE
    );
    assert_eq!(active["hasBlsShare"], true);
    assert_no_sealed_files(&identity.datadir());
    println!(
        "Single-process validator promotion and pre-stake renewal took {:?}",
        started.elapsed()
    );
}

pub async fn same_finalized_block(upstream: &Rpc, follower: &Rpc) {
    let local = follower
        .request("eth_getBlockByNumber", json!(["finalized", false]))
        .await
        .expect("local finalized block");
    let upstream = upstream
        .request("eth_getBlockByNumber", json!([local["number"], false]))
        .await
        .expect("upstream canonical block");
    assert!(local["hash"].is_string() && local["stateRoot"].is_string());
    assert_eq!(local["hash"], upstream["hash"]);
    assert_eq!(local["stateRoot"], upstream["stateRoot"]);
}
