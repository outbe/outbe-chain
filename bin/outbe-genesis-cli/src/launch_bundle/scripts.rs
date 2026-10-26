//! Per-component launch scripts and feeder configuration templates.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use crate::error::GenesisError;
use crate::network::profiles_dir;
use crate::seed_codec::parse_u64;

use super::{DEFAULT_MAX_VALIDATORS, port_of, quote, uses_production_tee_session};

/// Render `run-enclave.sh` for a validator.
pub fn enclave_script(
    config: &Value,
    index: usize,
    base_dir: &str,
) -> Result<String, GenesisError> {
    let mode = config
        .get("tee")
        .and_then(|t| t.get("mode"))
        .and_then(Value::as_str)
        .ok_or_else(|| GenesisError::validation("tee.mode is required"))?;
    let default_enclave_dir = format!("{base_dir}/enclave");
    let enclave_dir = config
        .get("enclave_dir")
        .and_then(Value::as_str)
        .unwrap_or(&default_enclave_dir);
    let enclave_runner = config
        .get("enclave_runner")
        .and_then(Value::as_str)
        .unwrap_or("./run-enclave.sh");
    let chain_id = parse_u64(
        config
            .get("chain_id")
            .ok_or_else(|| GenesisError::validation("chain_id is required"))?,
    )?;
    let chain_id_hex = format!("0x{chain_id:064x}");
    let port = port_of(config, "tee_enclave_port")?;
    let endpoint = format!("127.0.0.1:{port}");
    let image = config
        .get("enclave_image")
        .and_then(Value::as_str)
        .unwrap_or("outbe-tee-enclave:latest");
    let validator_dir = format!("{base_dir}/validator-{index}");
    let validator_tee = format!("{validator_dir}/tee");
    let name = format!("outbe-enclave-{index}");

    if mode == "dcap-required" {
        return Ok(format!(
            "\n# TEE enclave under gramine-sgx, on the host. Requires the SGX driver\n\
# (/dev/sgx_enclave, /dev/sgx_provision) and a running aesmd.service.\n\
# The node refuses to start until this answers on {endpoint}.\n\
mkdir -p {q_val_tee}\n\
\n\
for device in /dev/sgx_enclave /dev/sgx_provision; do\n\
  [ -e \"$device\" ] || {{ echo \"missing $device: SGX driver not loaded\" >&2; exit 1; }}\n\
done\n\
systemctl is-active --quiet aesmd.service \\\n\
  || {{ echo \"aesmd.service is not running; DCAP quoting will fail\" >&2; exit 1; }}\n\
\n\
cd {q_enclave_dir}\n\
exec {q_runner} \\\n\
  --socket {q_endpoint} \\\n\
  --tee-dir {q_val_tee} \\\n\
  --chain-id {chain_id_hex}\n",
            q_val_tee = quote(&validator_tee),
            q_enclave_dir = quote(enclave_dir),
            q_runner = quote(enclave_runner),
            q_endpoint = quote(&endpoint),
        ));
    }

    let manifest = format!("{enclave_dir}/outbe-tee-enclave.manifest.sgx");
    let enclave_tee = format!("{enclave_dir}/tee");
    let signing_key = format!("{base_dir}/test-sgx-signing-key.pem");
    let dkg_seed = format!("{:064x}", index + 1);
    Ok(format!(
        "\n# TEE enclave, unattested development profile. The node refuses to start\n\
# until this answers on {endpoint}.\n\
mkdir -p {q_val_tee}\n\
\n\
if [ -e /dev/sgx_enclave ] && [ -f {q_manifest} ]; then\n\
  # Real SGX hardware with a signed manifest: run it natively.\n\
  #\n\
  # The sealing directory is the one baked into the signed manifest, not a\n\
  # per-validator path: Gramine only lets the enclave touch files the manifest\n\
  # declares, so passing anything else fails with a bare \"Permission denied\".\n\
  # One signed manifest per host therefore means one sealing directory per\n\
  # host, which is what a real deployment has anyway.\n\
  cd {q_enclave_dir}\n\
  # `sudo` only when not already root: under systemd the unit runs as root and\n\
  # sudo may not even be present in the service environment.\n\
  exec ${{SUDO:-$([ \"$(id -u)\" = 0 ] || echo sudo)}} gramine-sgx outbe-tee-enclave \\\n\
    --socket {q_endpoint} \\\n\
    --tee-dir {q_enclave_tee} \\\n\
    --chain-id {chain_id_hex}\n\
fi\n\
\n\
# No SGX on this host: fall back to the container, which runs the enclave\n\
# under gramine-direct (LibOS only, no hardware isolation).\n\
docker rm -f {q_name} >/dev/null 2>&1 || true\n\
exec docker run --rm --name {q_name} \\\n\
  --network host \\\n\
  --log-driver local --log-opt max-size=10m --log-opt max-file=3 \\\n\
  --security-opt seccomp=unconfined \\\n\
  -v {q_signing_key}:/run/secrets/outbe-test-sgx-key.pem:ro \\\n\
  -v {q_val_tee}:/tee \\\n\
  {q_image} \\\n\
  --socket {q_endpoint} \\\n\
  --dkg-seed {dkg_seed} \\\n\
  --tee-dir /tee \\\n\
  --chain-id {chain_id_hex}\n",
        q_val_tee = quote(&validator_tee),
        q_manifest = quote(&manifest),
        q_enclave_dir = quote(enclave_dir),
        q_endpoint = quote(&endpoint),
        q_enclave_tee = quote(&enclave_tee),
        q_name = quote(&name),
        q_signing_key = quote(&signing_key),
        q_image = quote(image),
    ))
}

