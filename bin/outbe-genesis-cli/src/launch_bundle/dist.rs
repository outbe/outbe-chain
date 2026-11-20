//! Per-machine `.tgz` archive packaging, `SHA256SUMS`, `unpack.sh`, and `DEPLOY.md`.

use std::fs::{self, File};
use std::path::Path;

use flate2::write::GzEncoder;
use flate2::Compression;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::GenesisError;
use crate::network::rpc_bind_ip;
use crate::seed_codec::parse_u64;

use super::{copy_dir_recursive, port_of, quote, write_script, ROOT_HELPER};

fn pack_validator_archive(
    output_dir: &Path,
    dist: &Path,
    keys_dir: &Path,
    index: usize,
) -> Result<String, GenesisError> {
    let staging = dist.join(format!("validator-{index}"));
    fs::create_dir_all(&staging).map_err(|err| GenesisError::io(&staging, err))?;

    for name in ["genesis.json", "reth-bootnodes.txt", "DEPLOY.md"] {
        let item = output_dir.join(name);
        if item.is_file() {
            let dst = staging.join(name);
            fs::copy(&item, &dst).map_err(|err| GenesisError::io(&dst, err))?;
        }
    }
    let crs = output_dir.join(".bb-crs/bn254_g1.dat");
    let staged_crs = staging.join(".bb-crs/bn254_g1.dat");
    fs::create_dir_all(staged_crs.parent().expect("CRS path has a parent"))
        .map_err(|err| GenesisError::io(&staged_crs, err))?;
    fs::copy(&crs, &staged_crs).map_err(|err| GenesisError::io(&staged_crs, err))?;

    let val_folder = format!("validator-{index}");
    copy_dir_recursive(&output_dir.join(&val_folder), &staging.join(&val_folder))?;

    let keys_target = staging.join("keys").join(&val_folder);
    copy_dir_recursive(&keys_dir.join(&val_folder), &keys_target)?;

    let archive_name = format!("validator-{index}.tgz");
    let archive_path = dist.join(&archive_name);
    {
        let tar_gz =
            File::create(&archive_path).map_err(|err| GenesisError::io(&archive_path, err))?;
        let enc = GzEncoder::new(tar_gz, Compression::default());
        let mut tar = tar::Builder::new(enc);

        let dir_iter = fs::read_dir(&staging).map_err(|err| GenesisError::io(&staging, err))?;
        let mut entries: Vec<_> = dir_iter
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| GenesisError::io(&staging, err))?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name();
            if path.is_dir() {
                tar.append_dir_all(&name, &path)
                    .map_err(|err| GenesisError::io(&archive_path, err))?;
            } else {
                tar.append_path_with_name(&path, &name)
                    .map_err(|err| GenesisError::io(&archive_path, err))?;
            }
        }
        let enc = tar
            .into_inner()
            .map_err(|err| GenesisError::io(&archive_path, err))?;
        enc.finish()
            .map_err(|err| GenesisError::io(&archive_path, err))?;
    }

    fs::remove_dir_all(&staging).map_err(|err| GenesisError::io(&staging, err))?;
    Ok(archive_name)
}

/// Pack one self-contained `.tgz` archive per validator machine and write `SHA256SUMS` + `unpack.sh`.
pub fn build_distribution(
    output_dir: &Path,
    validators: &[Value],
    keys_dir: &Path,
    base_dir: &str,
) -> Result<Vec<String>, GenesisError> {
    let dist = output_dir.join("dist");
    if dist.exists() {
        fs::remove_dir_all(&dist).map_err(|err| GenesisError::io(&dist, err))?;
    }
    fs::create_dir_all(&dist).map_err(|err| GenesisError::io(&dist, err))?;

    let mut names = Vec::with_capacity(validators.len());
    for index in 0..validators.len() {
        names.push(pack_validator_archive(output_dir, &dist, keys_dir, index)?);
    }

    let mut sum_lines = Vec::with_capacity(names.len());
    for name in &names {
        let archive_file = dist.join(name);
        let bytes = fs::read(&archive_file).map_err(|err| GenesisError::io(&archive_file, err))?;
        let digest = hex::encode(Sha256::digest(&bytes));
        sum_lines.push(format!("{digest}  {name}"));
    }
    let sums_path = dist.join("SHA256SUMS");
    fs::write(&sums_path, format!("{}\n", sum_lines.join("\n")))
        .map_err(|err| GenesisError::io(&sums_path, err))?;

    let q_base = quote(base_dir);
    let unpack = format!(
        "\n{root_helper}\n\n\
# Unpack this machine's archive into {base_dir}. Run it ON the target machine,\n\
# from the directory holding validator-<index>.tgz.\n\
INDEX=\"${{1:?usage: unpack.sh <validator-index>}}\"\n\
ARCHIVE=\"validator-$INDEX.tgz\"\n\
\n\
[ -f \"$ARCHIVE\" ] || {{ echo \"no $ARCHIVE here\" >&2; exit 1; }}\n\
if command -v sha256sum >/dev/null && [ -f SHA256SUMS ]; then\n\
  grep \" $ARCHIVE\\$\" SHA256SUMS | sha256sum -c - || {{ echo \"checksum mismatch\" >&2; exit 1; }}\n\
fi\n\
\n\
as_root mkdir -p {q_base}\n\
as_root chown \"$USER\" {q_base}\n\
tar xzf \"$ARCHIVE\" -C {q_base}\n\
chmod -R go-rwx {q_base}/keys\n\
\n\
echo \"unpacked into {base_dir}; next: {base_dir}/validator-$INDEX/install-systemd.sh\"\n",
        root_helper = ROOT_HELPER,
    );
    write_script(&dist.join("unpack.sh"), &unpack)?;
    Ok(names)
}

