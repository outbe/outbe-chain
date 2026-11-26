use std::{path::Path, sync::Arc, time::Duration};

use eyre::{ensure, Context, Result};
use outbe_operator_node_role::{
    progress::{Progress, WatchDisplay},
    rpc::ConsensusStatus,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::task::JoinSet;

use super::{
    build::DeploymentKind,
    remote::{capture_founder, HostExec, NetworkArtifacts, SshOptions},
};

#[derive(Clone, Debug, Serialize)]
pub(super) struct FounderStatus {
    pub(super) name: String,
    pub(super) target: String,
    pub(super) validator: String,
    pub(super) radicle: String,
    pub(super) feeder: String,
    pub(super) feeder_healthy: Option<bool>,
    pub(super) hyperlane: String,
    pub(super) hyperlane_healthy: Option<bool>,
    pub(super) hyperlane_controller: Option<String>,
    pub(super) chain_id: Option<u64>,
    pub(super) finalized: Option<u64>,
    pub(super) execution_peers: Option<u64>,
    #[serde(skip_serializing)]
    pub(super) consensus: Option<ConsensusStatus>,
    pub(super) error: Option<String>,
}

impl FounderStatus {
    pub(super) fn services(&self) -> [&str; 4] {
        [&self.target, &self.validator, &self.radicle, &self.feeder]
    }
}

#[derive(Deserialize)]
struct RpcResponse {
    result: Option<Value>,
}

#[derive(Serialize)]
struct HostStatusJson<'a> {
    network: &'a str,
    chain_id: u64,
    founders: &'a [FounderStatus],
}

pub async fn show(
    artifacts: &Path,
    ssh_key: Option<&Path>,
    ssh_config: Option<&Path>,
    watch: bool,
    json: bool,
) -> Result<()> {
    let build = NetworkArtifacts::load(artifacts)?;
    let ssh: Arc<dyn HostExec> = Arc::new(
        SshOptions::new(ssh_key, ssh_config)?
            .with_network_known_hosts(&build.manifest.genesis.final_sha256)?,
    );
    show_with_exec(artifacts, ssh, watch, json).await
}

pub(crate) async fn show_with_exec(
    artifacts: &Path,
    exec: Arc<dyn HostExec>,
    watch: bool,
    json: bool,
) -> Result<()> {
    let build = NetworkArtifacts::load(artifacts)?;
    ensure!(
        build.manifest.deployment == DeploymentKind::Host,
        "VM `outbe-net-up network status` orchestration is scheduled for S6.3"
    );
    if json {
        let statuses = query_all(&build, &exec).await?;
        let payload = HostStatusJson {
            network: &build.config.name,
            chain_id: build.config.chain_id,
            founders: &statuses,
        };
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    let mut progress = Progress::new();
    progress.stage("querying founding validator status");
    let statuses = query_all(&build, &exec).await?;
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
        display.render(&render(&build, &query_all(&build, &exec).await?));
    }
}