/// Render `run-radicle.sh` for a validator.
pub fn radicle_script(
    config: &Value,
    index: usize,
    host: &str,
    keys_dir: &str,
) -> Result<String, GenesisError> {
    let port = port_of(config, "radicle_port")?;
    let status_port = port_of(config, "radicle_status_port")?;
    let home = format!("{keys_dir}/validator-{index}/radicle");
    let binary = config
        .get("radicle_binary")
        .and_then(Value::as_str)
        .unwrap_or("outbe-radicle");
    let max_validators = match config
        .get("validator_set")
        .and_then(|vs| vs.get("max_validators"))
    {
        Some(val) => parse_u64(val)?,
        None => DEFAULT_MAX_VALIDATORS,
    };
    let inbound_reserve = match config.get("radicle_external_inbound_reserve") {
        Some(val) => parse_u64(val)?,
        None => 16,
    };
    let socket = format!("{home}/node/outbe-control.sock");
    let advertise = format!("{host}:{port}");
    let q_home = quote(&home);
    let q_socket = quote(&socket);
    let q_binary = quote(binary);
    let q_advertise = quote(&advertise);

    Ok(format!(
        "\n# Validator-owned Radicle sidecar. The node refuses to start as a validator\n\
# without its control socket, and the status endpoint must stay loopback-only.\n\
#\n\
# `outbe-keygen validator` creates only keys/; the sidecar additionally\n\
# requires its working directories to exist, owner-only, before it will start.\n\
for directory in storage node cobs; do\n\
  path={q_home}/\"$directory\"\n\
  [ -d \"$path\" ] || mkdir -m 700 -- \"$path\"\n\
done\n\
\n\
# A crash or a hard restart leaves the control socket behind, and Heartwood\n\
# then refuses to start believing another node holds it. Clear it when nothing\n\
# is actually listening, so a systemd restart recovers on its own.\n\
socket={q_socket}\n\
if [ -S \"$socket\" ] && ! pgrep -f \"outbe-radicle .*{q_home}\" >/dev/null 2>&1; then\n\
  rm -f \"$socket\"\n\
fi\n\
\n\
exec {q_binary} \\\n\
  --home {q_home} \\\n\
  --control-socket {q_socket} \\\n\
  --listen 0.0.0.0:{port} \\\n\
  --status-listen 127.0.0.1:{status_port} \\\n\
  `# connection ceiling, not the current set size: the sidecar tracks the` \\\n\
  `# validator set from chain state, so a set that grows must not need a` \\\n\
  `# restart. Sized by the protocol maximum the ValidatorSet enforces.` \\\n\
  --max-validators {max_validators} \\\n\
  --external-inbound-reserve {inbound_reserve} \\\n\
  --advertise {q_advertise}\n"
    ))
}

