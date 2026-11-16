//! Immutable black-box RED/GREEN contract for automatic equivocation slashing.
#[path = "equivocation_auto_slashing/cluster.rs"]
mod cluster;
#[path = "equivocation_auto_slashing/network.rs"]
mod network;
#[path = "equivocation_auto_slashing/rpc.rs"]
mod rpc;
#[path = "equivocation_auto_slashing/services.rs"]
mod services;

use alloy_primitives::{address, U256};
use cluster::Cluster;
use network::{Injector, Kind};
use std::time::{Duration, Instant};

alloy_sol_types::sol! {
    interface ISlashIndicator { function getFelonyCount(address validator) external view returns (uint64); }
    interface IStaking { function getStake(address validator) external view returns (uint256); }
    interface IValidatorSet {
        function validatorByAddress(address addr) external view returns (address validatorAddress, bytes consensusPubkey, uint256 stake, uint8 status, uint64 slashCount, uint64 missedBlocks, uint64 missedVotes, uint64 blocksProposed, uint64 joinedAtHeight, uint64 deactivatedAtHeight, uint64 unbondingEnd, bool hasBLSShare);
    }
}
const SLASH: alloy_primitives::Address = address!("000000000000000000000000000000000000ee01");
const STAKING: alloy_primitives::Address = address!("000000000000000000000000000000000000ee02");
const VALIDATORS: alloy_primitives::Address = address!("000000000000000000000000000000000000ee00");

async fn scenario(kind: Kind) {
    let mut cluster = Cluster::start();
    cluster.wait_finalized(2).await;
    cluster.zero_balances().await;
    let rpc = cluster.rpcs[0].clone();
    let offender = cluster.addresses[3];
    let stake = rpc
        .call(
            STAKING,
            IStaking::getStakeCall {
                validator: offender,
            },
        )
        .await;
    assert!(stake > U256::ZERO);
    let mut nonces = Vec::new();
    for address in cluster.addresses.iter().take(3) {
        nonces.push(rpc.account("eth_getTransactionCount", *address).await);
    }
    cluster.processes[3].stop();
    let injector = Injector::start(&cluster.root, cluster.peers.clone(), kind);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let started = Instant::now();
    loop {
        let mut detected = false;
        for port in cluster.metrics.iter().take(3) {
            let text = client
                .get(format!("http://127.0.0.1:{port}/metrics"))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            detected |= text.lines().any(|line| {
                line.starts_with("reth_outbe_consensus_byzantine_evidence_total{")
                    && line.contains(&format!("kind=\"{}\"", kind.label()))
                    && line
                        .split_whitespace()
                        .last()
                        .and_then(|value| value.parse::<u64>().ok())
                        .is_some_and(|value| value >= 1)
            });
        }
        if detected {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "Stage 1 evidence missing for {kind:?}: {}",
            cluster.logs()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    drop(injector);
    cluster.zero_balances().await;
    println!("Stage 1 PASS {kind:?}: byzantine_evidence_total >= 1; honest EOA balances == 0");
    let detected_at = rpc.finalized().await.unwrap();
    cluster.wait_finalized(detected_at + 8).await;
    let count = rpc
        .call(
            SLASH,
            ISlashIndicator::getFelonyCountCall {
                validator: offender,
            },
        )
        .await;
    assert_eq!(
        count, 1,
        "Stage 2 {kind:?}: felony_count == {count}, expected 1"
    );
    let state = rpc
        .call(
            VALIDATORS,
            IValidatorSet::validatorByAddressCall { addr: offender },
        )
        .await;
    // JAILED with a retained live BLS share is the public JailRetained lifecycle.
    assert_eq!(state.status, 6, "JailRetained status");
    assert!(state.hasBLSShare, "JailRetained must retain the live share");
    assert_eq!(
        rpc.call(
            STAKING,
            IStaking::getStakeCall {
                validator: offender
            }
        )
        .await,
        stake * U256::from(92) / U256::from(100)
    );
    cluster.zero_balances().await;
    let mut senders = 0;
    for (address, before) in cluster.addresses.iter().take(3).zip(nonces) {
        let after = rpc.account("eth_getTransactionCount", *address).await;
        assert!(
            after == before || after == before + U256::from(1),
            "unexpected nonce change"
        );
        senders += usize::from(after == before + U256::from(1));
    }
    assert_eq!(
        senders, 1,
        "exactly one zero-balance honest sender submitted the slash"
    );
}
#[tokio::test]
async fn conflicting_notarize() {
    scenario(Kind::ConflictingNotarize).await;
}
#[tokio::test]
async fn conflicting_finalize() {
    scenario(Kind::ConflictingFinalize).await;
}
#[tokio::test]
async fn nullify_finalize() {
    scenario(Kind::NullifyFinalize).await;
}
