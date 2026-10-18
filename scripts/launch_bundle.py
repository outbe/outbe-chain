"""Render the per-machine launch bundle that goes with a generated genesis.

`create_genesis.py` calls this after it writes the genesis. For each founding
validator, it emits one directory of runnable scripts: storage setup, TEE
enclave, Radicle sidecar, the node itself, and the price feeder. It also emits
the reth bootnode list and a DEPLOY.md that guides an operator through the
network start.

Nothing here invents protocol values: ports, hosts and addresses come from the
same network.yaml, and the identities come from the key directory.
"""

from __future__ import annotations

import hashlib
import secrets
import shutil
import tarfile
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
    "public_rpc_port": 80,
    "public_radicle_status_port": 8080,
}

SECP256K1_P = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F
SECP256K1_N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
SECP256K1_G = (
    0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798,
    0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8,
)


# Protocol ceiling on the validator set, mirroring the ValidatorSet precompile
# default. The bundle uses it to size the connection limits of the Radicle
# sidecar. Thus validators that join later do not require a restart of every
# validator that already runs.
DEFAULT_MAX_VALIDATORS = 128


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


def _point_slope(p1: tuple[int, int], p2: tuple[int, int]) -> int:
    x1, y1 = p1
    x2, y2 = p2
    num, den = (3 * x1 * x1, 2 * y1) if p1 == p2 else (y2 - y1, x2 - x1)
    return (num * pow(den, -1, SECP256K1_P)) % SECP256K1_P


def _point_add(p1, p2):
    if p1 is None or p2 is None:
        return p2 if p1 is None else p1
    x1, y1 = p1
    x2, y2 = p2
    if x1 == x2 and (y1 + y2) % SECP256K1_P == 0:
        return None
    lam = _point_slope(p1, p2)
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


def _mint_secp256k1_hex(path: Path) -> str:
    while True:
        candidate = secrets.token_bytes(32)
        if 1 <= int.from_bytes(candidate, "big") < SECP256K1_N:
            raw = candidate.hex()
            path.write_text(raw)
            path.chmod(0o600)
            return raw


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
        raw = _mint_secp256k1_hex(path)
    x, y = _point_mul(int(raw, 16), SECP256K1_G)
    return f"{x:064x}{y:064x}"


def format_enode_host(host: str) -> str:
    return f"[{host}]" if ":" in host else host


# ---------------------------------------------------------------------------
# Per-component scripts
# ---------------------------------------------------------------------------


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

    if mode == "dcap-required":
        # Production runs the enclave natively under gramine-sgx, the way the
        # live network does. The SGX driver, the AESM socket and the sealed
        # state all live on the host. A container only adds a layer between the
        # enclave and the hardware that it must attest against.
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
    # The dev lane still runs the real enclave. On a host with SGX, it goes
    # through gramine-sgx with remote attestation disabled. That is the
    # `GramineDirectDev` profile: real hardware, no Intel collateral. The
    # container image is the fallback for a host without SGX.
    manifest = f"{enclave_dir}/outbe-tee-enclave.manifest.sgx"
    return f"""
# TEE enclave, unattested development profile. The node refuses to start
# until this answers on {endpoint}.
mkdir -p {quote(validator_dir + "/tee")}

if [ -e /dev/sgx_enclave ] && [ -f {quote(manifest)} ]; then
  # Real SGX hardware with a signed manifest: run it natively.
  #
  # The sealing directory is the one baked into the signed manifest, not a
  # per-validator path: Gramine only lets the enclave touch files the manifest
  # declares, so passing anything else fails with a bare "Permission denied".
  # One signed manifest per host therefore means one sealing directory per
  # host, which is what a real deployment has anyway.
  cd {quote(enclave_dir)}
  # `sudo` only when not already root: under systemd the unit runs as root and
  # sudo may not even be present in the service environment.
  exec ${{SUDO:-$([ "$(id -u)" = 0 ] || echo sudo)}} gramine-sgx outbe-tee-enclave \\
    --socket {quote(endpoint)} \\
    --tee-dir {quote(enclave_dir + "/tee")} \\
    --chain-id {chain_id_hex}
fi

# No SGX on this host: fall back to the container, which runs the enclave
# under gramine-direct (LibOS only, no hardware isolation).
docker rm -f {quote(name)} >/dev/null 2>&1 || true
exec docker run --rm --name {quote(name)} \\
  --network host \\
  --log-driver local --log-opt max-size=10m --log-opt max-file=3 \\
  --security-opt seccomp=unconfined \\
  -v {quote(base_dir + "/test-sgx-signing-key.pem")}:/run/secrets/outbe-test-sgx-key.pem:ro \\
  -v {quote(validator_dir + "/tee")}:/tee \\
  {quote(image)} \\
  --socket {quote(endpoint)} \\
  --dkg-seed {index + 1:064x} \\
  --tee-dir /tee \\
  --chain-id {chain_id_hex}
"""


