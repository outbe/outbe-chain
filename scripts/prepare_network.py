#!/usr/bin/env python3
"""
Prepare an Outbe network from an existing validators.json.

Inputs:
  - genesis.base.json: base chain config and initial alloc
  - seed-testnet.json or equivalent runtime seed config
  - validators.json: genesis validator public keys, EVM addresses, and consensus p2p addresses

Outputs:
  - genesis.json: seeded chain state with ValidatorSet/Staking/Rewards/precompile storage
  - reth-bootnodes.txt: stable Reth enodes for --bootnodes
  - validator-N/evm-key.hex: validator EVM key material when present in validators.json
  - validator-N/reth-p2p-secret.hex: stable Reth p2p node identity keys
  - commands/validator-N.sh: runnable node command per validator
  - network.md: human-readable launch plan with addresses, ports, and commands

This script does not create or consume the removed runtime --consensus.validators flag.
validators.json is a genesis/tooling input only.
"""

from __future__ import annotations

import argparse
import json
import os
import secrets
import stat
import subprocess
import sys
import time
from pathlib import Path
from typing import Any


SECP256K1_P = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F
SECP256K1_N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
SECP256K1_G = (
    0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798,
    0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8,
)

DEFAULT_PREFUND_WEI = 10_000 * 10**18
DEFAULT_CHAIN_ID = 54322345
DEFAULT_EPOCH_LENGTH_BLOCKS = 120
DEFAULT_DKG_PREPARE_WINDOW_BLOCKS = 30
DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS = 30
DEFAULT_GAS_LIMIT = "0x1c9c380"


def load_json(path: Path) -> Any:
    with path.open() as f:
        return json.load(f)


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w") as f:
        json.dump(value, f, indent=2)
        f.write("\n")


PRIVATE_VALIDATOR_FIELDS = {
    "private_key",
    "evm_private_key",
    "ecdsa_private_key",
    "evm_key",
    "reth_p2p_secret_hex",
    "reth_p2p_secret",
}


