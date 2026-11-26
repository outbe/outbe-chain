//! Per-component launch scripts and feeder configuration templates.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use crate::error::GenesisError;
use crate::network::{profiles_dir, rpc_bind_ip};
use crate::seed_codec::parse_u64;

use super::{port_of, quote, DEFAULT_MAX_VALIDATORS};

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
    let local_consensus_defaults = if config
        .get("consensus_use_local_defaults")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        "  --consensus.use-local-defaults \\\n"
    } else {
        ""
    };
    let validator_keys = format!("{keys_dir}/validator-{index}");
    let validator_dir = format!("{base_dir}/validator-{index}");
    let radicle_home = format!("{validator_keys}/radicle");

    let radicle_status_port = port_of(config, "radicle_status_port")?;
    let rpc_port = port_of(config, "rpc_port")?;
    let authrpc_port = port_of(config, "authrpc_port")?;
    let reth_p2p_port = port_of(config, "reth_p2p_port")?;
    let reth_discv5_port = port_of(config, "reth_discv5_port")?;
    let metrics_port = port_of(config, "metrics_port")?;
    let rpc_bind = rpc_bind_ip(config, host)?;

    Ok(format!(
        "\n# Validator node: execution, consensus, and resident TEE.\n\
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
  --chain {q_genesis} \\\n\
  --datadir \"$DATA\" \\\n\
  --engine.persistence-threshold 0 \\\n\
  --engine.memory-block-buffer-target 0 \\\n\
  --consensus.signing-key \"$KEYS/signing-key.hex\" \\\n\
  --validator.evm-key \"$KEYS/evm-key.hex\" \\\n\
  --consensus.listen-addr 0.0.0.0:{consensus_port} \\\n\
  --tee-bootstrap-timeout-secs 300 \\\n\
{local_consensus_defaults}\
  --consensus.storage-dir {q_consensus} \\\n\
  --radicle.control-socket {q_radicle_sock} \\\n\
  --radicle.status-address 127.0.0.1:{radicle_status_port} \\\n\
  --http --http.addr {rpc_bind} --http.port {rpc_port} \\\n\
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
        q_bootnodes = quote(&format!("{validator_dir}/reth-bootnodes.txt")),
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
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
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
    let mut api_provider = if price_provider == "mock" {
        String::new()
    } else if price_provider == "mock_http" {
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
    let p2p_address = validator
        .get("p2p_address")
        .and_then(Value::as_str)
        .unwrap_or("127.0.0.1");
    let validator_host = p2p_address
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(p2p_address);
    let rpc_bind = rpc_bind_ip(config, validator_host)?;

    Ok(format!(
        "# Price oracle feeder for validator-{index}.\n\
pairs = \"pairs.toml\"\n\
\n\
[chain]\n\
rpc_endpoint = \"http://{rpc_bind}:{rpc_port}\"\n\
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
# invented name is rejected at startup. `mock` is self-contained, `mock_http`\n\
# uses the configured REST endpoint and exchange providers use live streams.\n\
{api_provider}\n"
    ))
}

/// Placeholders for the RPC URLs and the Pyth key that the committed pairs file needs.
pub fn feeder_pairs_settings(config: &Value) -> Result<String, GenesisError> {
    let Some(table) = feeder_pairs_table(config)? else {
        return Ok(String::new());
    };
    let mut chain_ids = BTreeSet::new();
    for section in ["onchain_providers", "dex_providers", "gas_providers"] {
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

/// Render the non-secret Hyperlane validator configuration for one host.
pub fn hyperlane_public_config(config: &Value, index: usize) -> Result<String, GenesisError> {
    let public_url = config
        .get("hyperlane_public_urls")
        .and_then(Value::as_array)
        .and_then(|urls| urls.get(index))
        .and_then(Value::as_str)
        .ok_or_else(|| GenesisError::validation("missing Hyperlane public URL"))?;
    let origins = config
        .get("hyperlane_origins")
        .and_then(Value::as_array)
        .ok_or_else(|| GenesisError::validation("hyperlane_origins must be a list"))?;
    let value = serde_json::json!({
        "hyperlane": {
            "poll_interval_secs": config
                .get("hyperlane_poll_interval_secs")
                .and_then(Value::as_u64)
                .unwrap_or(30),
            "gasless": config
                .get("hyperlane_gasless")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        "health": {
            "enabled": true,
            "bind_address": "127.0.0.1:9003",
        },
        "storage": {
            "db_path": "/opt/outbe-chain/state/hyperlane/db",
        },
        "server": {
            "bind_address": "0.0.0.0:9000",
            "public_url": public_url,
        },
        "origin": origins,
    });
    let value: toml::Value = serde_json::from_value(value)
        .map_err(|error| GenesisError::validation(format!("Hyperlane TOML: {error}")))?;
    toml::to_string_pretty(&value)
        .map_err(|error| GenesisError::validation(format!("Hyperlane TOML: {error}")))
}

/// Render the daemon launcher. `outbe-up` injects the private key into `/run`.
pub fn hyperlane_script(
    config: &Value,
    index: usize,
    base_dir: &str,
    host: &str,
    validator: &Value,
) -> Result<String, GenesisError> {
    let chain_id = parse_u64(
        config
            .get("chain_id")
            .ok_or_else(|| GenesisError::validation("chain_id is required"))?,
    )?;
    let address = validator
        .get("address")
        .and_then(Value::as_str)
        .ok_or_else(|| GenesisError::validation("validator missing address"))?;
    let keys_dir = config
        .get("remote_keys_dir")
        .and_then(Value::as_str)
        .unwrap_or(base_dir);
    let rpc_bind = rpc_bind_ip(config, host)?;
    let validator_dir = format!("{base_dir}/validator-{index}");
    Ok(format!(
        "\nexport OUTBE_CHAIN_ID={}\n\
export OUTBE_RPC_BIND_IP={}\n\
export OUTBE_VALIDATOR_ADDRESS={}\n\
export OUTBE_HYPERLANE_KEY_PATH={}\n\
export OUTBE_HYPERLANE_CONFIG_PATH={}\n\
{}/outbe-up internal wait-rpc\n\
exec {}/outbe-up internal hyperlane\n",
        quote(&chain_id.to_string()),
        quote(&rpc_bind.to_string()),
        quote(address),
        quote(&format!("{keys_dir}/validator-{index}/hyperlane-key.hex")),
        quote(&format!("{validator_dir}/hyperlane-public.toml")),
        quote(base_dir),
        quote(base_dir),
    ))
}

/// Render the idempotent on-chain delegate/fund/register setup command.
pub fn hyperlane_setup_script(
    config: &Value,
    index: usize,
    base_dir: &str,
    host: &str,
) -> Result<String, GenesisError> {
    let keys_dir = config
        .get("remote_keys_dir")
        .and_then(Value::as_str)
        .unwrap_or(base_dir);
    let funding = config
        .get("hyperlane_signer_funding_wei")
        .and_then(Value::as_str)
        .ok_or_else(|| GenesisError::validation("missing hyperlane_signer_funding_wei"))?;
    let rpc_bind = rpc_bind_ip(config, host)?;
    Ok(format!(
        "\nexport OUTBE_RPC_BIND_IP={}\n\
export OUTBE_VALIDATOR_KEY_PATH={}\n\
export OUTBE_HYPERLANE_KEY_PATH={}\n\
export OUTBE_HYPERLANE_FUNDING_WEI={}\n\
exec {}/outbe-up internal hyperlane-setup\n",
        quote(&rpc_bind.to_string()),
        quote(&format!("{keys_dir}/validator-{index}/evm-key.hex")),
        quote(&format!("{keys_dir}/validator-{index}/hyperlane-key.hex")),
        quote(funding),
        quote(base_dir),
    ))
}