def radicle_script(
    *,
    config: dict[str, Any],
    index: int,
    host: str,
    keys_dir: str,
) -> str:
    port = port_of(config, "radicle_port")
    status_port = port_of(config, "radicle_status_port")
    home = f"{keys_dir}/validator-{index}/radicle"
    binary = str(config.get("radicle_binary", "outbe-radicle"))
    validator_set = config.get("validator_set")
    max_validators = int(
        dict(validator_set or {}).get("max_validators", DEFAULT_MAX_VALIDATORS)
    )
    return f"""
# Validator-owned Radicle sidecar. The node refuses to start as a validator
# without its control socket, and the status endpoint must stay loopback-only.
#
# `outbe-keygen validator` creates only keys/; the sidecar additionally
# requires its working directories to exist, owner-only, before it will start.
for directory in storage node cobs; do
  path={quote(home)}/"$directory"
  [ -d "$path" ] || mkdir -m 700 -- "$path"
done

# A crash or a hard restart leaves the control socket behind, and Heartwood
# then refuses to start believing another node holds it. Clear it when nothing
# is actually listening, so a systemd restart recovers on its own.
socket={quote(home + "/node/outbe-control.sock")}
if [ -S "$socket" ] && ! pgrep -f "outbe-radicle .*{quote(home)}" >/dev/null 2>&1; then
  rm -f "$socket"
fi

exec {quote(binary)} \\
  --home {quote(home)} \\
  --control-socket {quote(home + "/node/outbe-control.sock")} \\
  --listen 0.0.0.0:{port} \\
  --status-listen 127.0.0.1:{status_port} \\
  `# connection ceiling, not the current set size: the sidecar tracks the` \\
  `# validator set from chain state, so a set that grows must not need a` \\
  `# restart. Sized by the protocol maximum the ValidatorSet enforces.` \\
  --max-validators {max_validators} \\
  --external-inbound-reserve {int(config.get("radicle_external_inbound_reserve", 16))} \\
  --advertise {quote(f"{host}:{port}")}
"""


def _uses_production_tee_session(config: dict[str, Any]) -> bool:
    return bool(config.get("enclave_sgx", True)) or (
        config["tee"]["mode"] == "dcap-required"
    )


def node_script(
    *,
    config: dict[str, Any],
    index: int,
    base_dir: str,
    p2p_address: str,
) -> str:
    keys_dir = str(config.get("remote_keys_dir", base_dir))
    host, _, consensus_port = p2p_address.rpartition(":")
    binary = str(config.get("node_binary", "outbe-chain"))
    # Which TEE transport the node must speak. `dcap-required` always uses the
    # authenticated sealed session. `gramine-direct-dev` is ambiguous on its
    # own. The genesis says "no Intel collateral", but the enclave may still be
    # a real gramine-sgx one. That is the SGX-without-DCAP profile. A real
    # enclave speaks the production session, not the mock transport. The
    # policy default would pick the mock one, and the node would fail with
    # "development enclave connection failed". Thus state the mode explicitly.
    session_mode = (
        "  --tee-session-mode production-node-host \\\n"
        if _uses_production_tee_session(config)
        else ""
    )
    validator_keys = f"{keys_dir}/validator-{index}"
    validator_dir = f"{base_dir}/validator-{index}"
    radicle_home = f"{validator_keys}/radicle"
    return f"""
# Validator node: execution and consensus with native enclave Lysis.
KEYS={quote(validator_keys)}
DATA={quote(validator_dir + "/data")}
mkdir -p "$DATA" {quote(validator_dir + "/logs")}

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
  --consensus.listen-addr 0.0.0.0:{int(consensus_port)} \\
  --consensus.storage-dir {quote(validator_dir + "/consensus")} \\
  --radicle.control-socket {quote(radicle_home + "/node/outbe-control.sock")} \\
  --radicle.status-address 127.0.0.1:{port_of(config, "radicle_status_port")} \\
  --tee-enclave-socket 127.0.0.1:{port_of(config, "tee_enclave_port")} \\
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


def feeder_pairs(*, config: dict[str, Any]) -> str:
    """The pairs file every validator's feeder.toml names."""
    price_provider = str(config.get("price_provider", "mock_http"))
    default_quote = {"mock_http": "840"}.get(price_provider, "USDT")
    source_quote = str(config.get("price_source_quote", default_quote))
    return f"""[[currency_pairs]]
base = "COEN"
quote = "840"
markets = [{{ provider = "{price_provider}", quote = "{source_quote}" }}]

[[deviation_thresholds]]
base = "COEN"
threshold = "2.0"
"""