/// Render `run-node.sh` for a validator.
pub fn node_script(
    config: &Value,
    index: usize,
    base_dir: &str,
    p2p_address: &str,
) -> Result<String, GenesisError> {
    let keys_dir = config
        .get("remote_keys_dir")
        .and_then(Value::as_str)
        .unwrap_or(base_dir);
    let (host, consensus_port_str) = p2p_address
        .rsplit_once(':')
        .ok_or_else(|| GenesisError::validation(format!("invalid p2p_address: {p2p_address}")))?;
    let consensus_port: u16 = consensus_port_str
        .parse()
        .map_err(|_| GenesisError::validation(format!("invalid port in {p2p_address}")))?;
    let binary = config
        .get("node_binary")
        .and_then(Value::as_str)
        .unwrap_or("outbe-chain");
    let session_mode = if uses_production_tee_session(config) {
        "  --tee-session-mode production-node-host \\\n"
    } else {
        ""
    };
    let validator_keys = format!("{keys_dir}/validator-{index}");
    let validator_dir = format!("{base_dir}/validator-{index}");
    let radicle_home = format!("{validator_keys}/radicle");

    let radicle_status_port = port_of(config, "radicle_status_port")?;
    let tee_enclave_port = port_of(config, "tee_enclave_port")?;
    let rpc_port = port_of(config, "rpc_port")?;
    let authrpc_port = port_of(config, "authrpc_port")?;
    let reth_p2p_port = port_of(config, "reth_p2p_port")?;
    let reth_discv5_port = port_of(config, "reth_discv5_port")?;
    let metrics_port = port_of(config, "metrics_port")?;

    Ok(format!(
        "\n# Validator node: execution and consensus with native enclave Lysis.\n\
KEYS={q_val_keys}\n\
DATA={q_data}\n\
mkdir -p \"$DATA\" {q_logs}\n\
\n\
# reth reads the p2p key from the file verbatim, so normalize it in place.\n\
printf '%s' \"$(tr -d '[:space:]' < \"$KEYS/reth-p2p-secret.hex\")\" > \"$KEYS/reth-p2p-secret.hex\"\n\
\n\
# A debug build needs a larger thread stack: block 1 lazily initializes k256's\n\
# secp256k1 tables in an unoptimized frame that overflows reth's default.\n\
export RUST_MIN_STACK=\"${{RUST_MIN_STACK:-16777216}}\"\n\
\n\
exec {q_binary} node \\\n\
  --validator \\\n\
{session_mode}\
  --chain {q_genesis} \\\n\
  --datadir \"$DATA\" \\\n\
  --engine.persistence-threshold 0 \\\n\
  --engine.memory-block-buffer-target 0 \\\n\
  --consensus.signing-key \"$KEYS/signing-key.hex\" \\\n\
  --validator.evm-key \"$KEYS/evm-key.hex\" \\\n\
  --consensus.listen-addr 0.0.0.0:{consensus_port} \\\n\
  --consensus.storage-dir {q_consensus} \\\n\
  --radicle.control-socket {q_radicle_sock} \\\n\
  --radicle.status-address 127.0.0.1:{radicle_status_port} \\\n\
  --tee-enclave-socket 127.0.0.1:{tee_enclave_port} \\\n\
  --http --http.addr 127.0.0.1 --http.port {rpc_port} \\\n\
  --http.api eth,net,web3,outbe \\\n\
  --authrpc.port {authrpc_port} \\\n\
  --port {reth_p2p_port} \\\n\
  --discovery.port {reth_p2p_port} \\\n\
  --nat extip:{host} \\\n\
  --discovery.v5.addr 0.0.0.0 \\\n\
  --discovery.v5.port {reth_discv5_port} \\\n\
  --p2p-secret-key \"$KEYS/reth-p2p-secret.hex\" \\\n\
  --bootnodes \"$(grep -v '^[[:space:]]*#' {q_bootnodes} | paste -sd, -)\" \\\n\
  --metrics 127.0.0.1:{metrics_port} \\\n\
  --ipcpath \"$DATA/reth.ipc\" \\\n\
  --log.file.directory {q_logs}\n",
        q_val_keys = quote(&validator_keys),
        q_data = quote(&format!("{validator_dir}/data")),
        q_logs = quote(&format!("{validator_dir}/logs")),
        q_binary = quote(binary),
        q_genesis = quote(&format!("{base_dir}/genesis.json")),
        q_consensus = quote(&format!("{validator_dir}/consensus")),
        q_radicle_sock = quote(&format!("{radicle_home}/node/outbe-control.sock")),
        q_bootnodes = quote(&format!("{base_dir}/reth-bootnodes.txt")),
    ))
}

