//! Guest JSON-RPC batching, block-hash lookups, ValidatorSet queries, and certified_rpc checks.

use std::{collections::BTreeMap, net::Ipv4Addr, path::Path};

use eyre::{bail, ensure, Context, Result};
use outbe_operator_node_role::{progress::Progress, rpc::ConsensusStatus};
use outbe_operator_remote::{shell_quote, HostExec, Ssh};
use serde::Deserialize;
use serde_json::{json, Value};

use super::ensure_vm_deployment;
use crate::network::remote::NetworkArtifacts;

#[derive(Clone, Copy)]
pub(crate) struct VmRpcTarget<'a> {
    pub(crate) ssh: &'a Ssh,
    pub(crate) label: &'a str,
    pub(crate) service_ip: Ipv4Addr,
    pub(crate) rpc_port: u16,
}

#[derive(Default)]
pub(crate) struct BatchedRpcResult {
    pub(crate) chain_id: Option<u64>,
    pub(crate) finalized: Option<u64>,
    pub(crate) finalized_hash: Option<String>,
    pub(crate) execution_peers: Option<u64>,
    pub(crate) consensus: Option<ConsensusStatus>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct HyperlaneHealthStatus {
    pub(crate) healthy: bool,
    pub(crate) controller: String,
    pub(crate) active: Option<bool>,
}

/// Query the Hyperlane validator health endpoint over the VMM host bridge. Port 9003 is never
/// published externally; VM runtime config binds it only to the private service IP.
pub(crate) fn query_hyperlane_health(
    exec: &dyn HostExec,
    ssh: &Ssh,
    label: &str,
    service_ip: Ipv4Addr,
) -> Result<HyperlaneHealthStatus> {
    let url = format!("http://{service_ip}:9003/status");
    let cmd = format!(
        "curl -fsS --connect-timeout 2 --max-time 5 {}",
        shell_quote(&url)
    );
    let raw = exec
        .capture_raw(ssh, label, &cmd)
        .wrap_err_with(|| format!("query Hyperlane status from {url}"))?;
    serde_json::from_str(&raw).wrap_err_with(|| format!("parse Hyperlane status from {url}"))
}

pub(crate) fn ensure_vm_rpc_reachable_for_orchestration(build: &NetworkArtifacts) -> Result<()> {
    for founder in &build.config.founders {
        let resolved = build.config.resolve_founder_vm(founder)?;
        let rpc_bind_ip = build.config.founder_rpc_bind_ip(founder)?;
        ensure!(
            rpc_bind_ip == resolved.vm_ip,
            "refusing VM orchestration: founder `{}` sets rpc_bind_ip={rpc_bind_ip} != vm.vm_ip={}; \
             RPC-based readiness checks require rpc_bind_ip to match vm_ip (omit rpc_bind_ip or set it to {})",
            founder.name,
            resolved.vm_ip,
            resolved.vm_ip
        );
    }
    Ok(())
}

/// Extract the host component from an HTTP(S) URL and determine whether it is a private/loopback/
/// link-local address that must be queried via SSH from the first VMM host rather than locally.
pub(crate) fn is_private_rpc_url(url: &str) -> Result<bool> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| eyre::eyre!("certified_rpc must be an HTTP(S) URL: `{url}`"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, hp)| hp);
    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        bracketed
            .split(']')
            .next()
            .ok_or_else(|| eyre::eyre!("invalid IPv6 host in certified_rpc `{url}`"))?
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    ensure!(!host.is_empty(), "certified_rpc `{url}` has an empty host");
    if host.eq_ignore_ascii_case("localhost") {
        return Ok(true);
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        let is_private = match ip {
            std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback()
                    || (v6.segments()[0] & 0xfe00) == 0xfc00
                    || (v6.segments()[0] & 0xffc0) == 0xfe80
            }
        };
        return Ok(is_private);
    }
    Ok(false)
}