def feeder_config(
    *, config: dict[str, Any], index: int, validator: dict[str, Any], signer_key: str
) -> str:
    oracle = config.get("oracle", {}).get("config", {})
    price_provider = str(config.get("price_provider", "mock_http"))
    if price_provider == "mock_http":
        api_provider = f'''[[api_providers]]
name = "mock_http"
rest = "{config.get("price_feed_rest", "https://prc.testnet.outbe.net")}"'''
    else:
        websocket = str(config.get("price_feed_websocket", "")).strip()
        websocket_line = f'\nwebsocket = "{websocket}"' if websocket else ""
        api_provider = f'''[[api_providers]]
name = "{price_provider}"{websocket_line}'''
    return f"""# Price oracle feeder for validator-{index}.
pairs = "pairs.toml"

[chain]
rpc_endpoint = "http://127.0.0.1:{port_of(config, "rpc_port")}"
chain_id = {int(config["chain_id"])}
gasless_oracle_votes = true

[account]
private_key = "0x{signer_key}"
validator_address = "{validator["address"]}"

[oracle]
vote_period = {int(oracle.get("vote_period", 60))}
poll_interval_secs = 2
# Pairs come from pairs.toml, not from the node.
sources = "local"

[health]
enabled = true
bind_address = "127.0.0.1:{port_of(config, "feeder_health_port")}"

# The feeder only accepts provider names from its built-in list (mock, pyth,
# chainlink, binance, kraken, okx, gate, huobi, mexc, coinbase, mock_http); an
# invented name is rejected at startup. `mock_http` uses the configured REST
# endpoint; exchange providers use live WebSocket market streams.
{api_provider}
"""


def feeder_script(*, config: dict[str, Any], index: int, base_dir: str) -> str:
    binary = str(config.get("feeder_binary", "outbe-feeder"))
    return f"""
# Price oracle feeder. Start it once the node serves RPC; oracle votes are
# rejected before the chain produces blocks.
exec {quote(binary)} --config {quote(f"{base_dir}/validator-{index}/feeder.toml")}
"""


def start_all_script(
    *, config: dict[str, Any], index: int, base_dir: str
) -> str:
    directory = f"{base_dir}/validator-{index}"
    status_port = port_of(config, "radicle_status_port")
    rpc_port = port_of(config, "rpc_port")
    return f"""
# Bring up the configured storage, enclave, Radicle and node in dependency order.
cd {quote(directory)}
mkdir -p logs


# run-enclave.sh execs into the enclave, so it must be backgrounded: calling it
# directly would replace this script and nothing below would ever run.
nohup ./run-enclave.sh > logs/enclave.log 2>&1 &
echo $! > enclave.pid
echo "waiting for the enclave..."
for _ in $(seq 1 60); do
  if (exec 3<>/dev/tcp/127.0.0.1/{{TEE_PORT}}) 2>/dev/null; then exec 3>&-; break; fi
  sleep 1
done

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

echo
echo "validator-{index} started:"
echo "  node:    logs/node.log    (pid $(cat node.pid))"
echo "  radicle: logs/radicle.log (pid $(cat radicle.pid))"
echo "  feeder:  logs/feeder.log  (pid $(cat feeder.pid))"
echo "  enclave: docker logs outbe-enclave-{index}"
"""


def stop_all_script(*, index: int, base_dir: str) -> str:
    directory = f"{base_dir}/validator-{index}"
    return f"""
cd {quote(directory)}
for name in feeder node radicle enclave; do
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
"""


# ---------------------------------------------------------------------------
# Signed enclave
# ---------------------------------------------------------------------------
#
# The enclave is signed ONCE, where the signing key lives, and the signed
# artifacts travel in the bundle. If each machine signs instead, every host gets
# its own mr_signer. That gives four different enclave identities on one
# network. A `dcap-required` genesis pins a single mrsigner, so it would reject
# them outright.
# The private key never enters the bundle.