/// Resolve the committed pairs file that `feeder_pairs` names, relative to `profiles/`.
pub fn feeder_pairs_file(config: &Value) -> Option<PathBuf> {
    let name = config.get("feeder_pairs").and_then(Value::as_str)?;
    if name.is_empty() {
        return None;
    }
    Some(profiles_dir().join(name))
}

/// Parse the committed pairs TOML file into a `serde_json::Value`.
pub fn feeder_pairs_table(config: &Value) -> Result<Option<Value>, GenesisError> {
    let Some(path) = feeder_pairs_file(config) else {
        return Ok(None);
    };
    let raw = fs::read_to_string(&path).map_err(|err| GenesisError::io(&path, err))?;
    let toml_val: toml::Value = toml::from_str(&raw)?;
    let json_val = serde_json::to_value(toml_val)?;
    Ok(Some(json_val))
}

/// Return the `pairs.toml` contents for every validator.
pub fn feeder_pairs(config: &Value) -> Result<String, GenesisError> {
    if let Some(path) = feeder_pairs_file(config) {
        return fs::read_to_string(&path).map_err(|err| GenesisError::io(&path, err));
    }
    let price_provider = config
        .get("price_provider")
        .and_then(Value::as_str)
        .unwrap_or("mock_http");
    let default_quote = if price_provider == "mock_http" {
        "840"
    } else {
        "USDT"
    };
    let source_quote = config
        .get("price_source_quote")
        .and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        })
        .unwrap_or_else(|| default_quote.to_owned());
    Ok(format!(
        "[[currency_pairs]]\n\
base = \"COEN\"\n\
quote = \"840\"\n\
markets = [{{ provider = \"{price_provider}\", quote = \"{source_quote}\" }}]\n\
\n\
[[deviation_thresholds]]\n\
base = \"COEN\"\n\
threshold = \"2.0\"\n"
    ))
}