pub(super) async fn query_certified_rpc_chain_id_local(url: &str) -> Result<u64> {
    let client = outbe_operator_node_role::rpc::RpcClient::new(url)?;
    client.chain_id().await
}

fn query_certified_rpc_chain_id_via_host(exec: &dyn HostExec, ssh: &Ssh, url: &str) -> Result<u64> {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_chainId",
        "params": []
    });
    let payload = serde_json::to_vec(&req)?;
    let cmd = format!(
        "curl -fsS --connect-timeout 5 --max-time 15 -H 'content-type: application/json' --data-binary @- {}",
        shell_quote(url)
    );
    let raw = exec.capture_stdin(ssh, "certified-rpc", &cmd, &payload)?;
    let resp: Value = serde_json::from_str(&raw)
        .wrap_err_with(|| format!("parse eth_chainId response from {url}"))?;
    if let Some(err) = resp.get("error").filter(|e| !e.is_null()) {
        bail!("RPC error from {url}: {err}");
    }
    resp.get("result")
        .and_then(parse_hex_u64)
        .ok_or_else(|| eyre::eyre!("missing hex chain_id result from {url}"))
}

/// Verify `certified_rpc` answers `eth_chainId` with `build.config.chain_id` (directly from this
/// machine for public endpoints, or via SSH through `first_host` when `certified_rpc` uses a
/// private/loopback/link-local address).
pub(crate) async fn verify_certified_rpc_chain_id(
    build: &NetworkArtifacts,
    first_host: &Ssh,
    exec: &dyn HostExec,
) -> Result<()> {
    let url = &build.config.certified_rpc;
    let expected = build.config.chain_id;
    let use_first_host = is_private_rpc_url(url)?;
    let via = if use_first_host {
        format!("first host {}", first_host.host)
    } else {
        "local machine".to_owned()
    };
    let actual = if use_first_host {
        query_certified_rpc_chain_id_via_host(exec, first_host, url)
    } else {
        query_certified_rpc_chain_id_local(url).await
    }
    .wrap_err_with(|| {
        format!(
            "certified_rpc ({url}) did not answer eth_chainId (checked via {via}); \
             verify certified_rpc in network.yaml is reachable and serving chain_id {expected}"
        )
    })?;
    ensure!(
        actual == expected,
        "certified_rpc ({url}) returned chain_id {actual}, expected {expected} (checked via {via})"
    );
    Ok(())
}

/// Load VM network artifacts, ensure VM deployment & reachable RPC bindings, and verify
/// `certified_rpc` answers `eth_chainId == chain_id`.
pub(crate) async fn load_and_verify_vm_orchestration(
    artifacts: &Path,
    exec: &dyn HostExec,
    progress: &mut Progress,
) -> Result<NetworkArtifacts> {
    let build = NetworkArtifacts::load(artifacts)?;
    ensure_vm_deployment(&build)?;
    ensure_vm_rpc_reachable_for_orchestration(&build)?;
    let first_host = build
        .config
        .resolve_founder_vm(&build.config.founders[0])?
        .vmm_host;
    progress.stage(&format!(
        "verifying certified_rpc ({}) answers eth_chainId == {}",
        build.config.certified_rpc, build.config.chain_id
    ));
    verify_certified_rpc_chain_id(&build, &first_host, exec).await?;
    Ok(build)
}

