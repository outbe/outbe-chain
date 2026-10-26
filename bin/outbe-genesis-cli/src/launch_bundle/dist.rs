//! Per-machine `.tgz` archive packaging, `SHA256SUMS`, `unpack.sh`, and `DEPLOY.md`.

use std::fs::{self, File};
use std::path::Path;

use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::Value;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::error::GenesisError;
use crate::seed_codec::parse_u64;

use super::{port_of, quote, write_script};

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), GenesisError> {
    fs::create_dir_all(dst).map_err(|err| GenesisError::io(dst, err))?;
    for entry_res in WalkDir::new(src) {
        let entry = entry_res.map_err(|e| GenesisError::io(src, std::io::Error::other(e)))?;
        let rel = entry
            .path()
            .strip_prefix(src)
            .map_err(|e| GenesisError::io(src, std::io::Error::other(e)))?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target).map_err(|err| GenesisError::io(&target, err))?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|err| GenesisError::io(parent, err))?;
            }
            fs::copy(entry.path(), &target).map_err(|err| GenesisError::io(&target, err))?;
        }
    }
    Ok(())
}

fn pack_validator_archive(
    output_dir: &Path,
    dist: &Path,
    keys_dir: &Path,
    index: usize,
) -> Result<String, GenesisError> {
    let staging = dist.join(format!("validator-{index}"));
    fs::create_dir_all(&staging).map_err(|err| GenesisError::io(&staging, err))?;

    for name in [
        "genesis.json",
        "reth-bootnodes.txt",
        "DEPLOY.md",
        "install-systemd.sh",
    ] {
        let item = output_dir.join(name);
        if item.is_file() {
            let dst = staging.join(name);
            fs::copy(&item, &dst).map_err(|err| GenesisError::io(&dst, err))?;
        }
    }
    for folder in ["systemd", "enclave"] {
        let src = output_dir.join(folder);
        if src.is_dir() {
            copy_dir_recursive(&src, &staging.join(folder))?;
        }
    }

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
        let bytes =
            fs::read(&archive_file).map_err(|err| GenesisError::io(&archive_file, err))?;
        let digest = hex::encode(Sha256::digest(&bytes));
        sum_lines.push(format!("{digest}  {name}"));
    }
    let sums_path = dist.join("SHA256SUMS");
    fs::write(&sums_path, format!("{}\n", sum_lines.join("\n")))
        .map_err(|err| GenesisError::io(&sums_path, err))?;

    let q_base = quote(base_dir);
    let unpack = format!(
        "\n# Unpack this machine's archive into {base_dir}. Run it ON the target machine,\n\
# from the directory holding validator-<index>.tgz.\n\
INDEX=\"${{1:?usage: unpack.sh <validator-index>}}\"\n\
ARCHIVE=\"validator-$INDEX.tgz\"\n\
\n\
[ -f \"$ARCHIVE\" ] || {{ echo \"no $ARCHIVE here\" >&2; exit 1; }}\n\
if command -v sha256sum >/dev/null && [ -f SHA256SUMS ]; then\n\
  grep \" $ARCHIVE\\$\" SHA256SUMS | sha256sum -c - || {{ echo \"checksum mismatch\" >&2; exit 1; }}\n\
fi\n\
\n\
sudo mkdir -p {q_base}\n\
sudo chown \"$USER\" {q_base}\n\
tar xzf \"$ARCHIVE\" -C {q_base}\n\
chmod -R go-rwx {q_base}/keys\n\
\n\
# The signed enclave, when the bundle carries one, belongs next to the binaries.\n\
if [ -d {q_base}/enclave ]; then\n\
  sudo install -m 755 {q_base}/enclave/outbe-tee-enclave {q_base}/\n\
  sudo install -m 644 {q_base}/enclave/outbe-tee-enclave.manifest* {q_base}/\n\
  sudo install -m 644 {q_base}/enclave/outbe-tee-enclave.sig {q_base}/\n\
fi\n\
\n\
echo \"unpacked into {base_dir}; next: ./install-systemd.sh $INDEX\"\n"
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
    let default_base = keys_dir
        .parent()
        .unwrap_or(keys_dir)
        .display()
        .to_string();
    let base_dir = config
        .get("remote_base_dir")
        .and_then(Value::as_str)
        .unwrap_or(&default_base);
    let default_keys = keys_dir.display().to_string();
    let remote_keys_dir = config
        .get("remote_keys_dir")
        .and_then(Value::as_str)
        .unwrap_or(&default_keys);
    let chain_id = parse_u64(
        config
            .get("chain_id")
            .ok_or_else(|| GenesisError::validation("chain_id is required"))?,
    )?;
    let mode = config
        .get("tee")
        .and_then(|t| t.get("mode"))
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
        rows.push(format!("| validator-{index} | `{p2p}` | `{addr}` |"));
    }
    let rows_joined = rows.join("\n");
    let bootnode_lines = enodes
        .iter()
        .map(|enode| format!("  - `{enode}`"))
        .collect::<Vec<_>>()
        .join("\n");
    let dev_warning = if mode == "dcap-required" {
        ""
    } else {
        "\n> **`gramine-direct-dev`**: the enclave runs unattested. Use it for a \
devnet only. A production network needs `tee.mode: dcap-required`, real \
SGX hardware, and the exact release measurements.\n"
    };

    let reth_p2p_port = port_of(config, "reth_p2p_port")?;
    let reth_discv5_port = port_of(config, "reth_discv5_port")?;
    let radicle_port = port_of(config, "radicle_port")?;
    let rpc_port = port_of(config, "rpc_port")?;
    let authrpc_port = port_of(config, "authrpc_port")?;
    let metrics_port = port_of(config, "metrics_port")?;
    let radicle_status_port = port_of(config, "radicle_status_port")?;
    let tee_enclave_port = port_of(config, "tee_enclave_port")?;
    let feeder_health_port = port_of(config, "feeder_health_port")?;
    let keys_dir_display = keys_dir.display();

    Ok(format!(
        "# Deploying this network\n\
\n\
Chain id `{chain_id}`, four founding validators, TEE mode `{mode}`.\n\
{dev_warning}\n\
| Machine | Consensus p2p | Validator address |\n\
|---|---|---|\n\
{rows_joined}\n\
\n\
Everything below was generated from your `network.yaml`. Ports are identical on\n\
every machine, because each founder runs on its own host.\n\
\n\
## 1. Copy files to each machine\n\
\n\
Every machine gets the shared files plus **only its own** key directory:\n\
\n\
```bash\n\
# from this directory on the operator workstation, for machine N (0..3):\n\
ssh <machine-N> \"mkdir -p {base_dir} {remote_keys_dir}\"\n\
scp genesis.json reth-bootnodes.txt <machine-N>:{base_dir}/\n\
scp -r validator-N <machine-N>:{base_dir}/\n\
scp -r {keys_dir_display}/validator-N <machine-N>:{remote_keys_dir}/\n\
```\n\
\n\
`validator-N/` holds the run scripts; the key directory holds the secrets.\n\
Never copy one validator's key directory to another machine.\n\
\n\
## 2. Open the ports\n\
\n\
Between the four machines:\n\
\n\
| Port | Purpose |\n\
|---|---|\n\
| `{consensus_port}` | Commonware consensus p2p |\n\
| `{reth_p2p_port}` | reth p2p (TCP + UDP) |\n\
| `{reth_discv5_port}` | reth discv5 (UDP) |\n\
| `{radicle_port}` | Radicle replication |\n\
\n\
Everything else stays on loopback and must not be exposed: RPC\n\
`{rpc_port}`, authrpc `{authrpc_port}`,\n\
metrics `{metrics_port}`, Radicle status\n\
`{radicle_status_port}`, the enclave socket\n\
`{tee_enclave_port}` and the feeder health endpoint\n\
`{feeder_health_port}`.\n\
\n\
## 3. Prerequisites on every machine\n\
\n\
- Python 3.11+ - reads the shared storage TOML at launch\n\
- Docker - the TEE enclave runs as a container\n\
- `outbe-cli` on every SGX validator host for automatic TEE renewal\n\
- the `outbe-chain`, `outbe-radicle` and `outbe-feeder` binaries on `PATH`\n\
  (or set `node_binary`, `radicle_binary` and `feeder_binary` in the yaml to\n\
  absolute paths and regenerate)\n\
- for `dcap-required`: SGX hardware exposing `/dev/sgx_enclave` and\n\
  `/dev/sgx_provision`\n\
\n\
## 4. Start each machine\n\
\n\
```bash\n\
cd {base_dir}/validator-N\n\
./preflight.sh N        # verifies genesis/enclave/state before anything starts\n\
sudo {base_dir}/install-systemd.sh N\n\
```\n\
\n\
`install-systemd.sh` installs one templated unit per role, starts them in\n\
dependency order, and enables the daily TEE renewal timer for a NodeHost SGX\n\
session. The services recover after clean or failed exits. `preflight.sh` is\n\
read-only: run it first and compare the genesis and enclave digests it prints\n\
across all four machines - they must be identical.\n\
\n\
`start-all.sh` starts the components in dependency order - configured storage, enclave,\n\
Radicle sidecar, node, feeder - and writes pids and logs into\n\
`{base_dir}/validator-N/`. To run one component in the foreground instead, call\n\
its script directly: `run-enclave.sh`, `run-radicle.sh`,\n\
`run-node.sh`, `run-feeder.sh`. `stop-all.sh` reverses it.\n\
\n\
**Start all four machines within a few minutes of each other.** Block 1 carries\n\
the founding DKG ceremony and needs every genesis validator online - unlike a\n\
later reshare it does not complete on a threshold. If it fails, stop all four,\n\
delete `{base_dir}/validator-N/data` on each machine, and start again; the\n\
genesis itself stays valid.\n\
\n\
## 5. Verify\n\
\n\
```bash\n\
# height advances on each machine\n\
curl -s -X POST http://127.0.0.1:{rpc_port} \\\n\
  -H 'content-type: application/json' \\\n\
  -d '{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_blockNumber\",\"params\":[]}}'\n\
\n\
# all four validators active\n\
outbe-cli --rpc-url http://127.0.0.1:{rpc_port} validator list\n\
\n\
# feeder health\n\
curl -s http://127.0.0.1:{feeder_health_port}/health\n\
```\n\
\n\
The network is healthy when every machine reports an advancing non-zero block\n\
number and `validator list` shows four active validators.\n\
\n\
## What runs where\n\
\n\
| Component | Process | Notes |\n\
|---|---|---|\n\
| Execution + consensus | `outbe-chain node` | one binary, no Engine API split |\n\
| TEE enclave | Docker container | the node refuses to start without it |\n\
| Radicle | `outbe-radicle` | validator startup requires its control socket |\n\
| Price feeder | `outbe-feeder` | submits oracle votes |\n\
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
- Re-running `create_genesis.py` without the pinned `timestamp:` produces a\n\
  different genesis hash. Keep the\n\
  timestamp that is now in your yaml.\n"
    ))
}