SIGNED_ENCLAVE_FILES = (
    "outbe-tee-enclave",
    "outbe-tee-enclave.manifest",
    "outbe-tee-enclave.manifest.sgx",
    "outbe-tee-enclave.sig",
)


def stage_signed_enclave(*, config: dict[str, Any], output_dir: Path) -> dict[str, str] | None:
    """Copy the signed enclave into the bundle and report its identity.

    `signed_enclave_dir` points at the directory that holds the artifacts that
    `gramine-sgx-sign` produced on the build host. Without it, the bundle
    carries no enclave, and each machine has to sign its own. That is allowed,
    but it is the very thing that produces mismatched identities, so say so out
    loud.
    """
    source = config.get("signed_enclave_dir")
    if not source:
        return None
    source_dir = Path(str(source))
    staged = output_dir / "enclave"
    staged.mkdir(parents=True, exist_ok=True)
    for name in SIGNED_ENCLAVE_FILES:
        origin = source_dir / name
        if not origin.is_file():
            raise ValueError(
                f"`signed_enclave_dir` is missing {name}. Sign the enclave on the "
                f"build host first (gramine-manifest + gramine-sgx-sign) and point "
                f"this at the directory holding the result."
            )
        shutil.copy2(origin, staged / name)
    # A private key in the bundle would go to every machine. Refuse it.
    for stray in staged.glob("*.pem"):
        stray.unlink()
    return {"path": str(staged)}


# ---------------------------------------------------------------------------
# Public entry point (caddy)
# ---------------------------------------------------------------------------
#
# The node binds RPC to loopback on purpose, so something has to publish it.
# caddy terminates the public listener and reverse-proxies to 127.0.0.1. That
# keeps the node itself unreachable from the internet and gives one place to
# add CORS, TLS or auth later. This mirrors how the live testnet is fronted.


def caddyfile(*, config: dict[str, Any], host: str) -> str:
    """Caddy site for one validator: RPC and the Radicle status endpoint.

    Radicle's replication port is raw p2p, not HTTP, so it stays a plain port
    (already opened between the machines), and this site does not proxy it.
    """
    rpc_port = port_of(config, "rpc_port")
    status_port = port_of(config, "radicle_status_port")
    public_rpc = int(config.get("public_rpc_port", 80))
    public_radicle = int(config.get("public_radicle_status_port", 8080))
    return f"""# Generated for {host}. Plain HTTP on the public address: no TLS,
# because these hosts are addressed by IP and have no certificate names.
{{
	auto_https off
	admin off
}}

# Ethereum JSON-RPC
:{public_rpc} {{
	@options method OPTIONS
	handle @options {{
		header {{
			Access-Control-Allow-Origin *
			Access-Control-Allow-Methods "GET, POST, OPTIONS"
			Access-Control-Allow-Headers "Content-Type"
			Access-Control-Max-Age 86400
		}}
		respond 204
	}}
	handle {{
		reverse_proxy 127.0.0.1:{rpc_port} {{
			header_up X-Real-IP {{remote_host}}
			header_down -Access-Control-Allow-Origin
		}}
		header Access-Control-Allow-Origin *
	}}
}}

# Radicle sidecar status (read-only JSON)
:{public_radicle} {{
	handle {{
		reverse_proxy 127.0.0.1:{status_port} {{
			header_up X-Real-IP {{remote_host}}
		}}
		header Access-Control-Allow-Origin *
	}}
}}
"""


def caddy_install_script(*, config: dict[str, Any], base_dir: str, index: int) -> str:
    public_rpc = int(config.get("public_rpc_port", 80))
    public_radicle = int(config.get("public_radicle_status_port", 8080))
    return f"""
# Publish this validator's RPC and Radicle status through caddy.
if ! command -v caddy >/dev/null; then
  echo "installing caddy..."
  sudo apt-get update -qq
  sudo apt-get install -y -qq debian-keyring debian-archive-keyring apt-transport-https curl >/dev/null
  curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' \\
    | sudo gpg --batch --yes --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
  curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' \\
    | sudo tee /etc/apt/sources.list.d/caddy-stable.list >/dev/null
  sudo apt-get update -qq
  sudo apt-get install -y -qq caddy >/dev/null
fi

sudo install -m 644 {quote(base_dir + f"/validator-{index}/Caddyfile")} /etc/caddy/Caddyfile
sudo systemctl enable --now caddy
sudo systemctl reload caddy 2>/dev/null || sudo systemctl restart caddy

# Let the world reach the published ports; the node's own ports stay loopback.
sudo ufw allow {public_rpc}/tcp >/dev/null 2>&1 || true
sudo ufw allow {public_radicle}/tcp >/dev/null 2>&1 || true

echo "published:"
echo "  RPC            http://$(curl -s -m 5 ifconfig.me || echo '<this-host>'):{public_rpc}"
echo "  Radicle status http://$(curl -s -m 5 ifconfig.me || echo '<this-host>'):{public_radicle}"
"""


