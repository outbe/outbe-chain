use std::{path::Path, time::Duration};

use eyre::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use tokio::task::JoinSet;

use crate::{
    support::progress::{Progress, WatchDisplay},
    validator::rpc::ConsensusStatus,
};

use super::remote::{NetworkBuild, SshOptions};

#[derive(Clone, Debug)]
pub(super) struct FounderStatus {
    pub(super) name: String,
    pub(super) target: String,
    pub(super) validator: String,
    pub(super) enclave: String,
    pub(super) radicle: String,
    pub(super) feeder: String,
    pub(super) hyperlane: String,
    pub(super) hyperlane_healthy: Option<bool>,
    pub(super) hyperlane_controller: Option<String>,
    pub(super) chain_id: Option<u64>,
    pub(super) finalized: Option<u64>,
    pub(super) execution_peers: Option<u64>,
    pub(super) consensus: Option<ConsensusStatus>,
    pub(super) error: Option<String>,
}

#[derive(Deserialize)]
struct RpcResponse {
    result: Option<Value>,
}

pub async fn show(artifacts: &Path, ssh_key: Option<&Path>, watch: bool) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("querying founding validator status");
    let build = NetworkBuild::load(artifacts)?;
    let ssh = SshOptions::new(ssh_key)?;
    let statuses = query_all(&build, &ssh).await?;
    if !watch {
        progress.done(&format!("queried {} founding validators", statuses.len()));
        println!("\n{}", render(&build, &statuses));
        return Ok(());
    }

    progress.done(&format!(
        "connected to {} founding validators",
        statuses.len()
    ));
    let display = WatchDisplay::new();
    display.render(&render(&build, &statuses));
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    loop {
        tokio::select! {
            result = &mut interrupt => {
                result.wrap_err("failed to listen for Ctrl-C")?;
                return Ok(());
            }
            () = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
        display.render(&render(&build, &query_all(&build, &ssh).await?));
    }
}

pub(super) async fn query_all(
    build: &NetworkBuild,
    ssh: &SshOptions,
) -> Result<Vec<FounderStatus>> {
    let mut tasks = JoinSet::new();
    for founder in build.config.founders.clone() {
        let ssh = ssh.clone();
        tasks.spawn_blocking(move || query_one(&ssh, &founder));
    }
    let mut statuses = Vec::with_capacity(build.config.founders.len());
    while let Some(result) = tasks.join_next().await {
        statuses.push(result.wrap_err("founder status task panicked")?);
    }
    statuses.sort_by_key(|status| {
        build
            .config
            .founders
            .iter()
            .position(|founder| founder.name == status.name)
            .unwrap_or(usize::MAX)
    });
    Ok(statuses)
}

pub(super) fn healthy(status: &FounderStatus, chain_id: u64, peers: usize) -> bool {
    ready_for_hyperlane_setup(status, chain_id, peers)
        && status.hyperlane == "active"
        && status.hyperlane_healthy == Some(true)
}

pub(super) fn ready_for_hyperlane_setup(
    status: &FounderStatus,
    chain_id: u64,
    peers: usize,
) -> bool {
    let services_ready = [
        status.target.as_str(),
        status.validator.as_str(),
        status.enclave.as_str(),
        status.radicle.as_str(),
        status.feeder.as_str(),
    ]
    .into_iter()
    .all(|state| state == "active");
    let chain_matches = status.chain_id == Some(chain_id);
    let finality_ready = status.finalized.is_some_and(|height| height > 0);
    let execution_ready = status
        .execution_peers
        .is_some_and(|count| count >= peers as u64);
    let consensus_ready = status.consensus.as_ref().is_some_and(|value| {
        value.mode == "signer"
            && value.has_threshold_shares
            && value.connected_peers >= peers as u64
    });
    [
        status.error.is_none(),
        services_ready,
        chain_matches,
        finality_ready,
        execution_ready,
        consensus_ready,
    ]
    .into_iter()
    .all(|ready| ready)
}

pub(super) fn render(build: &NetworkBuild, statuses: &[FounderStatus]) -> String {
    let required_peers = build.config.founders.len().saturating_sub(1);
    let healthy_count = statuses
        .iter()
        .filter(|status| healthy(status, build.config.chain_id, required_peers))
        .count();
    let mut output = format!(
        "Network     {} | chain {} | healthy {healthy_count}/{}\n\n",
        build.config.name,
        build.config.chain_id,
        statuses.len()
    );
    output.push_str("Founder              Services                      Hyperlane              Finalized  Exec  Consensus\n");
    for status in statuses {
        let finalized = value(status.finalized);
        let execution = value(status.execution_peers);
        let consensus = status.consensus.as_ref().map_or_else(
            || "unavailable".to_owned(),
            |consensus| {
                format!(
                    "{} peers={} shares={}",
                    consensus.mode,
                    consensus.connected_peers,
                    if consensus.has_threshold_shares {
                        "yes"
                    } else {
                        "no"
                    }
                )
            },
        );
        output.push_str(&format!(
            "{:<20} {:<29} {:<22} {:<10} {:<5} {}\n",
            status.name,
            format!(
                "t:{} v:{} e:{} r:{} f:{} h:{}",
                short(&status.target),
                short(&status.validator),
                short(&status.enclave),
                short(&status.radicle),
                short(&status.feeder),
                short(&status.hyperlane)
            ),
            format!(
                "{} controller={}",
                status
                    .hyperlane_healthy
                    .map_or("?", |healthy| if healthy { "ok" } else { "bad" }),
                status.hyperlane_controller.as_deref().unwrap_or("unknown")
            ),
            finalized,
            execution,
            consensus
        ));
        if let Some(error) = &status.error {
            output.push_str(&format!("  error: {error}\n"));
        }
    }
    output.trim_end().to_owned()
}

