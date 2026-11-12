use std::{path::Path, time::Duration};

use alloy_primitives::Address;
use eyre::{Context, Result};

use crate::{
    support::{
        process::{self, ROOT},
        progress::WatchDisplay,
    },
    validator::{
        journal::Journal,
        rpc::{Block, ConsensusStatus, EpochInfo, RpcClient, ValidatorRecord},
        validator_address,
        wait::{eta_suffix, yes_no, CatchupEta},
    },
};

const LOCAL_RPC: &str = "http://127.0.0.1:8545";
const FULL_NODE: &str = "outbe-full-node.service";
const VALIDATOR_SERVICE: &str = "outbe-validator.service";
const ENCLAVE_SERVICE: &str = "outbe-enclave.service";
const RADICLE_SERVICE: &str = "outbe-radicle.service";
const FEEDER_SERVICE: &str = "outbe-feeder.service";
const RENEW_TIMER: &str = "outbe-tee-renew.timer";
const STATUS_POLL_TIMEOUT: Duration = Duration::from_secs(5);

struct StatusSnapshot<'a> {
    local: Option<&'a Block>,
    remote: Option<&'a Block>,
    execution_peers: Option<u64>,
    consensus: Option<&'a ConsensusStatus>,
    validator: Option<&'a ValidatorRecord>,
    local_epoch: Option<&'a EpochInfo>,
    remote_epoch: Option<&'a EpochInfo>,
    sync_eta_seconds: Option<u64>,
    dkg_eta_seconds: Option<u64>,
}

pub async fn show(watch: bool) -> Result<()> {
    process::require_root()?;
    let journal = Journal::load()?;
    let local = RpcClient::new(LOCAL_RPC)?;
    let remote = RpcClient::new(journal.config.network.certified_rpc.clone())?;
    let validator = validator_address(&Path::new(ROOT).join("keys/evm-key.hex"))?;
    if !watch {
        println!(
            "{}",
            status_line(&local, &remote, validator, None, None).await
        );
        return Ok(());
    }

    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    let display = WatchDisplay::new();
    let mut last_status = None;
    let mut sync_eta = CatchupEta::default();
    let mut dkg_eta = CatchupEta::default();
    loop {
        let (line, refresh_delay) = match tokio::time::timeout(
            STATUS_POLL_TIMEOUT,
            status_line(
                &local,
                &remote,
                validator,
                Some(&mut sync_eta),
                Some(&mut dkg_eta),
            ),
        )
        .await
        {
            Ok(line) => {
                last_status = Some(line.clone());
                (line, Duration::from_secs(5))
            }
            Err(_) => (
                last_status.as_ref().map_or_else(
                    || "Status      RPC busy; node still running".to_owned(),
                    |status| {
                        format!(
                            "{status}\nStatus      RPC busy; node still running (last successful poll)"
                        )
                    },
                ),
                Duration::ZERO,
            ),
        };
        display.render(&line);
        tokio::select! {
            result = &mut interrupt => {
                result.wrap_err("failed to listen for Ctrl-C")?;
                return Ok(());
            }
            () = tokio::time::sleep(refresh_delay) => {}
        }
    }
}

async fn status_line(
    local: &RpcClient,
    remote: &RpcClient,
    validator: Address,
    sync_eta: Option<&mut CatchupEta>,
    dkg_eta: Option<&mut CatchupEta>,
) -> String {
    let (remote_block, local_block, execution_peers, consensus, record, local_epoch, remote_epoch) = tokio::join!(
        remote.finalized_block(),
        local.finalized_block(),
        local.peer_count(),
        local.consensus_status(),
        remote.validator(validator),
        local.epoch_info(),
        remote.epoch_info(),
    );
    let remote_block = remote_block.ok();
    let local_block = local_block.ok();
    let consensus = consensus.ok();
    let sync_eta_seconds = sync_eta.and_then(|eta| {
        let remaining = remote_block
            .as_ref()?
            .number
            .saturating_sub(local_block.as_ref()?.number);
        eta.seconds_remaining(remaining)
    });
    let dkg_eta_seconds = dkg_eta.and_then(|eta| {
        let status = consensus.as_ref()?;
        if status.mode == "signer" || status.next_planned_activation_height == 0 {
            return None;
        }
        let finalized = remote_block
            .as_ref()
            .map_or(status.last_finalized_block, |block| block.number);
        eta.seconds_remaining(
            status
                .next_planned_activation_height
                .saturating_sub(finalized),
        )
    });
    format_status(StatusSnapshot {
        local: local_block.as_ref(),
        remote: remote_block.as_ref(),
        execution_peers: execution_peers.ok(),
        consensus: consensus.as_ref(),
        validator: record.as_ref().ok(),
        local_epoch: local_epoch.as_ref().ok(),
        remote_epoch: remote_epoch.as_ref().ok(),
        sync_eta_seconds,
        dkg_eta_seconds,
    })
}