# ---------------------------------------------------------------------------
# systemd units
# ---------------------------------------------------------------------------
#
# The run-*.sh scripts each exec one process in the foreground, which is
# exactly what a systemd service wants. Units give us what a shell-launched
# background process cannot. With units, the processes:
#   - survive the session that started them,
#   - restart on failure,
#   - order themselves by dependency,
#   - are inspected with journalctl instead of scattered log files.


UNIT_ROLES = (
    ("enclave", "TEE enclave", None),
    ("radicle", "Radicle sidecar", "outbe-enclave@%i.service"),
    ("node", "validator node", "outbe-radicle@%i.service"),
    ("feeder", "price oracle feeder", "outbe-node@%i.service"),
)


def tee_renew_script(*, config: dict[str, Any], index: int, base_dir: str, keys_dir: str) -> str:
    key_dir = f"{keys_dir}/validator-{index}"
    return f"""
KEY="$(tr -d '[:space:]' < {quote(key_dir + "/evm-key.hex")})"
exec {quote(base_dir + "/outbe-cli")} tee renew \\
  --enclave-socket 127.0.0.1:{port_of(config, "tee_enclave_port")} \\
  --node-data-dir {quote(base_dir + f"/validator-{index}/data")} \\
  --reth-p2p-secret-key {quote(key_dir + "/reth-p2p-secret.hex")} \\
  --private-key "$KEY" \\
  --rpc-url http://127.0.0.1:{port_of(config, "rpc_port")}
"""


def systemd_unit(*, role: str, description: str, after: str | None, base_dir: str) -> str:
    """One templated unit per role. %i is the validator index."""
    dependency_suffix = f" {after}" if after else ""
    user = {"enclave": "root"}.get(role, "ubuntu")
    return f"""[Unit]
Description=Outbe {description} (validator %i)
After=network-online.target{dependency_suffix}
Wants=network-online.target{dependency_suffix}
StartLimitIntervalSec=0

[Service]
Type=simple
User={user}
WorkingDirectory={base_dir}/validator-%i
ExecStart={base_dir}/validator-%i/run-{role}.sh
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
"""


def write_systemd_units(*, output_dir: Path, base_dir: str, enable_tee_renewal: bool) -> None:
    unit_dir = output_dir / "systemd"
    unit_dir.mkdir(parents=True, exist_ok=True)
    for role, description, after in UNIT_ROLES:
        unit = systemd_unit(
            role=role,
            description=description,
            after=after,
            base_dir=base_dir,
        )
        (unit_dir / f"outbe-{role}@.service").write_text(unit)

    (unit_dir / "outbe-tee-renew@.service").write_text(
        f"""[Unit]
Description=Outbe founder TEE lease renewal (validator %i)
After=network-online.target outbe-enclave@%i.service outbe-node@%i.service
Requisite=outbe-enclave@%i.service outbe-node@%i.service

[Service]
Type=oneshot
User=ubuntu
WorkingDirectory={base_dir}/validator-%i
ExecStart={base_dir}/validator-%i/run-tee-renew.sh
TimeoutStartSec=10min
UMask=0077
LimitCORE=0
"""
    )
    (unit_dir / "outbe-tee-renew@.timer").write_text(
        """[Unit]
Description=Daily Outbe founder TEE lease renewal check (validator %i)

[Timer]
OnCalendar=daily
Persistent=true

[Install]
WantedBy=timers.target
"""
    )

    renewal_install = (
        '\nsudo systemctl enable --now "outbe-tee-renew@$INDEX.timer"\n'
        if enable_tee_renewal
        else ""
    )
    install = f"""
# Install and start every Outbe service for validator $1 on this machine.
INDEX="${{1:?usage: install-systemd.sh <validator-index>}}"

sudo install -m 644 {quote(base_dir)}/systemd/outbe-*@.service /etc/systemd/system/
sudo install -m 644 {quote(base_dir)}/systemd/outbe-*@.timer /etc/systemd/system/
sudo systemctl daemon-reload


for role in enclave radicle node feeder; do
  sudo systemctl enable --now "outbe-$role@$INDEX.service"
done
{renewal_install}
echo
echo "services for validator $INDEX:"
systemctl list-units 'outbe-*' --no-pager --no-legend | sed 's/^/  /'
echo
echo "follow one with: journalctl -u outbe-node@$INDEX -f"
"""
    write_script(output_dir / "install-systemd.sh", install)


