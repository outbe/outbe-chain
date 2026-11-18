use crate::{leases::validator_binding, operator, rpc::Rpc, tee_cluster::Cluster, upstream::FUNDS};
use alloy_primitives::{eip191_hash_message, Address, Bytes, B256, U256};
use outbe_consensus::bls::{self, KeyBackend};
use outbe_primitives::signer::OutbeEvmSigner;
use outbe_validatorset::runtime::status;
use serde_json::{json, Value};
use std::{
    process::Command,
    time::{Duration, Instant},
};

const SUBJECT: usize = 3;

#[tokio::test]
async fn live_jailed_validator_keeps_its_key_and_needs_readiness_and_reshare_to_vote() {
    let started = Instant::now();
    let mut cluster = Cluster::start_with_genesis(
        json!({"chain_id":424242,"epoch_length_blocks":24}),
        |genesis, founders| {
            let offender = founders[SUBJECT];
            genesis["alloc"][format!("{offender:x}")] = json!({"balance": U256::from(FUNDS)});
            let slash =
                &mut genesis["alloc"]["000000000000000000000000000000000000ee01"]["storage"];
            slash[format!("0x{:064x}", 1)] = json!(format!("0x{:064x}", 300));
            slash[format!("0x{:064x}", 12)] = json!(format!("0x{:064x}", 4));
            genesis["config"]["dkgPrepareWindowBlocks"] = json!(8);
        },
    );
    cluster.wait_finalized_on(2, 4).await;
    let upstream = cluster.rpcs[0].clone();
    let jailed_rpc = cluster.rpcs[SUBJECT].clone();
    let addr = cluster.addresses[SUBJECT];
    let evm = bls::load_validator_evm_signer(
        &cluster
            .root
            .join(format!("validator-{SUBJECT}/evm-key.hex")),
        &KeyBackend::Plaintext,
    )
    .unwrap();
    operator::add_slash_margin(&upstream, &evm).await;
    let params = owner_derive_params();
    let active = jailed_rpc
        .raw("outbe_deriveKeys", params.clone())
        .await
        .unwrap();
    assert!(active["result"]["sealed"].is_string(), "{active}");
    assert_eq!(
        key_export_error(&jailed_rpc).await["message"],
        "enclave returned error: upgrade proof dimensions exceed limits"
    );
    let pid = cluster.processes[SUBJECT].id();
    assert_eq!(validator(&upstream, addr).await["status"], status::ACTIVE);

    let prior_epoch = consensus_status(&upstream).await["lastDkgActivationHeight"]
        .as_u64()
        .unwrap();
    induce_downtime_jail(&mut cluster, &upstream, &jailed_rpc, addr, pid).await;
    wait_for_closed_authority(&mut cluster, &jailed_rpc, &params).await;
    let export = key_export_error(&jailed_rpc).await;
    assert_eq!(export["code"], -32603);
    assert_eq!(export["message"], "PrivateExecutionUnavailable");
    assert_eq!(cluster.processes[SUBJECT].id(), pid);
    restore_voting_after_reshare(&mut cluster, &upstream, &jailed_rpc, &evm, prior_epoch).await;
    assert_eq!(cluster.processes[SUBJECT].id(), pid);
    let restored = jailed_rpc.raw("outbe_deriveKeys", params).await.unwrap();
    assert!(restored["result"]["sealed"].is_string(), "{restored}");

    induce_downtime_jail(&mut cluster, &upstream, &jailed_rpc, addr, pid).await;
    prove_cold_jailed_waits(&mut cluster, &upstream, addr).await;
    println!(
        "Warm and cold jail gates observed in {:?}",
        started.elapsed()
    );
}

async fn key_export_error(rpc: &Rpc) -> Value {
    // A deliberately empty proof distinguishes the authority gate from proof validation.
    let envelope = rpc.raw(
        "outbe_upgradeKeyV1",
        json!(["0x", {"anchorOutcome":"0x", "committeeTransitions":[], "admission":"0x"}, true]),
    ).await.unwrap();
    assert!(envelope["result"].is_null(), "{envelope}");
    envelope["error"].clone()
}

