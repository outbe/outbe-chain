"""Render the per-machine launch bundle that goes with a generated genesis.

`create_genesis.py` calls this once the genesis is written. For each founding
validator it emits one directory of runnable scripts — MongoDB, TEE enclave,
Radicle sidecar, the node itself, and the price feeder — plus the reth bootnode
list and a DEPLOY.md that walks an operator through bringing the network up.

Nothing here invents protocol values: ports, hosts and addresses come from the
same network.yaml, and the identities come from the key directory.
"""

from __future__ import annotations

import json
import secrets
import shlex
import stat
from pathlib import Path
from typing import Any

# One layout for every founder: they run on separate machines, so they share
# the same ports. Any of these is overridable from the yaml.
DEFAULT_PORTS = {
    "reth_p2p_port": 30303,
    "reth_discv5_port": 31303,
    "rpc_port": 8545,
    "authrpc_port": 8551,
    "metrics_port": 9101,
    "radicle_port": 8776,
    "radicle_status_port": 8876,
    "tee_enclave_port": 17000,
    "feeder_health_port": 9002,
    "mongodb_port": 27017,
    "ocomp_supervisor_port": 9765,
}

SECP256K1_P = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F
SECP256K1_N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
SECP256K1_G = (
    0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798,
    0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8,
)

MONGO_IMAGE = "mongo:7"


def quote(value: Any) -> str:
    return shlex.quote(str(value))


def port_of(config: dict[str, Any], name: str) -> int:
    return int(config.get(name, DEFAULT_PORTS[name]))