def sanitized_validators_for_output(validators: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [
        {key: value for key, value in validator.items() if key not in PRIVATE_VALIDATOR_FIELDS}
        for validator in validators
    ]


def default_base_genesis(
    *,
    chain_id: int,
    epoch_length_blocks: int,
    dkg_prepare_window_blocks: int,
    dkg_activation_grace_blocks: int,
    gas_limit: str,
) -> dict[str, Any]:
    return {
        "config": {
            "chainId": chain_id,
            "homesteadBlock": 0,
            "eip150Block": 0,
            "eip155Block": 0,
            "eip158Block": 0,
            "byzantiumBlock": 0,
            "constantinopleBlock": 0,
            "petersburgBlock": 0,
            "istanbulBlock": 0,
            "berlinBlock": 0,
            "londonBlock": 0,
            "mergeNetsplitBlock": 0,
            "terminalTotalDifficulty": 0,
            "terminalTotalDifficultyPassed": True,
            "shanghaiTime": 0,
            "cancunTime": 0,
            "pragueTime": 0,
            "epochLengthBlocks": epoch_length_blocks,
            "dkgPrepareWindowBlocks": dkg_prepare_window_blocks,
            "dkgActivationGraceBlocks": dkg_activation_grace_blocks,
        },
        "nonce": "0x0",
        "timestamp": hex(int(time.time())),
        "extraData": "0x",
        "gasLimit": gas_limit,
        "difficulty": "0x0",
        "mixHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
        "coinbase": "0x0000000000000000000000000000000000000000",
        "alloc": {},
    }


def normalize_hex(value: str, *, expected_len: int | None = None, field: str = "hex") -> str:
    raw = value.strip().lower()
    if raw.startswith("0x"):
        raw = raw[2:]
    if expected_len is not None and len(raw) != expected_len:
        raise ValueError(f"{field} must be {expected_len} hex chars, got {len(raw)}")
    try:
        bytes.fromhex(raw)
    except ValueError as exc:
        raise ValueError(f"{field} is not valid hex") from exc
    return raw


def parse_host_port(value: str) -> tuple[str, int]:
    if value.startswith("["):
        end = value.find("]")
        if end < 0 or end + 1 >= len(value) or value[end + 1] != ":":
            raise ValueError(f"invalid host:port: {value}")
        host = value[1:end]
        port_s = value[end + 2 :]
    else:
        if ":" not in value:
            raise ValueError(f"invalid host:port: {value}")
        host, port_s = value.rsplit(":", 1)
    if not host:
        raise ValueError(f"missing host in host:port: {value}")
    port = int(port_s)
    if port <= 0 or port > 65535:
        raise ValueError(f"invalid port in host:port: {value}")
    return host, port


def format_enode_host(host: str) -> str:
    if ":" in host and not (host.startswith("[") and host.endswith("]")):
        return f"[{host}]"
    return host


def shell_quote(value: str) -> str:
    return "'" + value.replace("'", "'\"'\"'") + "'"


def compact_hex_balance(amount: int) -> str:
    if amount < 0:
        raise ValueError("balance cannot be negative")
    return hex(amount)


def point_add(
    p1: tuple[int, int] | None, p2: tuple[int, int] | None
) -> tuple[int, int] | None:
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


def point_mul(scalar: int, point: tuple[int, int]) -> tuple[int, int]:
    if scalar <= 0 or scalar >= SECP256K1_N:
        raise ValueError("secp256k1 scalar out of range")
    result: tuple[int, int] | None = None
    addend: tuple[int, int] | None = point
    k = scalar
    while k:
        if k & 1:
            result = point_add(result, addend)
        addend = point_add(addend, addend)
        k >>= 1
    if result is None:
        raise ValueError("invalid secp256k1 scalar produced point at infinity")
    return result


def generate_reth_secret_hex() -> str:
    while True:
        scalar = int.from_bytes(secrets.token_bytes(32), "big")
        if 1 <= scalar < SECP256K1_N:
            return f"{scalar:064x}"


def reth_node_id_from_secret(secret_hex: str) -> str:
    raw = normalize_hex(secret_hex, expected_len=64, field="reth p2p secret")
    scalar = int(raw, 16)
    x, y = point_mul(scalar, SECP256K1_G)
    return f"{x:064x}{y:064x}"


def validator_field(validator: dict[str, Any], names: list[str]) -> str | None:
    for name in names:
        value = validator.get(name)
        if isinstance(value, str) and value.strip():
            return value.strip()
    return None


def validator_reth_address(
    validator: dict[str, Any],
    index: int,
    *,
    reth_p2p_base_port: int,
) -> str:
    explicit = validator_field(validator, ["reth_p2p_address", "reth_address", "reth"])
    if explicit is not None:
        parse_host_port(explicit)
        return explicit
    consensus_address = validator_field(validator, ["p2p_address"])
    if consensus_address is None:
        raise ValueError(f"validator {index} missing required p2p_address")
    host, _ = parse_host_port(consensus_address)
    return f"{host}:{reth_p2p_base_port + index}"


def validator_consensus_address(validator: dict[str, Any], index: int) -> str:
    value = validator_field(validator, ["p2p_address"])
    if value is None:
        raise ValueError(f"validator {index} missing required p2p_address")
    parse_host_port(value)
    return value


def validator_signing_key_path(
    validator: dict[str, Any],
    index: int,
    *,
    runtime_base_dir: str,
) -> str:
    explicit = validator_field(validator, ["signing_key_path", "bls_signing_key_path"])
    if explicit is not None:
        return explicit
    return f"{runtime_base_dir}/validator-{index}/signing-key.hex"


def validator_wallet_info(validator: dict[str, Any]) -> tuple[str, str | None]:
    address = validator_field(validator, ["address"])
    if address is None:
        raise ValueError("validator missing address")
    private_key = validator_field(
        validator,
        ["private_key", "evm_private_key", "ecdsa_private_key", "evm_key"],
    )
    return address, private_key


def parse_hosts(args: argparse.Namespace, expected: int) -> list[str]:
    values: list[str] = []
    if args.validator_hosts:
        values.extend(
            host.strip()
            for host in args.validator_hosts.split(",")
            if host.strip()
        )
    if args.validator_hosts_file:
        values.extend(
            line.strip()
            for line in args.validator_hosts_file.read_text().splitlines()
            if line.strip() and not line.lstrip().startswith("#")
        )
    if not values:
        raise ValueError("--generate-validators requires --validator-hosts or --validator-hosts-file")
    for value in values:
        if ":" in value:
            raise ValueError(
                "validator hosts must be hosts/IPs only, without ports; ports are assigned by the script"
            )
    if len(values) == 1:
        return values * expected
    if len(values) != expected:
        raise ValueError(
            f"expected {expected} validator hosts, got {len(values)}"
        )
    return values


def run_dkg_bootstrap(
    *,
    chain_binary: str,
    output_dir: Path,
    count: int,
) -> None:
    cmd = [
        chain_binary,
        "dkg",
        "bootstrap",
        "--output-dir",
        str(output_dir),
        "--validators",
        str(count),
    ]
    subprocess.run(cmd, check=True)


def update_generated_validators(
    *,
    validators_path: Path,
    hosts: list[str],
    consensus_p2p_base_port: int,
    reth_p2p_base_port: int,
) -> list[dict[str, Any]]:
    validators = load_json(validators_path)
    if not isinstance(validators, list):
        raise ValueError("generated validators.json must contain a JSON array")
    if len(validators) != len(hosts):
        raise ValueError(
            f"generated validator count {len(validators)} does not match host count {len(hosts)}"
        )
    for index, validator in enumerate(validators):
        if not isinstance(validator, dict):
            raise ValueError(f"generated validator {index} must be an object")
        host = hosts[index]
        validator["p2p_address"] = f"{host}:{consensus_p2p_base_port}"
        validator["reth_p2p_address"] = f"{host}:{reth_p2p_base_port}"
    write_json(validators_path, validators)
    return validators


def generated_wallet_private_keys(output_dir: Path, count: int) -> dict[int, str]:
    keys: dict[int, str] = {}
    for index in range(count):
        path = output_dir / f"validator-{index}" / "evm-key.hex"
        if path.exists():
            raw = normalize_hex(path.read_text(), expected_len=64, field=f"validator {index} evm key")
            keys[index] = "0x" + raw
    return keys


def prepare_prefunded_genesis(
    base_genesis: dict[str, Any],
    validators: list[dict[str, Any]],
    *,
    prefund_wei: int,
) -> dict[str, Any]:
    genesis = json.loads(json.dumps(base_genesis))
    alloc = genesis.setdefault("alloc", {})
    if not isinstance(alloc, dict):
        raise ValueError("genesis alloc must be an object")
    if prefund_wei == 0:
        return genesis
    for validator in validators:
        address, _ = validator_wallet_info(validator)
        key = normalize_hex(address, expected_len=40, field="validator address")
        entry = alloc.setdefault(key, {})
        entry.setdefault("balance", compact_hex_balance(prefund_wei))
    return genesis


def run_seed_genesis(
    *,
    repo_root: Path,
    preseed_genesis: Path,
    seed: Path,
    validators: Path,
    output_genesis: Path,
) -> None:
    cmd = [
        sys.executable,
        str(repo_root / "scripts" / "seed_genesis.py"),
        "--genesis",
        str(preseed_genesis),
        "--seed",
        str(seed),
        "--validators",
        str(validators),
        "--output",
        str(output_genesis),
    ]
    subprocess.run(cmd, check=True)


def command_lines(
    *,
    chain_binary: str,
    genesis_runtime_path: str,
    datadir: str,
    rpc_host: str,
    rpc_port: int,
    reth_p2p_port: int,
    discv5_host: str,
    discv5_port: int,
    bootnodes_runtime_path: str,
    p2p_secret_runtime_path: str,
    authrpc_port: int,
    ipc_path: str,
    metrics_host: str,
    metrics_port: int,
    log_dir: str,
    signing_key_path: str,
    evm_key_path: str,
    consensus_listen_host: str,
    consensus_listen_port: int,
    use_local_defaults: bool,
) -> list[str]:
    lines = [
        f"{chain_binary} node \\",
        "  --validator \\",
        f"  --chain {shell_quote(genesis_runtime_path)} \\",
        f"  --datadir {shell_quote(datadir)} \\",
        f"  --http --http.addr {rpc_host} --http.port {rpc_port} \\",
        "  --http.api eth,net,web3,outbe \\",
        f"  --port {reth_p2p_port} \\",
        f"  --discovery.port {reth_p2p_port} \\",
        f"  --discovery.v5.addr {discv5_host} \\",
        f"  --discovery.v5.port {discv5_port} \\",
        f"  --bootnodes \"$(grep -v '^[[:space:]]*#' {shell_quote(bootnodes_runtime_path)} | paste -sd, -)\" \\",
        f"  --p2p-secret-key-hex \"$(tr -d '[:space:]' < {shell_quote(p2p_secret_runtime_path)})\" \\",
        f"  --authrpc.port {authrpc_port} \\",
        f"  --ipcpath {shell_quote(ipc_path)} \\",
        f"  --metrics {metrics_host}:{metrics_port} \\",
        f"  --log.file.directory {shell_quote(log_dir)} \\",
        f"  --consensus.signing-key {shell_quote(signing_key_path)} \\",
        f"  --validator.evm-key {shell_quote(evm_key_path)} \\",
        f"  --consensus.listen-addr {consensus_listen_host}:{consensus_listen_port}",
    ]
    if use_local_defaults:
        lines[-1] += " \\"
        lines.append("  --consensus.use-local-defaults")
    return lines


def write_command_script(path: Path, lines: list[str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    content = "#!/usr/bin/env bash\nset -euo pipefail\n\n" + "\n".join(lines) + "\n"
    path.write_text(content)
    mode = path.stat().st_mode
    path.chmod(mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def write_secret_hex(path: Path, hex_value: str) -> None:
    tmp_path = path.with_name(f".{path.name}.tmp.{os.getpid()}")
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    fd = os.open(tmp_path, flags, stat.S_IRUSR | stat.S_IWUSR)
    try:
        with os.fdopen(fd, "w") as handle:
            handle.write(hex_value + "\n")
        os.chmod(tmp_path, stat.S_IRUSR | stat.S_IWUSR)
        os.replace(tmp_path, path)
    except Exception:
        try:
            os.unlink(tmp_path)
        except FileNotFoundError:
            pass
        raise


def build_network_markdown(
    *,
    validators: list[dict[str, Any]],
    genesis_path: Path,
    copied_validators_path: Path,
    bootnodes_path: Path,
    commands: list[tuple[int, str, list[str]]],
    reth_rows: list[dict[str, Any]],
    runtime_base_dir: str,
    include_private_keys: bool,
    wallet_private_keys: dict[int, str] | None = None,
) -> str:
    wallet_private_keys = wallet_private_keys or {}
    lines: list[str] = []
    lines.append("# Outbe Network Launch Plan")
    lines.append("")
    lines.append("Generated from an existing `validators.json`.")
    lines.append("")
    lines.append("## Artifacts")
    lines.append("")
    lines.append(f"- Genesis: `{genesis_path}`")
    lines.append(f"- Validators input copy: `{copied_validators_path}`")
    lines.append(f"- Reth bootnodes: `{bootnodes_path}`")
    lines.append(f"- Runtime base dir used in commands: `{runtime_base_dir}`")
    lines.append("")
    lines.append("`validators.json` is a genesis/tooling input only. Do not pass it to node runtime; `--consensus.validators` is removed.")
    lines.append("")
    lines.append("## Bootnodes")
    lines.append("")
    lines.append("```text")
    lines.extend(row["enode"] for row in reth_rows)
    lines.append("```")
    lines.append("")
    lines.append("## Validators")
    lines.append("")
    header = "| # | EVM address | BLS public key | Consensus P2P | Reth P2P | RPC | Metrics |"
    sep = "|---:|---|---|---|---|---|---|"
    lines.extend([header, sep])
    for row in reth_rows:
        pk = row["public_key"]
        short_pk = f"`{pk[:12]}...{pk[-12:]}`"
        lines.append(
            f"| {row['index']} | `{row['address']}` | {short_pk} | "
            f"`{row['consensus_p2p']}` | `{row['reth_p2p']}` | "
            f"`http://{row['host']}:{row['rpc_port']}` | `{row['host']}:{row['metrics_port']}` |"
        )
    lines.append("")
    lines.append("## Wallets")
    lines.append("")
    lines.append("| # | Address | Private key |")
    lines.append("|---:|---|---|")
    for index, validator in enumerate(validators):
        address, private_key = validator_wallet_info(validator)
        private_key = private_key or wallet_private_keys.get(index)
        if include_private_keys and private_key:
            key_cell = f"`{private_key}`"
        elif private_key:
            key_cell = "`present in generated/input key material; redacted`"
        else:
            key_cell = "not included; use the operator-owned key for this address"
        lines.append(f"| {index} | `{address}` | {key_cell} |")
    lines.append("")
    lines.append("## Per-Validator Commands")
    lines.append("")
    lines.append("Copy the generated `genesis.json`, `reth-bootnodes.txt`, each validator's `signing-key.hex`, `evm-key.hex`, and `reth-p2p-secret.hex` to the paths used below. For existing-validator inputs, `evm-key.hex` is materialized only when the validator JSON includes the matching private key or an operator-owned `validator-N/evm-key.hex` already exists; otherwise provide the operator-owned key for the listed validator address before launch.")
    lines.append("")
    for index, script_path, cmd in commands:
        lines.append(f"### Validator {index}")
        lines.append("")
        lines.append(f"Script: `{script_path}`")
        lines.append("")
        lines.append("```bash")
        lines.extend(cmd)
        lines.append("```")
        lines.append("")
    lines.append("## Checks")
    lines.append("")
    lines.append("```bash")
    lines.append("curl -sS -H 'content-type: application/json' \\")
    lines.append("  --data '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_blockNumber\",\"params\":[]}' \\")
    lines.append("  http://<rpc-host>:<rpc-port>")
    lines.append("")
    lines.append("curl -sS -H 'content-type: application/json' \\")
    lines.append("  --data '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"net_peerCount\",\"params\":[]}' \\")
    lines.append("  http://<rpc-host>:<rpc-port>")
    lines.append("")
    lines.append("curl -sS http://<metrics-host>:<metrics-port>/metrics | rg 'outbe_reshares_completed_total|commonware_p2p_active_peers|outbe_consensus_reth_tip_hash_match|outbe_parent_cert_store_size'")
    lines.append("```")
    lines.append("")
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser(description="Prepare Outbe genesis and launch plan from validators.json")
    parser.add_argument("--genesis-base", type=Path)
    parser.add_argument("--seed", required=True, type=Path)
    parser.add_argument("--validators", type=Path)
    parser.add_argument("--generate-validators", type=int, help="Generate DKG/key material for N validators before preparing the network")
    parser.add_argument("--validator-hosts", help="Comma-separated validator hosts/IPs used with --generate-validators")
    parser.add_argument("--validator-hosts-file", type=Path, help="File with one validator host/IP per line")
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--chain-binary", default="outbe-chain")
    parser.add_argument("--runtime-base-dir", help="Path prefix used in generated commands; defaults to output dir")
    parser.add_argument("--prefund-wei", type=int, default=DEFAULT_PREFUND_WEI)
    parser.add_argument("--chain-id", type=int, default=DEFAULT_CHAIN_ID)
    parser.add_argument("--epoch-length-blocks", type=int, default=DEFAULT_EPOCH_LENGTH_BLOCKS)
    parser.add_argument("--dkg-prepare-window-blocks", type=int, default=DEFAULT_DKG_PREPARE_WINDOW_BLOCKS)
    parser.add_argument("--dkg-activation-grace-blocks", type=int, default=DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS)
    parser.add_argument("--gas-limit", default=DEFAULT_GAS_LIMIT)
    parser.add_argument("--consensus-p2p-base-port", type=int, default=30400)
    parser.add_argument("--reth-p2p-base-port", type=int, default=30303)
    parser.add_argument("--reth-discv5-base-port", type=int, default=31303)
    parser.add_argument("--rpc-base-port", type=int, default=8545)
    parser.add_argument("--authrpc-base-port", type=int, default=8551)
    parser.add_argument("--metrics-base-port", type=int, default=9101)
    parser.add_argument("--http-addr", default="0.0.0.0")
    parser.add_argument("--metrics-addr", default="0.0.0.0")
    parser.add_argument("--discv5-addr", default="0.0.0.0")
    parser.add_argument("--consensus-listen-host", default="0.0.0.0")
    parser.add_argument("--use-local-defaults", action="store_true")
    parser.add_argument("--include-private-keys", action="store_true")
    parser.add_argument("--force-reth-secrets", action="store_true")
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parents[1]
    output_dir = args.output_dir
    runtime_base_dir = args.runtime_base_dir or str(output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)

    if args.validators and args.generate_validators:
        raise ValueError("use either --validators or --generate-validators, not both")
    if not args.validators and not args.generate_validators:
        raise ValueError("provide --validators or --generate-validators")

    wallet_private_keys: dict[int, str] = {}
    validators_path: Path
    if args.generate_validators:
        if args.generate_validators <= 0:
            raise ValueError("--generate-validators must be > 0")
        hosts = parse_hosts(args, args.generate_validators)
        run_dkg_bootstrap(
            chain_binary=args.chain_binary,
            output_dir=output_dir,
            count=args.generate_validators,
        )
        validators_path = output_dir / "validators.json"
        update_generated_validators(
            validators_path=validators_path,
            hosts=hosts,
            consensus_p2p_base_port=args.consensus_p2p_base_port,
            reth_p2p_base_port=args.reth_p2p_base_port,
        )
        wallet_private_keys = generated_wallet_private_keys(
            output_dir,
            args.generate_validators,
        )
    else:
        validators_path = args.validators

    validators_raw = load_json(validators_path)
    if not isinstance(validators_raw, list) or not validators_raw:
        raise ValueError("validators.json must contain a non-empty JSON array")
    validators: list[dict[str, Any]] = validators_raw

    for index, validator in enumerate(validators):
        if not isinstance(validator, dict):
            raise ValueError(f"validator {index} must be an object")
        normalize_hex(validator_field(validator, ["public_key"]) or "", expected_len=96, field=f"validator {index} public_key")
        normalize_hex(validator_field(validator, ["address"]) or "", expected_len=40, field=f"validator {index} address")
        validator_consensus_address(validator, index)

    copied_validators_path = output_dir / "validators.json"
    sanitized_validators = sanitized_validators_for_output(validators)
    if validators_path.resolve() != copied_validators_path.resolve():
        write_json(copied_validators_path, sanitized_validators)
    elif sanitized_validators != validators:
        copied_validators_path = output_dir / "validators.public.json"
        write_json(copied_validators_path, sanitized_validators)

    if args.genesis_base:
        base_genesis = load_json(args.genesis_base)
    else:
        base_genesis = default_base_genesis(
            chain_id=args.chain_id,
            epoch_length_blocks=args.epoch_length_blocks,
            dkg_prepare_window_blocks=args.dkg_prepare_window_blocks,
            dkg_activation_grace_blocks=args.dkg_activation_grace_blocks,
            gas_limit=args.gas_limit,
        )
        write_json(output_dir / "genesis.base.json", base_genesis)
    preseed_genesis = prepare_prefunded_genesis(
        base_genesis,
        validators,
        prefund_wei=args.prefund_wei,
    )
    preseed_path = output_dir / "genesis.prefund.json"
    genesis_path = output_dir / "genesis.json"
    write_json(preseed_path, preseed_genesis)

    run_seed_genesis(
        repo_root=repo_root,
        preseed_genesis=preseed_path,
        seed=args.seed,
        validators=copied_validators_path,
        output_genesis=genesis_path,
    )

    bootnodes: list[str] = []
    rows: list[dict[str, Any]] = []
    commands: list[tuple[int, str, list[str]]] = []
    commands_dir = output_dir / "commands"

    for index, validator in enumerate(validators):
        consensus_p2p = validator_consensus_address(validator, index)
        host, consensus_port = parse_host_port(consensus_p2p)
        reth_p2p = validator_reth_address(
            validator,
            index,
            reth_p2p_base_port=args.reth_p2p_base_port,
        )
        reth_host, reth_port = parse_host_port(reth_p2p)
        validator_dir = output_dir / f"validator-{index}"
        validator_dir.mkdir(parents=True, exist_ok=True)

        secret_path = validator_dir / "reth-p2p-secret.hex"
        secret_from_json = validator_field(
            validator,
            ["reth_p2p_secret_hex", "reth_p2p_secret"],
        )
        if secret_from_json is not None:
            secret_hex = normalize_hex(secret_from_json, expected_len=64, field=f"validator {index} reth p2p secret")
            write_secret_hex(secret_path, secret_hex)
        elif secret_path.exists() and not args.force_reth_secrets:
            secret_hex = normalize_hex(secret_path.read_text(), expected_len=64, field=f"validator {index} existing reth p2p secret")
            write_secret_hex(secret_path, secret_hex)
        else:
            secret_hex = generate_reth_secret_hex()
            write_secret_hex(secret_path, secret_hex)

        node_id = reth_node_id_from_secret(secret_hex)
        enode = f"enode://{node_id}@{format_enode_host(reth_host)}:{reth_port}"
        bootnodes.append(enode)

        address, private_key = validator_wallet_info(validator)
        evm_key_path = validator_dir / "evm-key.hex"
        if private_key:
            private_key_hex = normalize_hex(private_key, expected_len=64, field=f"validator {index} evm private_key")
            write_secret_hex(evm_key_path, private_key_hex)
        elif evm_key_path.exists():
            private_key_hex = normalize_hex(evm_key_path.read_text(), expected_len=64, field=f"validator {index} existing evm key")
            write_secret_hex(evm_key_path, private_key_hex)
        public_key = normalize_hex(validator["public_key"], expected_len=96, field=f"validator {index} public_key")
        rpc_port = int(validator.get("rpc_port", args.rpc_base_port))
        authrpc_port = int(validator.get("authrpc_port", args.authrpc_base_port))
        metrics_port = int(validator.get("metrics_port", args.metrics_base_port))
        discv5_port = int(validator.get("reth_discv5_port", args.reth_discv5_base_port))
        signing_key_path = validator_signing_key_path(
            validator,
            index,
            runtime_base_dir=runtime_base_dir,
        )

        runtime_validator_dir = f"{runtime_base_dir}/validator-{index}"
        runtime_secret_path = f"{runtime_validator_dir}/reth-p2p-secret.hex"
        runtime_evm_key_path = f"{runtime_validator_dir}/evm-key.hex"
        runtime_bootnodes_path = f"{runtime_base_dir}/reth-bootnodes.txt"
        runtime_genesis_path = f"{runtime_base_dir}/genesis.json"
        datadir = f"{runtime_validator_dir}/data"
        ipc_path = f"{datadir}/reth.ipc"
        log_dir = f"{runtime_validator_dir}/logs"

        cmd = command_lines(
            chain_binary=args.chain_binary,
            genesis_runtime_path=runtime_genesis_path,
            datadir=datadir,
            rpc_host=args.http_addr,
            rpc_port=rpc_port,
            reth_p2p_port=reth_port,
            discv5_host=args.discv5_addr,
            discv5_port=discv5_port,
            bootnodes_runtime_path=runtime_bootnodes_path,
            p2p_secret_runtime_path=runtime_secret_path,
            authrpc_port=authrpc_port,
            ipc_path=ipc_path,
            metrics_host=args.metrics_addr,
            metrics_port=metrics_port,
            log_dir=log_dir,
            signing_key_path=signing_key_path,
            evm_key_path=runtime_evm_key_path,
            consensus_listen_host=args.consensus_listen_host,
            consensus_listen_port=consensus_port,
            use_local_defaults=args.use_local_defaults,
        )
        script_path = commands_dir / f"validator-{index}.sh"
        write_command_script(script_path, cmd)
        commands.append((index, str(script_path), cmd))

        rows.append(
            {
                "index": index,
                "host": host,
                "address": address,
                "public_key": "0x" + public_key,
                "consensus_p2p": consensus_p2p,
                "reth_p2p": reth_p2p,
                "rpc_port": rpc_port,
                "metrics_port": metrics_port,
                "enode": enode,
            }
        )

    bootnodes_path = output_dir / "reth-bootnodes.txt"
    bootnodes_path.write_text("\n".join(bootnodes) + "\n")

    network_md = build_network_markdown(
        validators=validators,
        genesis_path=genesis_path,
        copied_validators_path=copied_validators_path,
        bootnodes_path=bootnodes_path,
        commands=commands,
        reth_rows=rows,
        runtime_base_dir=runtime_base_dir,
        include_private_keys=args.include_private_keys,
        wallet_private_keys=wallet_private_keys,
    )
    network_path = output_dir / "network.md"
    network_path.write_text(network_md)

    print("Prepared Outbe network:")
    print(f"  genesis:        {genesis_path}")
    print(f"  validators:     {copied_validators_path}")
    print(f"  reth bootnodes: {bootnodes_path}")
    print(f"  network plan:   {network_path}")
    print(f"  commands:       {commands_dir}")


if __name__ == "__main__":
    main()