/// Issue all 4 consensus/execution JSON-RPC queries in a single HTTP JSON-RPC batch request over
/// stdin to the VM's service IP (`http://<service_ip>:<rpc_port>`, never DHCP IP).
pub(crate) fn query_vm_rpc_batch(
    exec: &dyn HostExec,
    ssh: &Ssh,
    label: &str,
    service_ip: Ipv4Addr,
    rpc_port: u16,
) -> Result<BatchedRpcResult> {
    let url = format!("http://{service_ip}:{rpc_port}");
    let batch_req = json!([
        {"jsonrpc": "2.0", "id": 1, "method": "eth_chainId", "params": []},
        {"jsonrpc": "2.0", "id": 2, "method": "eth_getBlockByNumber", "params": ["finalized", false]},
        {"jsonrpc": "2.0", "id": 3, "method": "net_peerCount", "params": []},
        {"jsonrpc": "2.0", "id": 4, "method": "outbe_consensusStatus", "params": []},
    ]);
    let payload = serde_json::to_vec(&batch_req)?;
    let cmd = curl_json_rpc_cmd(&url);
    let raw = exec.capture_stdin(ssh, label, &cmd, &payload)?;
    let items: Vec<Value> = serde_json::from_str(&raw)
        .wrap_err_with(|| format!("parse batched JSON-RPC response from {url}"))?;

    let mut out = BatchedRpcResult::default();
    for item in items {
        apply_rpc_batch_item(&mut out, &item);
    }
    Ok(out)
}

fn apply_rpc_batch_item(out: &mut BatchedRpcResult, item: &Value) {
    let Some(id) = item.get("id").and_then(Value::as_u64) else {
        return;
    };
    if item.get("error").is_some_and(|e| !e.is_null()) {
        return;
    }
    let Some(result) = item.get("result").filter(|r| !r.is_null()) else {
        return;
    };
    match id {
        1 => out.chain_id = parse_hex_u64(result),
        2 => {
            out.finalized = result.get("number").and_then(parse_hex_u64);
            out.finalized_hash = result
                .get("hash")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
        }
        3 => out.execution_peers = parse_hex_u64(result),
        4 => out.consensus = serde_json::from_value(result.clone()).ok(),
        _ => {}
    }
}

/// Fetch the canonical block hash at `height` from a VM's service IP over SSH ControlMaster.
pub(crate) fn query_vm_block_hash(
    exec: &dyn HostExec,
    target: VmRpcTarget<'_>,
    height: u64,
) -> Result<String> {
    let url = format!("http://{}:{}", target.service_ip, target.rpc_port);
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_getBlockByNumber",
        "params": [format!("0x{height:x}"), false]
    });
    let payload = serde_json::to_vec(&req)?;
    let cmd = curl_json_rpc_cmd(&url);
    let raw = exec.capture_stdin(target.ssh, target.label, &cmd, &payload)?;
    let resp: Value = serde_json::from_str(&raw)
        .wrap_err_with(|| format!("parse block hash JSON-RPC response from {url}"))?;
    resp.pointer("/result/hash")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            eyre::eyre!(
                "VM {} ({}) returned no block hash for height {height}",
                target.label,
                target.service_ip
            )
        })
}