/// Render `DEPLOY.md` operator guide.
pub fn deploy_markdown(
    config: &Value,
    validators: &[Value],
    keys_dir: &Path,
    enodes: &[String],
) -> Result<String, GenesisError> {
    let default_base = keys_dir.parent().unwrap_or(keys_dir).display().to_string();
    let base_dir = config
        .get("remote_base_dir")
        .and_then(Value::as_str)
        .unwrap_or(&default_base);
    let chain_id = parse_u64(
        config
            .get("chain_id")
            .ok_or_else(|| GenesisError::validation("chain_id is required"))?,
    )?;
    let mode = config
        .pointer("/tee/mode")
        .and_then(Value::as_str)
        .ok_or_else(|| GenesisError::validation("tee.mode is required"))?;
    let first_p2p = validators
        .first()
        .and_then(|v| v.get("p2p_address"))
        .and_then(Value::as_str)
        .ok_or_else(|| GenesisError::validation("validators[0] missing p2p_address"))?;
    let consensus_port = first_p2p
        .rsplit_once(':')
        .map(|(_, port)| port)
        .unwrap_or("26656");

    let mut rows = Vec::with_capacity(validators.len());
    for (index, validator) in validators.iter().enumerate() {
        let p2p = validator
            .get("p2p_address")
            .and_then(Value::as_str)
            .unwrap_or("");
        let addr = validator
            .get("address")
            .and_then(Value::as_str)
            .unwrap_or("");
        let hyperlane = validator
            .get("hyperlane_address")
            .and_then(Value::as_str)
            .unwrap_or("");
        rows.push(format!(
            "| validator-{index} | `{p2p}` | `{addr}` | `{hyperlane}` |"
        ));
    }
    let rows_joined = rows.join("\n");
    let founder_count = validators.len();
    let last_founder = founder_count.saturating_sub(1);
    let bootnode_lines = enodes
        .iter()
        .map(|enode| format!("  - `{enode}`"))
        .collect::<Vec<_>>()
        .join("\n");
    let dev_warning = if mode == "dcap-required" {
        ""
    } else {
        "\n> **`no-attestation-dev`**: the node runs TEE operations without \
hardware attestation. Use this mode only on devnet or testnet. Mainnet requires \
`tee.mode: dcap-required` and a compatible attestation source.\n"
    };

    let reth_p2p_port = port_of(config, "reth_p2p_port")?;
    let reth_discv5_port = port_of(config, "reth_discv5_port")?;
    let radicle_port = port_of(config, "radicle_port")?;
    let rpc_port = port_of(config, "rpc_port")?;
    let authrpc_port = port_of(config, "authrpc_port")?;
    let metrics_port = port_of(config, "metrics_port")?;
    let radicle_status_port = port_of(config, "radicle_status_port")?;
    let feeder_health_port = port_of(config, "feeder_health_port")?;
    let first_host = first_p2p
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(first_p2p);
    let rpc_bind = if config.get("rpc_bind_ip").and_then(Value::as_str) == Some("validator") {
        "<this-validator-private-ip>".to_owned()
    } else {
        rpc_bind_ip(config, first_host)?.to_string()
    };
    Ok(format!(
        "# Deploying this network\n\
\n\
Chain id `{chain_id}`, {founder_count} founding validators, TEE mode `{mode}`.\n\
{dev_warning}\n\
| Machine | Consensus p2p | Validator address | Hyperlane signer |\n\
|---|---|---|---|\n\
{rows_joined}\n\
\n\
Everything below was generated from your `network.yaml`. Ports are identical on\n\
every machine, because each founder runs on its own host.\n\
\n\
## 1. Copy the per-validator archive to each machine\n\
\n\
The generator writes one self-contained archive per machine under `dist/`. Each\n\
archive contains the shared genesis and bootnodes, systemd units and installer,\n\
launch scripts, and **only that validator's** keys.\n\
\n\
```bash\n\
# From the generated output directory, for machine N (0..{last_founder}):\n\
scp dist/validator-N.tgz dist/SHA256SUMS dist/unpack.sh <machine-N>:~/\n\
ssh <machine-N>\n\
cd ~\n\
chmod +x unpack.sh\n\
./unpack.sh N\n\
```\n\
\n\
`unpack.sh` verifies the archive checksum and extracts it into `{base_dir}`.\n\
Never copy one validator's archive or key directory to another machine.\n\
\n\
## 2. Open the ports\n\
\n\
Between all founding machines:\n\
\n\
| Port | Purpose |\n\
|---|---|\n\
| `{consensus_port}` | Commonware consensus p2p |\n\
| `{reth_p2p_port}` | reth p2p (TCP + UDP) |\n\
| `{reth_discv5_port}` | reth discv5 (UDP) |\n\
| `{radicle_port}` | Radicle replication |\n\
| `9000` | Hyperlane checkpoints (public) |\n\
\n\
RPC uses its explicit loopback/private bind `{rpc_bind}:{rpc_port}`. Everything\n\
else stays on loopback and must not be exposed: authrpc `{authrpc_port}`,\n\
metrics `{metrics_port}`, Radicle status\n\
`{radicle_status_port}`, the feeder health endpoint\n\
`{feeder_health_port}` and the Hyperlane health endpoint `9003`.\n\
\n\
## 3. Prerequisites on every machine\n\
\n\
- Python 3.11+ - reads the shared storage TOML at launch\n\
- the `outbe-chain`, `outbe-radicle`, `outbe-feeder` and\n\
  `outbe-hyperlane-validator` binaries on `PATH`\n\
  (or set the corresponding binary fields in the yaml to\n\
  absolute paths and regenerate)\n\
\n\
## 4. Start each machine\n\
\n\
```bash\n\
cd {base_dir}/validator-N\n\
./preflight.sh N        # verifies genesis/CRS/state before anything starts\n\
./install-systemd.sh\n\
```\n\
\n\
`install-systemd.sh` installs one non-templated unit per role and starts the\n\
`outbe-validator.target`. The node manages its resident TEE lifecycle and renewal.\n\
Use `./install-systemd.sh --prepare-only` to install without starting it.\n\
The services recover after clean or failed exits. `preflight.sh`\n\
is read-only: run it first and compare the genesis digest it prints across all\n\
founding machines.\n\
\n\
Inspect the stack with `systemctl status outbe-validator.target` and follow the\n\
node with `journalctl -u outbe-validator -f`.\n\
\n\
**Start all {founder_count} machines within a few minutes of each other.** Block 1 carries\n\
the founding DKG ceremony and needs every genesis validator online - unlike a\n\
later reshare it does not complete on a threshold. If it fails, stop all founders,\n\
delete `{base_dir}/validator-N/data` on each machine, and start again; the\n\
genesis itself stays valid.\n\
\n\
## 5. Verify\n\
\n\
```bash\n\
# height advances on each machine\n\
curl -s -X POST http://{rpc_bind}:{rpc_port} \\\n\
  -H 'content-type: application/json' \\\n\
  -d '{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_blockNumber\",\"params\":[]}}'\n\
\n\
# all {founder_count} validators active\n\
outbe-cli --rpc-url http://{rpc_bind}:{rpc_port} validator list\n\
\n\
# feeder health\n\
curl -s http://127.0.0.1:{feeder_health_port}/health\n\
```\n\
\n\
The network is healthy when every machine reports an advancing non-zero block\n\
number and `validator list` shows {founder_count} active validators.\n\
\n\
## What runs where\n\
\n\
| Component | Process | Notes |\n\
|---|---|---|\n\
| Execution + consensus | `outbe-chain node` | one binary, no Engine API split |\n\
| TEE | resident inside `outbe-chain node` | autonomous lifecycle and renewal |\n\
| Radicle | `outbe-radicle` | validator startup requires its control socket |\n\
| Price feeder | `outbe-feeder` | submits oracle votes |\n\
| Hyperlane validator | `outbe-hyperlane-validator` | signs and serves checkpoints |\n\
\n\
## Storage configuration\n\
\n\
Tribute, Nod and Gem records, buckets and indexes live in the node's EVM state.\n\
Metadosis reads Tribute from state, executes confidential Lysis in the enclave,\n\
and saves the ready encrypted Nod records. No separate entity datasource is configured.\n\
\n\
## Notes\n\
\n\
- `reth-bootnodes.txt` pins each machine's reth identity:\n\
{bootnode_lines}\n\
- Re-running `outbe-genesis-cli` without the pinned `timestamp:` produces a\n\
  different genesis hash. Keep the\n\
  timestamp that is now in your yaml.\n"
    ))
}