pub(super) async fn query_all(
    build: &NetworkArtifacts,
    exec: &Arc<dyn HostExec>,
) -> Result<Vec<FounderStatus>> {
    let mut tasks = JoinSet::new();
    for founder in build.config.founders.clone() {
        let exec = Arc::clone(exec);
        tasks.spawn_blocking(move || query_one(exec.as_ref(), &founder));
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

pub(crate) struct RpcReadinessSample<'a> {
    pub chain_id: Option<u64>,
    pub finalized: Option<u64>,
    pub execution_peers: Option<u64>,
    pub consensus: Option<&'a ConsensusStatus>,
}

pub(crate) fn rpc_ready_for_hyperlane(
    sample: &RpcReadinessSample<'_>,
    expected_chain_id: u64,
    peers: usize,
) -> bool {
    let chain_matches = sample.chain_id == Some(expected_chain_id);
    let finality_ready = sample.finalized.is_some_and(|height| height > 0);
    let execution_ready = sample
        .execution_peers
        .is_some_and(|count| count >= peers as u64);
    let consensus_ready = sample.consensus.is_some_and(|value| {
        value.mode == "signer"
            && value.resident_ready()
            && value.has_threshold_shares
            && value.connected_peers >= peers as u64
    });
    chain_matches && finality_ready && execution_ready && consensus_ready
}

pub(super) fn ready_for_hyperlane_setup(
    status: &FounderStatus,
    chain_id: u64,
    peers: usize,
) -> bool {
    let services_ready = status.services().into_iter().all(|state| state == "active");
    let feeder_ready = status.feeder_healthy == Some(true);
    let sample = RpcReadinessSample {
        chain_id: status.chain_id,
        finalized: status.finalized,
        execution_peers: status.execution_peers,
        consensus: status.consensus.as_ref(),
    };
    status.error.is_none()
        && services_ready
        && feeder_ready
        && rpc_ready_for_hyperlane(&sample, chain_id, peers)
}

pub(super) fn render(build: &NetworkArtifacts, statuses: &[FounderStatus]) -> String {
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
                    "{} peers={} shares={} tee={}",
                    consensus.mode,
                    consensus.connected_peers,
                    if consensus.has_threshold_shares {
                        "yes"
                    } else {
                        "no"
                    },
                    if consensus.resident_ready() {
                        "ready"
                    } else {
                        "not-ready"
                    },
                )
            },
        );
        output.push_str(&format!(
            "{:<20} {:<29} {:<22} {:<10} {:<5} {}\n",
            status.name,
            format!(
                "t:{} v:{} r:{} f:{} h:{}",
                short(&status.target),
                short(&status.validator),
                short(&status.radicle),
                short_health(&status.feeder, status.feeder_healthy),
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

fn query_one(exec: &dyn HostExec, founder: &super::config::Founder) -> FounderStatus {
    match query_one_inner(exec, founder) {
        Ok(status) => status,
        Err(error) => FounderStatus {
            name: founder.name.clone(),
            target: "unreachable".into(),
            validator: "unknown".into(),
            radicle: "unknown".into(),
            feeder: "unknown".into(),
            feeder_healthy: None,
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

fn query_one_inner(exec: &dyn HostExec, founder: &super::config::Founder) -> Result<FounderStatus> {
    let rpc_bind_ip = founder.rpc_bind_ip.unwrap_or(std::net::Ipv4Addr::LOCALHOST);
    let url = format!("http://{rpc_bind_ip}:{}", founder.rpc_port());
    let request = |method: &str, params: &str, id: u8| {
        format!(
            "curl -fsS --max-time 4 -H 'content-type: application/json' --data '{{\"jsonrpc\":\"2.0\",\"method\":\"{method}\",\"params\":{params},\"id\":{id}}}' {url} || true; printf '\\n'"
        )
    };
    let command = format!(
        "printf '%s\\n' \"$(systemctl is-active outbe-validator.target 2>/dev/null || true)\" \"$(systemctl is-active outbe-validator.service 2>/dev/null || true)\" \"$(systemctl is-active outbe-radicle.service 2>/dev/null || true)\" \"$(systemctl is-active outbe-feeder.service 2>/dev/null || true)\" \"$(systemctl is-active outbe-hyperlane-validator.service 2>/dev/null || true)\"; (curl -fsS --max-time 3 http://127.0.0.1:9002/status || true) | tr -d '\\n'; printf '\\n'; (curl -fsS --max-time 3 http://127.0.0.1:9003/status || true) | tr -d '\\n'; printf '\\n'; {}; {}; {}; {}",
        request("eth_chainId", "[]", 1),
        request("eth_getBlockByNumber", "[\"finalized\",false]", 2),
        request("net_peerCount", "[]", 3),
        request("outbe_consensusStatus", "[]", 4),
    );
    let output = capture_founder(exec, founder, &command)?;
    Ok(parse_status(&founder.name, &output))
}

fn parse_status(name: &str, output: &str) -> FounderStatus {
    let mut lines = output.lines();
    let target = lines.next().unwrap_or("unknown").to_owned();
    let validator = lines.next().unwrap_or("unknown").to_owned();
    let radicle = lines.next().unwrap_or("unknown").to_owned();
    let feeder = lines.next().unwrap_or("unknown").to_owned();
    let hyperlane = lines.next().unwrap_or("unknown").to_owned();
    let feeder_healthy = lines
        .next()
        .and_then(|line| serde_json::from_str::<Value>(line).ok())
        .and_then(|value| value.get("healthy").and_then(Value::as_bool));
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
    FounderStatus {
        name: name.to_owned(),
        target,
        validator,
        radicle,
        feeder,
        feeder_healthy,
        hyperlane,
        hyperlane_healthy,
        hyperlane_controller,
        chain_id,
        finalized,
        execution_peers,
        consensus,
        error: None,
    }
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

fn short_health(state: &str, healthy: Option<bool>) -> &str {
    if state != "active" {
        return short(state);
    }
    match healthy {
        Some(true) => "ok",
        Some(false) => "bad",
        None => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_requires_live_services_finality_consensus_and_resident_tee() {
        let output = r#"active
active
active
active
active
{"healthy":true}
{"healthy":true,"controller":"waiting-for-controller"}
{"result":"0x2a"}
{"result":{"number":"0x1"}}
{"result":"0x3"}
{"result":{"mode":"signer","connectedPeers":3,"hasThresholdShares":true,"isValidator":true,"enclave":{"state":"ready","ready":true,"offerKeyReady":true}}}
"#;
        let mut status = parse_status("validator-0", output);
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
        status.feeder_healthy = Some(false);
        assert!(!healthy(&status, 42, 3));
        status.feeder_healthy = Some(true);
        let consensus = status.consensus.as_mut().unwrap();
        consensus.enclave.offer_key_ready = false;
        assert!(!healthy(&status, 42, 3));
        let consensus = status.consensus.as_mut().unwrap();
        consensus.enclave.offer_key_ready = true;
        consensus.mode = "verifier".into();
        assert!(!healthy(&status, 42, 3));
    }
}