/// Query cumulative per-epoch `missedVotes` for every active validator via `eth_call` to the
/// `ValidatorSet` precompile (`0x...EE00`): `getActiveValidators()` followed by a single batched
/// `validatorByAddress(address)` `eth_call` to `service_ip`.
/// Fails closed on RPC error, empty validator set, or missing/malformed ABI return data.
pub(crate) fn query_validators_missed_votes(
    exec: &dyn HostExec,
    ssh: &Ssh,
    label: &str,
    service_ip: Ipv4Addr,
    rpc_port: u16,
) -> Result<BTreeMap<String, u64>> {
    use outbe_operator_node_role::rpc::{
        decode_get_active_validators, decode_validator_missed_votes, encode_get_active_validators,
        encode_validator_by_address, VALIDATOR_SET,
    };

    let url = format!("http://{service_ip}:{rpc_port}");
    let cmd = curl_json_rpc_cmd(&url);
    let precompile = format!("{VALIDATOR_SET:#x}");

    let val_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_call",
        "params": [{
            "to": precompile,
            "data": format!("0x{}", hex::encode(encode_get_active_validators())),
        }, "finalized"]
    });
    let raw_vals = exec
        .capture_stdin(ssh, label, &cmd, &serde_json::to_vec(&val_req)?)
        .wrap_err_with(|| {
            format!("query IValidatorSet.getActiveValidators from {label} ({service_ip})")
        })?;
    let val_resp: Value = serde_json::from_str(&raw_vals)
        .wrap_err_with(|| format!("parse IValidatorSet.getActiveValidators response from {url}"))?;
    let val_bytes = decode_rpc_hex_result(
        &val_resp,
        &format!("IValidatorSet.getActiveValidators on {label} ({service_ip})"),
    )?;
    let addresses = decode_get_active_validators(&val_bytes).wrap_err_with(|| {
        format!("ABI-decode IValidatorSet.getActiveValidators on {label} ({service_ip})")
    })?;
    ensure!(
        !addresses.is_empty(),
        "IValidatorSet.getActiveValidators returned empty validator list on {label} ({service_ip})"
    );

    let batch_req: Vec<Value> = addresses
        .iter()
        .enumerate()
        .map(|(idx, addr)| {
            json!({
                "jsonrpc": "2.0",
                "id": (idx + 1) as u64,
                "method": "eth_call",
                "params": [{
                    "to": precompile,
                    "data": format!("0x{}", hex::encode(encode_validator_by_address(*addr))),
                }, "finalized"]
            })
        })
        .collect();
    let raw_part = exec
        .capture_stdin(ssh, label, &cmd, &serde_json::to_vec(&batch_req)?)
        .wrap_err_with(|| {
            format!("query IValidatorSet.validatorByAddress batch from {label} ({service_ip})")
        })?;
    let items: Vec<Value> = serde_json::from_str(&raw_part).wrap_err_with(|| {
        format!("parse IValidatorSet.validatorByAddress batch response from {url}")
    })?;

    let mut missed_by_addr: BTreeMap<String, u64> = BTreeMap::new();
    for item in items {
        let id = item.get("id").and_then(Value::as_u64).ok_or_else(|| {
            eyre::eyre!(
                "IValidatorSet.validatorByAddress batch item missing numeric id on {label} ({service_ip})"
            )
        })?;
        let idx = usize::try_from(id.saturating_sub(1)).unwrap_or(usize::MAX);
        let addr = addresses.get(idx).ok_or_else(|| {
            eyre::eyre!(
                "IValidatorSet.validatorByAddress batch returned unexpected id {id} on {label} ({service_ip})"
            )
        })?;
        let addr_key = format!("{addr:#x}");
        let result_bytes = decode_rpc_hex_result(
            &item,
            &format!("IValidatorSet.validatorByAddress({addr_key}) on {label} ({service_ip})"),
        )?;
        let missed = decode_validator_missed_votes(&result_bytes).wrap_err_with(|| {
            format!(
                "ABI-decode IValidatorSet.validatorByAddress({addr_key}) on {label} ({service_ip})"
            )
        })?;
        missed_by_addr.insert(addr_key, missed);
    }

    ensure!(
        missed_by_addr.len() == addresses.len(),
        "IValidatorSet.validatorByAddress batch on {label} ({service_ip}) returned {} of {} validator(s)",
        missed_by_addr.len(),
        addresses.len()
    );
    Ok(missed_by_addr)
}

fn decode_rpc_hex_result(resp: &Value, context: &str) -> Result<Vec<u8>> {
    if let Some(err) = resp.get("error").filter(|e| !e.is_null()) {
        bail!("{context} returned RPC error: {err}");
    }
    let hex_str = resp
        .get("result")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre::eyre!("{context} returned no hex result"))?;
    hex::decode(hex_str.strip_prefix("0x").unwrap_or(hex_str))
        .wrap_err_with(|| format!("decode hex for {context}"))
}

fn curl_json_rpc_cmd(url: &str) -> String {
    format!(
        "curl -fsS --connect-timeout 5 --max-time 30 -H 'content-type: application/json' --data-binary @- {}",
        shell_quote(url)
    )
}

fn parse_hex_u64(value: &Value) -> Option<u64> {
    let raw = value.as_str()?.trim();
    let hex = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X"))?;
    u64::from_str_radix(hex, 16).ok()
}