async fn wait_for_closed_authority(cluster: &mut Cluster, rpc: &Rpc, params: &Value) {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        cluster.assert_running(4);
        let info = consensus_status(rpc).await;
        let denied = rpc
            .raw("outbe_deriveKeys", params.clone())
            .await
            .unwrap_or(Value::Null);
        if info["enclave"]["offerKeyReady"] == true
            && info["hasThresholdShares"] == false
            && denied["error"]["code"] == -32603
            && denied["result"].is_null()
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "JAILED did not close authority: {info} {denied}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn restore_voting_after_reshare(
    cluster: &mut Cluster,
    upstream: &Rpc,
    local: &Rpc,
    evm: &OutbeEvmSigner,
    prior_epoch: u64,
) {
    // Unjail is legal only after a boundary drops the retained jailed member.
    wait_activation(upstream, prior_epoch).await;
    operator::add_slash_margin(upstream, evm).await;
    operator::unjail(upstream, evm).await;
    assert_eq!(
        validator(upstream, evm.address()).await["status"],
        status::PENDING
    );
    let after_unjail = consensus_status(local).await;
    assert_ne!(after_unjail["mode"], "signer");
    assert_eq!(after_unjail["hasThresholdShares"], false);

    let before_reshare = consensus_status(upstream).await["lastDkgActivationHeight"]
        .as_u64()
        .unwrap();
    operator::confirm_ready(upstream, evm).await;
    wait_activation(upstream, before_reshare).await;
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        cluster.assert_running(4);
        let info = consensus_status(local).await;
        if info["mode"] == "signer" && info["hasThresholdShares"] == true {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no signer after confirm-ready/reshare: {info}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn prove_cold_jailed_waits(cluster: &mut Cluster, upstream: &Rpc, operator: Address) {
    let prior = validator_binding(upstream, operator).await;
    let pid = cluster.reboot_founder(SUBJECT, 0);
    let log_path = cluster.root.join(format!("validator-{SUBJECT}/node.log"));
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        cluster.assert_running(4);
        let log = std::fs::read_to_string(&log_path).unwrap();
        if log.contains("Enrollment(WaitForUnjail)") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "cold jailed validator did not wait for unjail"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let height = upstream.finalized().await.unwrap();
    cluster.wait_finalized_on(height + 2, SUBJECT).await;
    cluster.assert_running(4);
    assert_eq!(cluster.processes[SUBJECT].id(), pid);
    let unchanged = validator_binding(upstream, operator).await;
    assert_eq!(unchanged.enclaveId, prior.enclaveId);
    assert_eq!(unchanged.bindingVersion, prior.bindingVersion);
    assert!(cluster.rpcs[SUBJECT]
        .request("outbe_consensusStatus", json!([]))
        .await
        .is_none());
}

async fn induce_downtime_jail(
    cluster: &mut Cluster,
    upstream: &Rpc,
    subject: &Rpc,
    address: Address,
    pid: u32,
) {
    let deadline = Instant::now() + Duration::from_secs(180);
    while validator(upstream, address).await["status"] != status::JAILED {
        cluster.assert_running(4);
        assert!(
            Instant::now() < deadline,
            "downtime did not jail the validator"
        );
        let height = upstream.finalized().await.unwrap();
        while subject.finalized().await.unwrap_or_default() < height {
            cluster.assert_running(4);
            assert!(
                Instant::now() < deadline,
                "validator did not catch up before the next pause"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        signal(pid, "STOP");
        // Model missed consensus rounds without suspending a local persistence deadline.
        let resume_by = Instant::now() + Duration::from_secs(10);
        while Instant::now() < resume_by {
            if validator(upstream, address).await["status"] == status::JAILED {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        signal(pid, "CONT");
    }
}

async fn validator(rpc: &Rpc, addr: Address) -> Value {
    rpc.request("outbe_getValidator", json!([addr]))
        .await
        .expect("validator status")
}

async fn consensus_status(rpc: &Rpc) -> Value {
    rpc.request("outbe_consensusStatus", json!([]))
        .await
        .unwrap_or(Value::Null)
}

async fn wait_activation(rpc: &Rpc, prior: u64) {
    let deadline = Instant::now() + Duration::from_secs(180);
    while consensus_status(rpc).await["lastDkgActivationHeight"]
        .as_u64()
        .unwrap_or_default()
        <= prior
    {
        assert!(
            Instant::now() < deadline,
            "DKG boundary after {prior} not observed"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn signal(pid: u32, signal: &str) {
    assert!(Command::new("kill")
        .args([format!("-{signal}"), pid.to_string()])
        .status()
        .unwrap()
        .success());
}

fn owner_derive_params() -> Value {
    let owner = OutbeEvmSigner::from_secret_bytes([0x5f; 32]).unwrap();
    let recipient: B256 = "0x8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a"
        .parse()
        .unwrap();
    let digest = eip191_hash_message(
        [
            b"outbe/gratis/derive-keys/v1".as_slice(),
            owner.address().as_slice(),
            recipient.as_slice(),
        ]
        .concat(),
    );
    json!([
        "Gratis",
        owner.address(),
        recipient,
        Bytes::copy_from_slice(&owner.sign_hash(&digest).unwrap())
    ])
}