def preflight_script(*, config: dict[str, Any], base_dir: str, index: int) -> str:
    """Check, on this machine, everything that silently breaks a launch.

    Every item here cost a real debugging session: a genesis that differs
    between machines, a mismatched enclave identity, leftover state from an
    earlier genesis, and ports already held by a previous run.
    """
    tee_port = port_of(config, "tee_enclave_port")
    rpc_port = port_of(config, "rpc_port")
    return f"""
# Read-only pre-launch check for validator {index}. Exits non-zero on anything
# that would make the network fail after start rather than before.
cd {quote(base_dir)}
fail=0
note() {{ printf '  %-42s %s\\n' "$1" "$2"; }}

note "genesis sha256" "$(sha256sum genesis.json | cut -c1-16)"
echo "  ^ genesis must be identical on every machine"

if [ -f outbe-tee-enclave.sig ]; then
  note "enclave mr_enclave" "$(gramine-sgx-sigstruct-view outbe-tee-enclave.sig 2>/dev/null | grep -oE 'mr_enclave: [0-9a-f]+' | cut -c13-28)"
  note "enclave mr_signer" "$(gramine-sgx-sigstruct-view outbe-tee-enclave.sig 2>/dev/null | grep -oE 'mr_signer: [0-9a-f]+' | cut -c12-27)"
  echo "  ^ mr_signer must match across machines; a per-host key breaks dcap-required"
else
  note "enclave signature" "MISSING"; fail=1
fi

# State from an earlier genesis belongs to a different chain; the node refuses to start on it.
for stale in validator-{index}/data validator-{index}/consensus tee; do
  [ -e "$stale" ] && {{ note "leftover state" "$stale - remove before a new genesis"; fail=1; }}
done

for port in {tee_port} {rpc_port}; do
  ss -ltn 2>/dev/null | grep -q ":$port " && {{ note "port $port" "already in use"; fail=1; }}
done

if [ "$fail" = 0 ]; then echo; echo "  preflight OK"; else echo; echo "  preflight FAILED"; fi
exit "$fail"
"""


# ---------------------------------------------------------------------------
# Per-machine distribution
# ---------------------------------------------------------------------------
#
# The point of the bundle is that nobody assembles anything by hand afterwards.
# Each machine gets one archive that holds everything it needs and nothing that
# belongs to another validator. Each machine also gets a checksum manifest.
# Thus the manifest catches a half-finished copy before the network starts,
# not after.


def _pack_validator_archive(
    *, output_dir: Path, dist: Path, keys_dir: Path, index: int
) -> str:
    staging = dist / f"validator-{index}"
    staging.mkdir()
    for name in ("genesis.json", "reth-bootnodes.txt", "DEPLOY.md", "install-systemd.sh"):
        item = output_dir / name
        if item.is_file():
            shutil.copy2(item, staging / item.name)
    for folder in ("systemd", "enclave"):
        src = output_dir / folder
        if src.is_dir():
            shutil.copytree(src, staging / folder)
    # This machine's run scripts and ONLY this machine's key material.
    shutil.copytree(output_dir / f"validator-{index}", staging / f"validator-{index}")
    keys_target = staging / "keys" / f"validator-{index}"
    keys_target.parent.mkdir(exist_ok=True)
    shutil.copytree(keys_dir / f"validator-{index}", keys_target)

    archive = dist / f"validator-{index}.tgz"
    with tarfile.open(archive, "w:gz") as tar:
        for entry in sorted(staging.iterdir()):
            tar.add(entry, arcname=entry.name)
    shutil.rmtree(staging)
    return archive.name