def write_script(path: Path, body: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/usr/bin/env bash\nset -euo pipefail\n\n" + body.strip() + "\n")
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


# ---------------------------------------------------------------------------
# Node transport identity
# ---------------------------------------------------------------------------


def _point_add(p1, p2):
    if p1 is None:
        return p2
    if p2 is None:
        return p1
    x1, y1 = p1
    x2, y2 = p2
    if x1 == x2 and (y1 + y2) % SECP256K1_P == 0:
        return None
    if p1 == p2:
        lam = (3 * x1 * x1) * pow(2 * y1, -1, SECP256K1_P)
    else:
        lam = (y2 - y1) * pow(x2 - x1, -1, SECP256K1_P)
    lam %= SECP256K1_P
    x3 = (lam * lam - x1 - x2) % SECP256K1_P
    y3 = (lam * (x1 - x3) - y1) % SECP256K1_P
    return x3, y3


def _point_mul(scalar: int, point):
    result, addend = None, point
    while scalar:
        if scalar & 1:
            result = _point_add(result, addend)
        addend = _point_add(addend, addend)
        scalar >>= 1
    if result is None:
        raise ValueError("invalid secp256k1 scalar")
    return result


def ensure_reth_p2p_secret(directory: Path) -> str:
    """Return the enode node id, minting a stable p2p identity when absent.

    Without a persisted secret reth picks a fresh identity on every restart and
    the bootnode list stops resolving. This is a transport identity only: it
    signs no consensus message and holds no funds.
    """
    path = directory / "reth-p2p-secret.hex"
    if path.is_file():
        raw = path.read_text().strip().removeprefix("0x")
        if len(raw) != 64:
            raise ValueError(f"{path} must hold 32 bytes of hex")
    else:
        while True:
            candidate = secrets.token_bytes(32)
            if 1 <= int.from_bytes(candidate, "big") < SECP256K1_N:
                raw = candidate.hex()
                break
        path.write_text(raw)
        path.chmod(0o600)
    x, y = _point_mul(int(raw, 16), SECP256K1_G)
    return f"{x:064x}{y:064x}"


def ensure_ocomp_evm_key(directory: Path) -> None:
    """The embedded OCOMP runtime signs its submissions with this key.

    It defaults to a copy of the validator's own EVM key, which the chain always
    accepts as the sender. Operators wanting a dedicated operational key replace
    this file and register it with `outbe-cli validator delegate ocomp <addr>`.
    """
    ocomp_key = directory / "ocomp-evm-key.hex"
    if ocomp_key.is_file():
        return
    ocomp_key.write_text((directory / "evm-key.hex").read_text().strip() + "\n")
    ocomp_key.chmod(0o600)


def format_enode_host(host: str) -> str:
    return f"[{host}]" if ":" in host else host


# ---------------------------------------------------------------------------
# Per-component scripts
# ---------------------------------------------------------------------------


def mongodb_script(*, config: dict[str, Any], index: int) -> str:
    port = port_of(config, "mongodb_port")
    name = f"outbe-mongo-{index}"
    return f"""
# Transaction-capable MongoDB the node projects finalized Tribute and Nod
# bodies into. A single-node replica set satisfies the majority read/write
# concern the execution path requires.
NAME={quote(name)}
PORT={port}
VOLUME="$NAME-data"

if [ -n "$(docker ps -q -f name="^$NAME$")" ]; then
  echo "$NAME already running"
  exit 0
fi
docker rm -f "$NAME" >/dev/null 2>&1 || true
docker volume create "$VOLUME" >/dev/null
docker run -d --name "$NAME" \\
  -p 127.0.0.1:$PORT:$PORT \\
  -v "$VOLUME":/data/db \\
  --restart unless-stopped \\
  {MONGO_IMAGE} --replSet rs0 --bind_ip_all --port $PORT

echo "waiting for MongoDB..."
until docker exec "$NAME" mongosh --quiet --port "$PORT" --eval 'db.runCommand({{ping:1}})' >/dev/null 2>&1; do
  sleep 1
done
docker exec "$NAME" mongosh --quiet --port "$PORT" --eval \\
  'try {{ rs.status() }} catch (e) {{ rs.initiate({{_id:"rs0",members:[{{_id:0,host:"127.0.0.1:"+{port}}}]}}) }}' >/dev/null
echo "MongoDB ready on 127.0.0.1:$PORT (replica set rs0)"
"""


def enclave_script(*, config: dict[str, Any], index: int, base_dir: str) -> str:
    tee = config["tee"]
    mode = str(tee["mode"])
    enclave_dir = str(config.get("enclave_dir", f"{base_dir}/enclave"))
    enclave_runner = str(config.get("enclave_runner", "./run-enclave.sh"))
    chain_id_hex = f"0x{int(config['chain_id']):064x}"
    port = port_of(config, "tee_enclave_port")
    endpoint = f"127.0.0.1:{port}"
    image = str(config.get("enclave_image", "outbe-tee-enclave:latest"))
    validator_dir = f"{base_dir}/validator-{index}"
    name = f"outbe-enclave-{index}"

    header = f"""
# TEE enclave serving the tribute-offer key over a loopback socket. The node
# refuses to start until this answers.
mkdir -p {quote(validator_dir + "/tee")}
docker rm -f {quote(name)} >/dev/null 2>&1 || true

docker run -d --name {quote(name)} \\
  --network host \\
  --restart unless-stopped \\
  --log-driver local --log-opt max-size=10m --log-opt max-file=3 \\"""

    if mode == "dcap-required":
        # Production runs the enclave natively under gramine-sgx, the way the
        # live network does: the SGX driver, the AESM socket and the sealed
        # state all live on the host, and a container only adds a layer between
        # the enclave and the hardware it must attest against.
        return f"""
# TEE enclave under gramine-sgx, on the host. Requires the SGX driver
# (/dev/sgx_enclave, /dev/sgx_provision) and a running aesmd.service.
# The node refuses to start until this answers on {endpoint}.
mkdir -p {quote(validator_dir + "/tee")}

for device in /dev/sgx_enclave /dev/sgx_provision; do
  [ -e "$device" ] || {{ echo "missing $device: SGX driver not loaded" >&2; exit 1; }}
done
systemctl is-active --quiet aesmd.service \\
  || {{ echo "aesmd.service is not running; DCAP quoting will fail" >&2; exit 1; }}

cd {quote(enclave_dir)}
exec {quote(enclave_runner)} \\
  --socket {quote(endpoint)} \\
  --tee-dir {quote(validator_dir + "/tee")} \\
  --chain-id {chain_id_hex}
"""
    return header + f"""
  --security-opt seccomp=unconfined \\
  -v {quote(base_dir + "/test-sgx-signing-key.pem")}:/run/secrets/outbe-test-sgx-key.pem:ro \\
  -v {quote(validator_dir + "/tee")}:/tee \\
  {quote(image)} \\
  --socket {quote(endpoint)} \\
  --dkg-seed {index + 1:064x} \\
  --tee-dir /tee \\
  --chain-id {chain_id_hex}

echo "enclave started on {endpoint} (gramine-direct-dev: unattested, dev only)"
"""


def radicle_script(
    *, config: dict[str, Any], index: int, host: str, keys_dir: str, repo_root: str
) -> str:
    port = port_of(config, "radicle_port")
    status_port = port_of(config, "radicle_status_port")
    home = f"{keys_dir}/validator-{index}/radicle"
    binary = str(config.get("radicle_binary", "outbe-radicle"))
    return f"""
# Validator-owned Radicle sidecar. The node refuses to start as a validator
# without its control socket, and the status endpoint must stay loopback-only.
exec {quote(binary)} \\
  --home {quote(home)} \\
  --control-socket {quote(home + "/node/outbe-control.sock")} \\
  --listen 0.0.0.0:{port} \\
  --status-listen 127.0.0.1:{status_port} \\
  --max-validators 4 \\
  --advertise {quote(f"{host}:{port}")}
"""


def node_script(
    *,
    config: dict[str, Any],
    index: int,
    base_dir: str,
    keys_dir: str,
    repo_root: str,
    consensus_port: int,
    host: str,
) -> str:
    binary = str(config.get("node_binary", "outbe-chain"))
    # Production pins the authenticated sealed session; the dev lane keeps the
    # policy default so the mock transport stays reachable.
    session_mode = (
        "  --tee-session-mode production-node-host \\\n"
        if config["tee"]["mode"] == "dcap-required"
        else ""
    )
    validator_keys = f"{keys_dir}/validator-{index}"
    validator_dir = f"{base_dir}/validator-{index}"
    radicle_home = f"{validator_keys}/radicle"
    return f"""
# Validator node: execution, consensus and the embedded OCOMP runtime in one
# process. OCOMP needs no separate daemon — it runs inside this binary and
# reads its keys from <datadir>/../ocomp/domain-v1.
KEYS={quote(validator_keys)}
DATA={quote(validator_dir + "/data")}
DOMAIN={quote(validator_dir + "/ocomp/domain-v1")}

mkdir -p "$DATA" "$DOMAIN" {quote(validator_dir + "/logs")}
install -m 600 "$KEYS/ocomp-key-v1.hex" "$DOMAIN/ocomp-key-v1.hex"
install -m 600 "$KEYS/ocomp-evm-key.hex" "$DOMAIN/ocomp-evm-key.hex"

# reth reads the p2p key from the file verbatim, so normalize it in place.
printf '%s' "$(tr -d '[:space:]' < "$KEYS/reth-p2p-secret.hex")" > "$KEYS/reth-p2p-secret.hex"

# A debug build needs a larger thread stack: block 1 lazily initializes k256's
# secp256k1 tables in an unoptimized frame that overflows reth's default.
export RUST_MIN_STACK="${{RUST_MIN_STACK:-16777216}}"

exec {quote(binary)} node \\
  --validator \\
{session_mode}\
  --chain {quote(base_dir + "/genesis.json")} \\
  --datadir "$DATA" \\
  --engine.persistence-threshold 0 \\
  --engine.memory-block-buffer-target 0 \\
  --consensus.signing-key "$KEYS/signing-key.hex" \\
  --validator.evm-key "$KEYS/evm-key.hex" \\
  --consensus.listen-addr 0.0.0.0:{consensus_port} \\
  --consensus.storage-dir {quote(validator_dir + "/consensus")} \\
  --radicle.control-socket {quote(radicle_home + "/node/outbe-control.sock")} \\
  --radicle.status-address 127.0.0.1:{port_of(config, "radicle_status_port")} \\
  --tee-enclave-socket 127.0.0.1:{port_of(config, "tee_enclave_port")} \\
  --projection.mongodb-uri "mongodb://127.0.0.1:{port_of(config, "mongodb_port")}/?replicaSet=rs0" \\
  --projection.mongodb-database {quote(f"outbe_projection_validator_{index}")} \\
  --projection.start-block 1 \\
  --http --http.addr 127.0.0.1 --http.port {port_of(config, "rpc_port")} \\
  --http.api eth,net,web3,outbe \\
  --authrpc.port {port_of(config, "authrpc_port")} \\
  --port {port_of(config, "reth_p2p_port")} \\
  --discovery.port {port_of(config, "reth_p2p_port")} \\
  --nat extip:{host} \\
  --discovery.v5.addr 0.0.0.0 \\
  --discovery.v5.port {port_of(config, "reth_discv5_port")} \\
  --p2p-secret-key "$KEYS/reth-p2p-secret.hex" \\
  --bootnodes "$(grep -v '^[[:space:]]*#' {quote(base_dir + "/reth-bootnodes.txt")} | paste -sd, -)" \\
  --metrics 127.0.0.1:{port_of(config, "metrics_port")} \\
  --ipcpath "$DATA/reth.ipc" \\
  --log.file.directory {quote(validator_dir + "/logs")}
"""


def feeder_config(
    *, config: dict[str, Any], index: int, validator: dict[str, Any], signer_key: str
) -> str:
    oracle = config.get("oracle", {}).get("config", {})
    return f"""# Price oracle feeder for validator-{index}.
[chain]
rpc_endpoint = "http://127.0.0.1:{port_of(config, "rpc_port")}"
chain_id = {int(config["chain_id"])}
gasless_oracle_votes = true

[account]
private_key = "0x{signer_key}"
validator_address = "{validator["address"]}"

[oracle]
vote_period = {int(oracle.get("vote_period", 8))}
poll_interval_secs = 2

[health]
enabled = true
bind_address = "127.0.0.1:{port_of(config, "feeder_health_port")}"

[[provider_endpoints]]
name = "outbe_prices"
rest = "{config.get("price_feed_rest", "https://prc.testnet.outbe.net")}"
websocket = "{config.get("price_feed_websocket", "prc.testnet.outbe.net")}"

[[currency_pairs]]
base = "COEN"
quote = "840"
chain_denom = "unit"
providers = ["outbe_prices"]

[[deviation_thresholds]]
base = "COEN"
threshold = 2.0
"""


def feeder_script(*, config: dict[str, Any], index: int, base_dir: str, repo_root: str) -> str:
    binary = str(config.get("feeder_binary", "outbe-feeder"))
    return f"""
# Price oracle feeder. Start it once the node serves RPC; oracle votes are
# rejected before the chain produces blocks.
exec {quote(binary)} --config {quote(f"{base_dir}/validator-{index}/feeder.toml")}
"""


def start_all_script(*, config: dict[str, Any], index: int, base_dir: str) -> str:
    directory = f"{base_dir}/validator-{index}"
    status_port = port_of(config, "radicle_status_port")
    rpc_port = port_of(config, "rpc_port")
    return f"""
# Bring up validator-{index} in dependency order: MongoDB and the enclave must
# answer before the node starts, and Radicle before the node's preflight.
cd {quote(directory)}
mkdir -p logs

./run-mongodb.sh
./run-enclave.sh

nohup ./run-radicle.sh > logs/radicle.log 2>&1 &
echo $! > radicle.pid
echo "waiting for the Radicle sidecar..."
for _ in $(seq 1 60); do
  if (exec 3<>/dev/tcp/127.0.0.1/{status_port}) 2>/dev/null; then exec 3>&-; break; fi
  sleep 1
done

nohup ./run-node.sh > logs/node.log 2>&1 &
echo $! > node.pid
echo "waiting for RPC..."
for _ in $(seq 1 180); do
  if (exec 3<>/dev/tcp/127.0.0.1/{rpc_port}) 2>/dev/null; then exec 3>&-; break; fi
  sleep 1
done

nohup ./run-feeder.sh > logs/feeder.log 2>&1 &
echo $! > feeder.pid

# OCOMP runs as its own processes: the node's ExEx drives them but does not
# host them. The Supervisor must answer before the Worker can register.
nohup ./run-ocomp-supervisor.sh > logs/ocomp-supervisor.log 2>&1 &
echo $! > ocomp-supervisor.pid
for _ in $(seq 1 60); do
  if (exec 3<>/dev/tcp/127.0.0.1/{{SUPERVISOR_PORT}}) 2>/dev/null; then exec 3>&-; break; fi
  sleep 1
done
nohup ./run-ocomp-exporter.sh > logs/ocomp-exporter.log 2>&1 &
echo $! > ocomp-exporter.pid
nohup ./run-ocomp-worker.sh > logs/ocomp-worker.log 2>&1 &
echo $! > ocomp-worker.pid

echo
echo "validator-{index} started:"
echo "  node:    logs/node.log    (pid $(cat node.pid))"
echo "  radicle: logs/radicle.log (pid $(cat radicle.pid))"
echo "  feeder:  logs/feeder.log  (pid $(cat feeder.pid))"
echo "  ocomp:   logs/ocomp-supervisor.log, logs/ocomp-exporter.log, logs/ocomp-worker.log"
echo "  enclave: docker logs outbe-enclave-{index}"
echo "  mongo:   docker logs outbe-mongo-{index}"
"""


def stop_all_script(*, index: int, base_dir: str) -> str:
    directory = f"{base_dir}/validator-{index}"
    return f"""
cd {quote(directory)}
for name in ocomp-worker ocomp-exporter ocomp-supervisor feeder node radicle; do
  if [ -f "$name.pid" ]; then
    pid="$(cat "$name.pid")"
    if kill -0 "$pid" 2>/dev/null; then
      kill "$pid"
      echo "stopped $name (pid $pid)"
    fi
    rm -f "$name.pid"
  fi
done
docker stop outbe-enclave-{index} >/dev/null 2>&1 && echo "stopped enclave" || true
echo "MongoDB left running; stop it with: docker stop outbe-mongo-{index}"
"""


# ---------------------------------------------------------------------------
# OCOMP roles
# ---------------------------------------------------------------------------


def ocomp_identity(genesis_path: Path, keys_dir: Path) -> dict[str, Any]:
    """Read the identity the OCOMP roles must be started with.

    chain id, genesis hash and install hash are plain fields of the genesis.
    The protocol bundle hash is not: it sits inside the canonical install
    bytes, right after the genesis hash and the fork id, both of which are
    known here — so it is located by anchoring on the genesis hash rather than
    by a bare offset. Every role validates the value against the bundle file it
    loads and exits with `HashMismatch` if it disagrees, so a wrong read fails
    immediately and loudly instead of producing a subtly wrong network.
    """
    genesis = json.loads(genesis_path.read_text())
    config = genesis.get("config", {})
    install = config.get("ocompForkInstallV1")
    if not install:
        raise ValueError(f"{genesis_path} has no OCOMP install; cannot launch OCOMP roles")
    canonical = bytes.fromhex(str(install["canonicalBytes"]).removeprefix("0x"))
    genesis_hash = bytes.fromhex(genesis_hash_of(keys_dir).removeprefix("0x"))
    anchor = canonical.find(genesis_hash)
    if anchor < 0:
        raise ValueError("OCOMP install does not carry this genesis hash")
    # request profile order: genesis_hash, fork_id, protocol_bundle_hash
    bundle_hash = canonical[anchor + 64 : anchor + 96]
    if len(bundle_hash) != 32:
        raise ValueError("OCOMP install is truncated before the protocol bundle hash")
    return {
        "chain_id": int(config["chainId"]),
        "genesis_hash": "0x" + genesis_hash.hex(),
        "protocol_bundle_hash": "0x" + bundle_hash.hex(),
        "install_hash": str(install["installHash"]),
    }


def genesis_hash_of(keys_dir: Path) -> str:
    """The genesis hash the OCOMP registrations were minted against.

    create_genesis.py records it beside each registration it mints, which is
    exactly the value the install document carries.
    """
    for marker in sorted(keys_dir.glob("validator-*/ocomp-registration-v1.genesis-hash")):
        return marker.read_text().strip()
    raise ValueError(
        f"no OCOMP registration marker under {keys_dir}; cannot identify the genesis "
        f"the OCOMP roles must run against"
    )


def ocomp_role_preamble(*, config: dict[str, Any], index: int, base_dir: str, identity: dict[str, Any]) -> str:
    return f"""
export OUTBE_OCOMP_BASE_PATH={quote(base_dir)}
export OCOMP_VALIDATOR_INDEX={index}
export OCOMP_CHAIN_ID={identity["chain_id"]}
export OCOMP_GENESIS_HASH={identity["genesis_hash"]}
export OCOMP_BOOT_NONCE=0x{f"{index + 1:02x}" * 32}
export OCOMP_PROTOCOL_BUNDLE_HASH={identity["protocol_bundle_hash"]}
export OCOMP_REGISTRY_GENERATION=1
export OUTBE_OCOMP_RPC_URL="http://127.0.0.1:{port_of(config, "rpc_port")}"
"""


def ocomp_supervisor_script(*, config: dict[str, Any], index: int, base_dir: str, identity: dict[str, Any]) -> str:
    binary = str(config.get("ocomp_binary", "outbe-ocomp"))
    return f"""
# OCOMP Supervisor: hands work to the Workers and submits their results.
# Runs as its own process; the node only talks to it over loopback.
{ocomp_role_preamble(config=config, index=index, base_dir=base_dir, identity=identity).strip()}

exec {quote(binary)} supervisor \\
  --supervisor-address 127.0.0.1:{port_of(config, "ocomp_supervisor_port")}
"""


def ocomp_exporter_script(*, config: dict[str, Any], index: int, base_dir: str, identity: dict[str, Any]) -> str:
    binary = str(config.get("ocomp_binary", "outbe-ocomp"))
    database = f"outbe_projection_validator_{index}_ocomp"
    return f"""
# OCOMP SnapshotExporter: materializes the finalized inputs the Workers read.
{ocomp_role_preamble(config=config, index=index, base_dir=base_dir, identity=identity).strip()}
export OUTBE_OCOMP_PROJECTION_MONGODB_URI="mongodb://127.0.0.1:{port_of(config, "mongodb_port")}/?replicaSet=rs0"
export OUTBE_OCOMP_PROJECTION_MONGODB_DATABASE={quote(database)}

exec {quote(binary)} snapshot-exporter \\
  --supervisor-address 127.0.0.1:{port_of(config, "ocomp_supervisor_port")}
"""


def ocomp_worker_script(*, config: dict[str, Any], index: int, base_dir: str, identity: dict[str, Any], ordinal: int = 0) -> str:
    binary = str(config.get("ocomp_binary", "outbe-ocomp"))
    # Same shape the harness uses: first byte identifies the host, the last
    # four the worker ordinal, so every worker process gets a distinct nonce.
    nonce = f"{index + 1:02x}" + "00" * 27 + f"{ordinal:08x}"
    return f"""
# OCOMP Worker {ordinal}: runs the actual computation and signs its result.
{ocomp_role_preamble(config=config, index=index, base_dir=base_dir, identity=identity).strip()}

exec {quote(binary)} worker \\
  --chain-id {identity["chain_id"]} \\
  --genesis-hash {identity["genesis_hash"]} \\
  --boot-nonce 0x{nonce} \\
  --worker-ordinal {ordinal} \\
  --protocol-bundle-hash {identity["protocol_bundle_hash"]} \\
  --supervisor-address 127.0.0.1:{port_of(config, "ocomp_supervisor_port")}
"""

# ---------------------------------------------------------------------------
# DEPLOY.md
# ---------------------------------------------------------------------------


def deploy_markdown(
    *,
    config: dict[str, Any],
    validators: list[dict[str, Any]],
    base_dir: str,
    keys_dir: str,
    local_keys_dir: Path,
    enodes: list[str],
) -> str:
    chain_id = int(config["chain_id"])
    mode = str(config["tee"]["mode"])
    consensus_port = validators[0]["p2p_address"].rsplit(":", 1)[1]
    rows = "\n".join(
        f"| validator-{index} | `{validator['p2p_address']}` | `{validator['address']}` |"
        for index, validator in enumerate(validators)
    )
    bootnode_lines = "\n".join(f"  - `{enode}`" for enode in enodes)
    dev_warning = (
        ""
        if mode == "dcap-required"
        else (
            "\n> **`gramine-direct-dev`**: the enclave runs unattested. Use it for a "
            "devnet only. A production network needs `tee.mode: dcap-required`, real "
            "SGX hardware, and the exact release measurements.\n"
        )
    )
    return f"""# Deploying this network

Chain id `{chain_id}`, four founding validators, TEE mode `{mode}`.
{dev_warning}
| Machine | Consensus p2p | Validator address |
|---|---|---|
{rows}

Everything below was generated from your `network.yaml`. Ports are identical on
every machine, because each founder runs on its own host.

## 1. Copy files to each machine

Every machine gets the shared files plus **only its own** key directory:

```bash
# from this directory on the operator workstation, for machine N (0..3):
ssh <machine-N> "mkdir -p {base_dir} {keys_dir}"
scp genesis.json reth-bootnodes.txt protocol-bundle-v1.ocb1 <machine-N>:{base_dir}/
scp -r validator-N <machine-N>:{base_dir}/
scp -r {local_keys_dir}/validator-N <machine-N>:{keys_dir}/
```

`validator-N/` holds the run scripts; the key directory holds the secrets.
Never copy one validator's key directory to another machine.

## 2. Open the ports

Between the four machines:

| Port | Purpose |
|---|---|
| `{consensus_port}` | Commonware consensus p2p |
| `{port_of(config, "reth_p2p_port")}` | reth p2p (TCP + UDP) |
| `{port_of(config, "reth_discv5_port")}` | reth discv5 (UDP) |
| `{port_of(config, "radicle_port")}` | Radicle replication |

Everything else stays on loopback and must not be exposed: RPC
`{port_of(config, "rpc_port")}`, authrpc `{port_of(config, "authrpc_port")}`,
metrics `{port_of(config, "metrics_port")}`, Radicle status
`{port_of(config, "radicle_status_port")}`, the enclave socket
`{port_of(config, "tee_enclave_port")}`, MongoDB
`{port_of(config, "mongodb_port")}`, and the feeder health endpoint
`{port_of(config, "feeder_health_port")}`.

## 3. Prerequisites on every machine

- Docker — MongoDB and the TEE enclave run as containers
- `outbe-cli` on the machine you verify from (step 5)
- the `outbe-chain`, `outbe-ocomp`, `outbe-radicle` and `outbe-feeder` binaries on `PATH`
  (or set `node_binary`, `radicle_binary` and `feeder_binary` in the yaml to
  absolute paths and regenerate)
- for `dcap-required`: SGX hardware exposing `/dev/sgx_enclave` and
  `/dev/sgx_provision`

## 4. Start each machine

```bash
cd {base_dir}/validator-N
./start-all.sh
```

`start-all.sh` starts the components in dependency order — MongoDB, enclave,
Radicle sidecar, node, feeder — and writes pids and logs into
`{base_dir}/validator-N/`. To run one component in the foreground instead, call
its script directly: `run-mongodb.sh`, `run-enclave.sh`, `run-radicle.sh`,
`run-node.sh`, `run-feeder.sh`. `stop-all.sh` reverses it.

**Start all four machines within a few minutes of each other.** Block 1 carries
the founding DKG ceremony and needs every genesis validator online — unlike a
later reshare it does not complete on a threshold. If it fails, stop all four,
delete `{base_dir}/validator-N/data` on each machine, and start again; the
genesis itself stays valid.

## 5. Verify

```bash
# height advances on each machine
curl -s -X POST http://127.0.0.1:{port_of(config, "rpc_port")} \\
  -H 'content-type: application/json' \\
  -d '{{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}}'

# all four validators active
outbe-cli --rpc-url http://127.0.0.1:{port_of(config, "rpc_port")} validator list

# feeder health
curl -s http://127.0.0.1:{port_of(config, "feeder_health_port")}/health
```

The network is healthy when every machine reports an advancing non-zero block
number and `validator list` shows four active validators.

## What runs where

| Component | Process | Notes |
|---|---|---|
| Execution + consensus | `outbe-chain node` | one binary, no Engine API split |
| OCOMP Supervisor | `outbe-ocomp supervisor` | own process; hands out work, submits results |
| OCOMP SnapshotExporter | `outbe-ocomp snapshot-exporter` | own process; materializes finalized inputs |
| OCOMP Worker | `outbe-ocomp worker` | own process, ordinal 0 |
| TEE enclave | Docker container | the node refuses to start without it |
| Radicle | `outbe-radicle` | validator startup requires its control socket |
| Price feeder | `outbe-feeder` | submits oracle votes |
| Projection store | MongoDB container | replica set `rs0`, mandatory |

## Notes

- OCOMP is three separate processes per validator, not something the node
  hosts: the node carries only the ExEx that drives them. They are started by
  `start-all.sh` after the node and talk to it over loopback.
- The OCOMP roles sign their submissions with `ocomp-evm-key.hex`,
  which defaults to a copy of the validator's own EVM key. To use a dedicated
  operational key, replace that file and register it on-chain with
  `outbe-cli validator delegate ocomp <address>`.
- `reth-bootnodes.txt` pins each machine's reth identity:
{bootnode_lines}
- Re-running `create_genesis.py` without the pinned `timestamp:` produces a
  different genesis hash and invalidates the OCOMP registrations. Keep the
  timestamp that is now in your yaml.
"""


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def render(
    *,
    config: dict[str, Any],
    validators: list[dict[str, Any]],
    genesis_path: Path,
    keys_dir: Path,
    repo_root: Path,
) -> None:
    output_dir = genesis_path.parent
    # Paths as they will exist on the target machines. They default to this
    # workstation's layout so a single-host trial run works unchanged.
    base_dir = str(config.get("remote_base_dir", output_dir))
    remote_keys_dir = str(config.get("remote_keys_dir", keys_dir))

    enodes = []
    for index, validator in enumerate(validators):
        directory = keys_dir / f"validator-{index}"
        host = validator["p2p_address"].rsplit(":", 1)[0]
        node_id = ensure_reth_p2p_secret(directory)
        ensure_ocomp_evm_key(directory)
        enodes.append(
            f"enode://{node_id}@{format_enode_host(host)}:{port_of(config, 'reth_p2p_port')}"
        )
    (output_dir / "reth-bootnodes.txt").write_text("\n".join(enodes) + "\n")
    identity = ocomp_identity(genesis_path, keys_dir)

    for index, validator in enumerate(validators):
        directory = output_dir / f"validator-{index}"
        host, _, consensus_port = validator["p2p_address"].rpartition(":")

        write_script(directory / "run-mongodb.sh", mongodb_script(config=config, index=index))
        write_script(
            directory / "run-enclave.sh",
            enclave_script(config=config, index=index, base_dir=base_dir),
        )
        write_script(
            directory / "run-radicle.sh",
            radicle_script(
                config=config,
                index=index,
                host=host,
                keys_dir=remote_keys_dir,
                repo_root=str(repo_root),
            ),
        )
        write_script(
            directory / "run-node.sh",
            node_script(
                config=config,
                index=index,
                base_dir=base_dir,
                keys_dir=remote_keys_dir,
                repo_root=str(repo_root),
                consensus_port=int(consensus_port),
                host=host,
            ),
        )
        write_script(
            directory / "run-feeder.sh",
            feeder_script(config=config, index=index, base_dir=base_dir, repo_root=str(repo_root)),
        )
        write_script(
            directory / "run-ocomp-supervisor.sh",
            ocomp_supervisor_script(
                config=config, index=index, base_dir=base_dir, identity=identity
            ),
        )
        write_script(
            directory / "run-ocomp-exporter.sh",
            ocomp_exporter_script(
                config=config, index=index, base_dir=base_dir, identity=identity
            ),
        )
        write_script(
            directory / "run-ocomp-worker.sh",
            ocomp_worker_script(
                config=config, index=index, base_dir=base_dir, identity=identity
            ),
        )
        signer_key = (
            (keys_dir / f"validator-{index}" / "evm-key.hex")
            .read_text()
            .strip()
            .removeprefix("0x")
        )
        feeder_path = directory / "feeder.toml"
        feeder_path.write_text(
            feeder_config(
                config=config, index=index, validator=validator, signer_key=signer_key
            )
        )
        feeder_path.chmod(0o600)
        write_script(
            directory / "start-all.sh",
            start_all_script(config=config, index=index, base_dir=base_dir).replace(
                "{SUPERVISOR_PORT}", str(port_of(config, "ocomp_supervisor_port"))
            ),
        )
        write_script(directory / "stop-all.sh", stop_all_script(index=index, base_dir=base_dir))

    (output_dir / "DEPLOY.md").write_text(
        deploy_markdown(
            config=config,
            validators=validators,
            base_dir=base_dir,
            keys_dir=remote_keys_dir,
            local_keys_dir=keys_dir,
            enodes=enodes,
        )
    )