fn format_status(snapshot: StatusSnapshot<'_>) -> String {
    let fullnode = process::systemctl_is_active(FULL_NODE);
    let validator_service = process::systemctl_is_active(VALIDATOR_SERVICE);
    let chain = chain_status(snapshot.local, snapshot.remote, snapshot.sync_eta_seconds);

    match (fullnode, validator_service) {
        (true, true) => format!(
            "Mode        conflict\nChain       {chain}\nServices    full-node active | validator active"
        ),
        (true, false) => {
            let peers = snapshot
                .execution_peers
                .map_or_else(|| "?".to_owned(), |value| value.to_string());
            let epoch = epoch_status(snapshot.local_epoch, snapshot.remote_epoch);
            format!(
                "Mode        full node\nChain       {chain}\nNetwork     execution peers {peers}\nEpochs      {epoch}\nServices    full-node active | validator inactive"
            )
        }
        (false, true) => {
            let lifecycle = snapshot
                .validator
                .map_or_else(|| "unknown".to_owned(), validator_lifecycle);
            let consensus = validator_consensus_status(
                snapshot.consensus,
                snapshot.remote,
                snapshot.dkg_eta_seconds,
            );
            let services = validator_services_status();
            let bls = snapshot
                .validator
                .map_or("?", |record| yes_no(record.has_bls_share));
            format!(
                "Mode        validator ({lifecycle})\nChain       {chain}\n{consensus}\nOn-chain    BLS share {bls}\nServices    {services}"
            )
        }
        (false, false) => {
            format!("Mode        stopped\nChain       {chain}\nServices    node services inactive")
        }
    }
}

fn chain_status(local: Option<&Block>, remote: Option<&Block>, eta_seconds: Option<u64>) -> String {
    let heights = format!(
        "local {} | remote {}",
        local.map_or_else(|| "?".into(), |block| block.number.to_string()),
        remote.map_or_else(|| "?".into(), |block| block.number.to_string())
    );
    let (Some(local), Some(remote)) = (local, remote) else {
        return heights;
    };
    let remaining = remote.number.saturating_sub(local.number);
    if remaining == 0 {
        format!("{heights} | synchronized")
    } else {
        format!(
            "{heights} | {remaining} blocks behind{}",
            eta_suffix(eta_seconds)
        )
    }
}

fn epoch_status(local: Option<&EpochInfo>, remote: Option<&EpochInfo>) -> String {
    match (local, remote) {
        (Some(local), Some(remote)) if local.epoch_number == remote.epoch_number => {
            format!("{} | reconstruction complete", local.epoch_number)
        }
        (Some(local), Some(remote)) => {
            let remaining = remote.epoch_number.saturating_sub(local.epoch_number);
            format!(
                "local {} | remote {} | reconstructing ({remaining} remaining)",
                local.epoch_number, remote.epoch_number
            )
        }
        (Some(local), None) => format!("local {} | remote unavailable", local.epoch_number),
        (None, Some(remote)) => format!("local unavailable | remote {}", remote.epoch_number),
        (None, None) => "unavailable".to_owned(),
    }
}

fn validator_consensus_status(
    status: Option<&ConsensusStatus>,
    remote: Option<&Block>,
    eta_seconds: Option<u64>,
) -> String {
    let Some(status) = status else {
        return "Consensus   unavailable".to_owned();
    };
    let mut value = format!(
        "Consensus   {} | peers {} | threshold shares {}",
        status.mode,
        status.connected_peers,
        yes_no(status.has_threshold_shares)
    );
    if status.mode != "signer" && status.next_planned_activation_height > 0 {
        let finalized = remote.map_or(status.last_finalized_block, |block| block.number);
        let remaining = status
            .next_planned_activation_height
            .saturating_sub(finalized);
        value.push_str(&format!(
            "\nDKG         activation {} | {remaining} blocks remaining{}",
            status.next_planned_activation_height,
            eta_suffix(eta_seconds)
        ));
    }
    value
}

fn validator_services_status() -> String {
    [
        ("enclave", ENCLAVE_SERVICE),
        ("radicle", RADICLE_SERVICE),
        ("feeder", FEEDER_SERVICE),
        ("renew", RENEW_TIMER),
    ]
    .into_iter()
    .map(|(name, unit)| {
        let state = if process::systemctl_is_active(unit) {
            "active"
        } else {
            "inactive"
        };
        format!("{name} {state}")
    })
    .collect::<Vec<_>>()
    .join(" | ")
}

fn validator_lifecycle(record: &ValidatorRecord) -> String {
    match record.status {
        1 => "pending".to_owned(),
        2 => "active".to_owned(),
        3 => "exiting".to_owned(),
        4 => "unbonding".to_owned(),
        5 => "inactive".to_owned(),
        6 => "jailed".to_owned(),
        status => format!("status-{status}"),
    }
}