def build_distribution(
    *,
    output_dir: Path,
    validators: list[dict[str, Any]],
    keys_dir: Path,
    base_dir: str,
) -> list[str]:
    """Pack one self-contained archive per machine. Returns their names."""
    dist = output_dir / "dist"
    shutil.rmtree(dist, ignore_errors=True)
    dist.mkdir(parents=True)

    names = [
        _pack_validator_archive(
            output_dir=output_dir, dist=dist, keys_dir=keys_dir, index=index
        )
        for index in range(len(validators))
    ]

    # One manifest over the archives. A truncated copy or a stale archive from
    # an earlier run is then a checksum mismatch, not a mystery at boot.
    lines = [
        f"{hashlib.sha256((dist / name).read_bytes()).hexdigest()}  {name}"
        for name in names
    ]
    (dist / "SHA256SUMS").write_text("\n".join(lines) + "\n")

    unpack = f"""
# Unpack this machine's archive into {base_dir}. Run it ON the target machine,
# from the directory holding validator-<index>.tgz.
INDEX="${{1:?usage: unpack.sh <validator-index>}}"
ARCHIVE="validator-$INDEX.tgz"

[ -f "$ARCHIVE" ] || {{ echo "no $ARCHIVE here" >&2; exit 1; }}
if command -v sha256sum >/dev/null && [ -f SHA256SUMS ]; then
  grep " $ARCHIVE\\$" SHA256SUMS | sha256sum -c - || {{ echo "checksum mismatch" >&2; exit 1; }}
fi

sudo mkdir -p {quote(base_dir)}
sudo chown "$USER" {quote(base_dir)}
tar xzf "$ARCHIVE" -C {quote(base_dir)}
chmod -R go-rwx {quote(base_dir)}/keys

# The signed enclave, when the bundle carries one, belongs next to the binaries.
if [ -d {quote(base_dir)}/enclave ]; then
  sudo install -m 755 {quote(base_dir)}/enclave/outbe-tee-enclave {quote(base_dir)}/
  sudo install -m 644 {quote(base_dir)}/enclave/outbe-tee-enclave.manifest* {quote(base_dir)}/
  sudo install -m 644 {quote(base_dir)}/enclave/outbe-tee-enclave.sig {quote(base_dir)}/
fi

echo "unpacked into {base_dir}; next: ./install-systemd.sh $INDEX"
"""
    write_script(dist / "unpack.sh", unpack)
    return names


# ---------------------------------------------------------------------------
# DEPLOY.md
# ---------------------------------------------------------------------------


def deploy_markdown(
    *,
    config: dict[str, Any],
    validators: list[dict[str, Any]],
    keys_dir: Path,
    enodes: list[str],
) -> str:
    base_dir = str(config.get("remote_base_dir", keys_dir.parent))
    remote_keys_dir = str(config.get("remote_keys_dir", keys_dir))
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
ssh <machine-N> "mkdir -p {base_dir} {remote_keys_dir}"
scp genesis.json reth-bootnodes.txt <machine-N>:{base_dir}/
scp -r validator-N <machine-N>:{base_dir}/
scp -r {keys_dir}/validator-N <machine-N>:{remote_keys_dir}/
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
`{port_of(config, "tee_enclave_port")}` and the feeder health endpoint
`{port_of(config, "feeder_health_port")}`.

## 3. Prerequisites on every machine

- Python 3.11+ - reads the shared storage TOML at launch
- Docker - the TEE enclave runs as a container
- `outbe-cli` on every SGX validator host for automatic TEE renewal
- the `outbe-chain`, `outbe-radicle` and `outbe-feeder` binaries on `PATH`
  (or set `node_binary`, `radicle_binary` and `feeder_binary` in the yaml to
  absolute paths and regenerate)
- for `dcap-required`: SGX hardware exposing `/dev/sgx_enclave` and
  `/dev/sgx_provision`

## 4. Start each machine

```bash
cd {base_dir}/validator-N
./preflight.sh N        # verifies genesis/enclave/state before anything starts
sudo {base_dir}/install-systemd.sh N
```

`install-systemd.sh` installs one templated unit per role, starts them in
dependency order, and enables the daily TEE renewal timer for a NodeHost SGX
session. The services recover after clean or failed exits. `preflight.sh` is
read-only: run it first and compare the genesis and enclave digests it prints
across all four machines - they must be identical.

