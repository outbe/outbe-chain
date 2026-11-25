use std::{path::Path, time::Duration};

use alloy_primitives::Address;
use eyre::{Context, Result};

use crate::{
    journal::Journal,
    process::{self, ROOT},
    progress::WatchDisplay,
    rpc::{Block, ConsensusStatus, RpcClient, ValidatorRecord},
    validator_address,
    wait::{eta_suffix, yes_no, CatchupEta},
};

const VALIDATOR_SERVICE: &str = "outbe-validator.service";
const RADICLE_SERVICE: &str = "outbe-radicle.service";
const FEEDER_SERVICE: &str = "outbe-feeder.service";
const HYPERLANE_SERVICE: &str = "outbe-hyperlane-validator.service";
const STATUS_POLL_TIMEOUT: Duration = Duration::from_secs(5);

struct StatusSnapshot<'a> {
    local: Option<&'a Block>,
    remote: Option<&'a Block>,
    consensus: Option<&'a ConsensusStatus>,
    validator: Option<&'a ValidatorRecord>,
    sync_eta_seconds: Option<u64>,
    dkg_eta_seconds: Option<u64>,
    feeder: Option<&'a serde_json::Value>,
    hyperlane: Option<&'a serde_json::Value>,
}

pub async fn show(watch: bool) -> Result<()> {
    process::require_root()?;
    let journal = Journal::load()?;
    let local = RpcClient::new(journal.config.network.local_rpc_url())?;
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
    let (remote_block, local_block, consensus, record, feeder, hyperlane) = tokio::join!(
        remote.finalized_block(),
        local.finalized_block(),
        local.consensus_status(),
        remote.validator(validator),
        sidecar_status("http://127.0.0.1:9002/status"),
        sidecar_status("http://127.0.0.1:9003/status"),
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
        consensus: consensus.as_ref(),
        validator: record.as_ref().ok(),
        sync_eta_seconds,
        dkg_eta_seconds,
        feeder: feeder.as_ref().ok(),
        hyperlane: hyperlane.as_ref().ok(),
    })
}

fn format_status(snapshot: StatusSnapshot<'_>) -> String {
    let validator_service = process::systemctl_is_active(VALIDATOR_SERVICE);
    let chain = chain_status(snapshot.local, snapshot.remote, snapshot.sync_eta_seconds);

    if !validator_service {
        return format!("Mode        stopped\nChain       {chain}\nServices    validator inactive");
    }
    let lifecycle = snapshot
        .validator
        .map_or_else(|| "unknown".to_owned(), validator_lifecycle);
    let consensus = validator_consensus_status(
        snapshot.consensus,
        snapshot.remote,
        snapshot.dkg_eta_seconds,
    );
    let services = validator_services_status();
    let feeder = health_status_line(snapshot.feeder);
    let hyperlane = hyperlane_status_line(snapshot.hyperlane);
    let bls = snapshot
        .validator
        .map_or("?", |record| yes_no(record.has_bls_share));
    format!(
        "Mode        validator ({lifecycle})\nChain       {chain}\n{consensus}\nOn-chain    BLS share {bls}\nFeeder      {feeder}\nHyperlane   {hyperlane}\nServices    {services}"
    )
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
    value.push_str(&format!(
        "\nTEE         {} | ready {} | resident key {}",
        status.enclave.state,
        yes_no(status.enclave.ready),
        yes_no(status.enclave.offer_key_ready)
    ));
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
        ("radicle", RADICLE_SERVICE),
        ("feeder", FEEDER_SERVICE),
        ("hyperlane", HYPERLANE_SERVICE),
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

async fn sidecar_status(url: &str) -> Result<serde_json::Value> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

fn health_status_line(status: Option<&serde_json::Value>) -> &'static str {
    match status.and_then(|value| value.get("healthy")?.as_bool()) {
        Some(true) => "healthy",
        Some(false) => "unhealthy",
        None => "unavailable",
    }
}

fn hyperlane_status_line(status: Option<&serde_json::Value>) -> String {
    let Some(status) = status else {
        return "unavailable".to_owned();
    };
    let healthy = status
        .get("healthy")
        .and_then(serde_json::Value::as_bool)
        .map_or(
            "unknown",
            |healthy| if healthy { "healthy" } else { "unhealthy" },
        );
    let controller = status
        .get("controller")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    format!("{healthy} | controller {controller}")
}

fn validator_lifecycle(record: &ValidatorRecord) -> String {
    if !record.registered {
        return "not promoted".to_owned();
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unregistered_resident_validator_is_not_promoted() {
        assert_eq!(
            validator_lifecycle(&ValidatorRecord::default()),
            "not promoted"
        );
    }
}