fn query_one(ssh: &SshOptions, founder: &super::config::Founder) -> FounderStatus {
    match query_one_inner(ssh, founder) {
        Ok(status) => status,
        Err(error) => FounderStatus {
            name: founder.name.clone(),
            target: "unreachable".into(),
            validator: "unknown".into(),
            enclave: "unknown".into(),
            radicle: "unknown".into(),
            feeder: "unknown".into(),
            hyperlane: "unknown".into(),
            hyperlane_healthy: None,
            hyperlane_controller: None,
            chain_id: None,
            finalized: None,
            execution_peers: None,
            consensus: None,
            error: Some(format!("{error:#}")),
        },
    }
}

fn query_one_inner(ssh: &SshOptions, founder: &super::config::Founder) -> Result<FounderStatus> {
    let url = format!("http://{}:8545", founder.rpc_bind_ip);
    let request = |method: &str, params: &str, id: u8| {
        format!(
            "curl -fsS --max-time 4 -H 'content-type: application/json' --data '{{\"jsonrpc\":\"2.0\",\"method\":\"{method}\",\"params\":{params},\"id\":{id}}}' {url} || true; printf '\\n'"
        )
    };
    let command = format!(
        "printf '%s\\n' \"$(systemctl is-active outbe-validator.target 2>/dev/null || true)\" \"$(systemctl is-active outbe-validator.service 2>/dev/null || true)\" \"$(systemctl is-active outbe-enclave.service 2>/dev/null || true)\" \"$(systemctl is-active outbe-radicle.service 2>/dev/null || true)\" \"$(systemctl is-active outbe-feeder.service 2>/dev/null || true)\" \"$(systemctl is-active outbe-hyperlane-validator.service 2>/dev/null || true)\"; (curl -fsS --max-time 3 http://127.0.0.1:9003/status || true) | tr -d '\\n'; printf '\\n'; {}; {}; {}; {}",
        request("eth_chainId", "[]", 1),
        request("eth_getBlockByNumber", "[\"finalized\",false]", 2),
        request("net_peerCount", "[]", 3),
        request("outbe_consensusStatus", "[]", 4),
    );
    let output = ssh.capture(founder, &command)?;
    let mut lines = output.lines();
    let target = lines.next().unwrap_or("unknown").to_owned();
    let validator = lines.next().unwrap_or("unknown").to_owned();
    let enclave = lines.next().unwrap_or("unknown").to_owned();
    let radicle = lines.next().unwrap_or("unknown").to_owned();
    let feeder = lines.next().unwrap_or("unknown").to_owned();
    let hyperlane = lines.next().unwrap_or("unknown").to_owned();
    let hyperlane_status = lines
        .next()
        .and_then(|line| serde_json::from_str::<Value>(line).ok());
    let hyperlane_healthy = hyperlane_status
        .as_ref()
        .and_then(|value| value.get("healthy"))
        .and_then(Value::as_bool);
    let hyperlane_controller = hyperlane_status
        .as_ref()
        .and_then(|value| value.get("controller"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let chain_id = rpc_result(lines.next()).and_then(|value| parse_hex(value.as_str()?));
    let finalized =
        rpc_result(lines.next()).and_then(|value| parse_hex(value.get("number")?.as_str()?));
    let execution_peers = rpc_result(lines.next()).and_then(|value| parse_hex(value.as_str()?));
    let consensus = rpc_result(lines.next()).and_then(|value| serde_json::from_value(value).ok());
    Ok(FounderStatus {
        name: founder.name.clone(),
        target,
        validator,
        enclave,
        radicle,
        feeder,
        hyperlane,
        hyperlane_healthy,
        hyperlane_controller,
        chain_id,
        finalized,
        execution_peers,
        consensus,
        error: None,
    })
}

fn rpc_result(line: Option<&str>) -> Option<Value> {
    let response: RpcResponse = serde_json::from_str(line?).ok()?;
    response.result
}

fn parse_hex(value: &str) -> Option<u64> {
    u64::from_str_radix(value.strip_prefix("0x").unwrap_or(value), 16).ok()
}

fn value(value: Option<u64>) -> String {
    value.map_or_else(|| "?".to_owned(), |value| value.to_string())
}

fn short(state: &str) -> &str {
    match state {
        "active" => "ok",
        "inactive" => "off",
        "activating" => "wait",
        "failed" => "fail",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_requires_finality_and_consensus() {
        let mut status = FounderStatus {
            name: "validator-0".into(),
            target: "active".into(),
            validator: "active".into(),
            enclave: "active".into(),
            radicle: "active".into(),
            feeder: "active".into(),
            hyperlane: "active".into(),
            hyperlane_healthy: Some(true),
            hyperlane_controller: Some("waiting-for-controller".into()),
            chain_id: Some(42),
            finalized: Some(1),
            execution_peers: Some(3),
            consensus: Some(ConsensusStatus {
                connected_peers: 3,
                has_threshold_shares: true,
                mode: "signer".into(),
                ..Default::default()
            }),
            error: None,
        };
        assert!(healthy(&status, 42, 3));
        status.hyperlane = "activating".into();
        status.hyperlane_healthy = None;
        assert!(ready_for_hyperlane_setup(&status, 42, 3));
        assert!(!healthy(&status, 42, 3));
        status.hyperlane = "active".into();
        status.hyperlane_healthy = Some(true);
        status.finalized = Some(0);
        assert!(!healthy(&status, 42, 3));
        status.finalized = Some(1);
        status.feeder = "activating".into();
        assert!(!healthy(&status, 42, 3));
    }
}