/// Render `feeder.toml` for one validator.
pub fn feeder_config(
    config: &Value,
    index: usize,
    validator: &Value,
    signer_key: &str,
) -> Result<String, GenesisError> {
    let vote_period = match config
        .get("oracle")
        .and_then(|o| o.get("config"))
        .and_then(|c| c.get("vote_period"))
    {
        Some(val) => parse_u64(val)?,
        None => 60,
    };
    let price_provider = config
        .get("price_provider")
        .and_then(Value::as_str)
        .unwrap_or("mock_http");
    let mut api_provider = if price_provider == "mock_http" {
        let rest = config
            .get("price_feed_rest")
            .and_then(Value::as_str)
            .unwrap_or("https://prc.testnet.outbe.net");
        format!("[[api_providers]]\nname = \"mock_http\"\nrest = \"{rest}\"")
    } else {
        let websocket = config
            .get("price_feed_websocket")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let websocket_line = if websocket.is_empty() {
            String::new()
        } else {
            format!("\nwebsocket = \"{websocket}\"")
        };
        format!("[[api_providers]]\nname = \"{price_provider}\"{websocket_line}")
    };
    api_provider.push_str(&feeder_pairs_settings(config)?);

    let rpc_port = port_of(config, "rpc_port")?;
    let chain_id = parse_u64(
        config
            .get("chain_id")
            .ok_or_else(|| GenesisError::validation("chain_id is required"))?,
    )?;
    let validator_address = validator
        .get("address")
        .and_then(Value::as_str)
        .ok_or_else(|| GenesisError::validation("validator missing address"))?;
    let feeder_health_port = port_of(config, "feeder_health_port")?;

    Ok(format!(
        "# Price oracle feeder for validator-{index}.\n\
pairs = \"pairs.toml\"\n\
\n\
[chain]\n\
rpc_endpoint = \"http://127.0.0.1:{rpc_port}\"\n\
chain_id = {chain_id}\n\
gasless_oracle_votes = true\n\
\n\
[account]\n\
private_key = \"0x{signer_key}\"\n\
validator_address = \"{validator_address}\"\n\
\n\
[oracle]\n\
vote_period = {vote_period}\n\
poll_interval_secs = 2\n\
# Pairs come from pairs.toml, not from the node.\n\
sources = \"local\"\n\
\n\
[health]\n\
enabled = true\n\
bind_address = \"127.0.0.1:{feeder_health_port}\"\n\
\n\
# The feeder only accepts provider names from its built-in list (mock, pyth,\n\
# chainlink, binance, kraken, okx, gate, huobi, mexc, coinbase, mock_http); an\n\
# invented name is rejected at startup. `mock_http` uses the configured REST\n\
# endpoint; exchange providers use live WebSocket market streams.\n\
{api_provider}\n"
    ))
}

/// Placeholders for the RPC URLs and the Pyth key that the committed pairs file needs.
pub fn feeder_pairs_settings(config: &Value) -> Result<String, GenesisError> {
    let Some(table) = feeder_pairs_table(config)? else {
        return Ok(String::new());
    };
    let mut chain_ids = BTreeSet::new();
    for section in ["onchain_providers", "dex_providers"] {
        if let Some(entries) = table.get(section).and_then(Value::as_array) {
            for entry in entries {
                if let Some(cid) = entry.get("chain_id").and_then(Value::as_i64) {
                    chain_ids.insert(cid);
                }
            }
        }
    }
    let mut text = String::new();
    if !chain_ids.is_empty() {
        text.push_str(
            "\n\n# Each validator sets its own RPC URL for each network.\n[rpc_endpoints]",
        );
        for cid in chain_ids {
            text.push_str(&format!("\n{cid} = \"REPLACE_WITH_RPC_URL\""));
        }
    }
    let has_pyth = table
        .get("currency_pairs")
        .and_then(Value::as_array)
        .is_some_and(|pairs| {
            pairs.iter().any(|pair| {
                pair.get("markets")
                    .and_then(Value::as_array)
                    .is_some_and(|markets| {
                        markets
                            .iter()
                            .any(|m| m.get("provider").and_then(Value::as_str) == Some("pyth"))
                    })
            })
        });
    if has_pyth {
        text.push_str(
            "\n\n[[api_providers]]\nname = \"pyth\"\napi_key = \"REPLACE_WITH_PYTH_API_KEY\"",
        );
    }
    Ok(text)
}