`start-all.sh` starts the components in dependency order - configured storage, enclave,
Radicle sidecar, node, feeder - and writes pids and logs into
`{base_dir}/validator-N/`. To run one component in the foreground instead, call
its script directly: `run-enclave.sh`, `run-radicle.sh`,
`run-node.sh`, `run-feeder.sh`. `stop-all.sh` reverses it.

**Start all four machines within a few minutes of each other.** Block 1 carries
the founding DKG ceremony and needs every genesis validator online - unlike a
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
| TEE enclave | Docker container | the node refuses to start without it |
| Radicle | `outbe-radicle` | validator startup requires its control socket |
| Price feeder | `outbe-feeder` | submits oracle votes |

## Storage configuration

Tribute, Nod and Gem records, buckets and indexes live in the node's EVM state.
Metadosis reads Tribute from state, executes confidential Lysis in the enclave,
and saves the ready encrypted Nod records. No separate entity datasource is configured.

## Notes

- `reth-bootnodes.txt` pins each machine's reth identity:
{bootnode_lines}
- Re-running `create_genesis.py` without the pinned `timestamp:` produces a
  different genesis hash. Keep the
  timestamp that is now in your yaml.
"""


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def _write_validator_scripts(
    *,
    config: dict[str, Any],
    index: int,
    validator: dict[str, Any],
    paths: tuple[Path, Path, str, str],
) -> None:
    output_dir, keys_dir, base_dir, remote_keys_dir = paths
    directory = output_dir / f"validator-{index}"
    host, _, _ = validator["p2p_address"].rpartition(":")

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
        ),
    )
    write_script(
        directory / "run-node.sh",
        node_script(
            config=config | {"remote_keys_dir": remote_keys_dir},
            index=index,
            base_dir=base_dir,
            p2p_address=validator["p2p_address"],
        ),
    )
    write_script(
        directory / "run-feeder.sh",
        feeder_script(config=config, index=index, base_dir=base_dir),
    )
    (directory / "Caddyfile").write_text(caddyfile(config=config, host=host))
    write_script(
        directory / "run-tee-renew.sh",
        tee_renew_script(
            config=config,
            index=index,
            base_dir=base_dir,
            keys_dir=remote_keys_dir,
        ),
    )
    write_script(
        directory / "preflight.sh",
        preflight_script(
            config=config,
            base_dir=base_dir,
            index=index,
        ),
    )
    write_script(
        directory / "install-caddy.sh",
        caddy_install_script(config=config, base_dir=base_dir, index=index),
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
    (directory / "pairs.toml").write_text(feeder_pairs(config=config))
    write_script(
        directory / "start-all.sh",
        start_all_script(
            config=config,
            index=index,
            base_dir=base_dir,
        ).replace("{TEE_PORT}", str(port_of(config, "tee_enclave_port"))),
    )
    write_script(directory / "stop-all.sh", stop_all_script(index=index, base_dir=base_dir))


def render(
    *,
    config: dict[str, Any],
    validators: list[dict[str, Any]],
    genesis_path: Path,
    keys_dir: Path,
) -> None:
    output_dir = genesis_path.parent
    # Paths as they will exist on the target machines. They default to this
    # workstation's layout so a single-host trial run works unchanged.
    base_dir = str(config.get("remote_base_dir", output_dir))
    remote_keys_dir = str(config.get("remote_keys_dir", keys_dir))

    enodes = [
        f"enode://{ensure_reth_p2p_secret(keys_dir / f'validator-{index}')}@{format_enode_host(validator['p2p_address'].rsplit(':', 1)[0])}:{port_of(config, 'reth_p2p_port')}"
        for index, validator in enumerate(validators)
    ]
    (output_dir / "reth-bootnodes.txt").write_text("\n".join(enodes) + "\n")
    paths = (output_dir, keys_dir, base_dir, remote_keys_dir)
    for index, validator in enumerate(validators):
        _write_validator_scripts(
            config=config,
            index=index,
            validator=validator,
            paths=paths,
        )

    stage_signed_enclave(config=config, output_dir=output_dir)
    write_systemd_units(
        output_dir=output_dir,
        base_dir=base_dir,
        enable_tee_renewal=_uses_production_tee_session(config),
    )

    build_distribution(
        output_dir=output_dir,
        validators=validators,
        keys_dir=keys_dir,
        base_dir=base_dir,
    )

    (output_dir / "DEPLOY.md").write_text(
        deploy_markdown(
            config=config | {"remote_base_dir": base_dir},
            validators=validators,
            keys_dir=keys_dir,
            enodes=enodes,
        )
    )