/// Render `run-feeder.sh` for one validator.
pub fn feeder_script(config: &Value, index: usize, base_dir: &str) -> String {
    let binary = config
        .get("feeder_binary")
        .and_then(Value::as_str)
        .unwrap_or("outbe-feeder");
    let feeder_toml = format!("{base_dir}/validator-{index}/feeder.toml");
    format!(
        "\n# Price oracle feeder. Start it once the node serves RPC; oracle votes are\n\
# rejected before the chain produces blocks.\n\
exec {} --config {}\n",
        quote(binary),
        quote(&feeder_toml)
    )
}

/// Render `start-all.sh` for one validator.
pub fn start_all_script(
    config: &Value,
    index: usize,
    base_dir: &str,
) -> Result<String, GenesisError> {
    let directory = format!("{base_dir}/validator-{index}");
    let status_port = port_of(config, "radicle_status_port")?;
    let rpc_port = port_of(config, "rpc_port")?;
    Ok(format!(
        "\n# Bring up the configured storage, enclave, Radicle and node in dependency order.\n\
cd {}\n\
mkdir -p logs\n\
\n\
\n\
# run-enclave.sh execs into the enclave, so it must be backgrounded: calling it\n\
# directly would replace this script and nothing below would ever run.\n\
nohup ./run-enclave.sh > logs/enclave.log 2>&1 &\n\
echo $! > enclave.pid\n\
echo \"waiting for the enclave...\"\n\
for _ in $(seq 1 60); do\n\
  if (exec 3<>/dev/tcp/127.0.0.1/{{TEE_PORT}}) 2>/dev/null; then exec 3>&-; break; fi\n\
  sleep 1\n\
done\n\
\n\
nohup ./run-radicle.sh > logs/radicle.log 2>&1 &\n\
echo $! > radicle.pid\n\
echo \"waiting for the Radicle sidecar...\"\n\
for _ in $(seq 1 60); do\n\
  if (exec 3<>/dev/tcp/127.0.0.1/{status_port}) 2>/dev/null; then exec 3>&-; break; fi\n\
  sleep 1\n\
done\n\
\n\
nohup ./run-node.sh > logs/node.log 2>&1 &\n\
echo $! > node.pid\n\
echo \"waiting for RPC...\"\n\
for _ in $(seq 1 180); do\n\
  if (exec 3<>/dev/tcp/127.0.0.1/{rpc_port}) 2>/dev/null; then exec 3>&-; break; fi\n\
  sleep 1\n\
done\n\
\n\
nohup ./run-feeder.sh > logs/feeder.log 2>&1 &\n\
echo $! > feeder.pid\n\
\n\
echo\n\
echo \"validator-{index} started:\"\n\
echo \"  node:    logs/node.log    (pid $(cat node.pid))\"\n\
echo \"  radicle: logs/radicle.log (pid $(cat radicle.pid))\"\n\
echo \"  feeder:  logs/feeder.log  (pid $(cat feeder.pid))\"\n\
echo \"  enclave: docker logs outbe-enclave-{index}\"\n",
        quote(&directory)
    ))
}

/// Render `stop-all.sh` for one validator.
pub fn stop_all_script(index: usize, base_dir: &str) -> String {
    let directory = format!("{base_dir}/validator-{index}");
    format!(
        "\ncd {}\n\
for name in feeder node radicle enclave; do\n\
  if [ -f \"$name.pid\" ]; then\n\
    pid=\"$(cat \"$name.pid\")\"\n\
    if kill -0 \"$pid\" 2>/dev/null; then\n\
      kill \"$pid\"\n\
      echo \"stopped $name (pid $pid)\"\n\
    fi\n\
    rm -f \"$name.pid\"\n\
  fi\n\
done\n\
docker stop outbe-enclave-{index} >/dev/null 2>&1 && echo \"stopped enclave\" || true\n",
        quote(&directory)
    )
}
