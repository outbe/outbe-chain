"""Founder identity, Radicle material, and Reth transport helpers for prepare_network.py."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import stat
import subprocess  # nosec B404
import time
from pathlib import Path
from typing import Any

from genesis_keys import radicle_node_id_from_public_key
from launch_bundle import SECP256K1_G, _mint_secp256k1_hex, _point_mul

PRIVATE_VALIDATOR_FIELDS = {
    "private_key",
    "evm_private_key",
    "ecdsa_private_key",
    "evm_key",
    "reth_p2p_secret_hex",
    "reth_p2p_secret",
}

generate_reth_secret_hex = _mint_secp256k1_hex


def load_json(path: Path) -> Any:
    with path.open() as f:
        return json.load(f)


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w") as f:
        json.dump(value, f, indent=2)
        f.write("\n")


def sanitized_validators_for_output(validators: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [
        {key: value for key, value in validator.items() if key not in PRIVATE_VALIDATOR_FIELDS}
        for validator in validators
    ]


def default_base_genesis(args: argparse.Namespace) -> dict[str, Any]:
    return {
        "config": {
            "chainId": args.chain_id,
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
            "epochLengthBlocks": args.epoch_length_blocks,
            "dkgPrepareWindowBlocks": args.dkg_prepare_window_blocks,
            "dkgActivationGraceBlocks": args.dkg_activation_grace_blocks,
        },
        "nonce": "0x0",
        "timestamp": hex(int(time.time())),
        "extraData": "0x",
        "gasLimit": args.gas_limit,
        "difficulty": "0x0",
        "mixHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
        "coinbase": "0x0000000000000000000000000000000000000000",
        "alloc": {},
    }


def normalize_hex(value: str, *, expected_len: int | None = None, field: str = "hex") -> str:
    raw = value.strip().lower().removeprefix("0x")
    if expected_len is not None and len(raw) != expected_len:
        raise ValueError(f"{field} must be {expected_len} hex chars, got {len(raw)}")
    try:
        bytes.fromhex(raw)
    except ValueError as exc:
        raise ValueError(f"{field} is not valid hex") from exc
    return raw


def parse_host_port(value: str) -> tuple[str, int]:
    host, sep, port_s = (
        value[1:].partition("]:") if value.startswith("[") else value.rpartition(":")
    )
    if not sep or not host:
        raise ValueError(f"invalid host:port: {value}")
    port = int(port_s)
    if not 1 <= port <= 65535:
        raise ValueError(f"invalid port in host:port: {value}")
    return host, port


def format_enode_host(host: str) -> str:
    return f"[{host}]" if ":" in host and not host.startswith("[") else host


def shell_quote(value: str) -> str:
    return "'" + value.replace("'", "'\"'\"'") + "'"


def compact_hex_balance(amount: int) -> str:
    if amount < 0:
        raise ValueError("balance cannot be negative")
    return hex(amount)


def reth_node_id_from_secret(secret_hex: str) -> str:
    raw = normalize_hex(secret_hex, expected_len=64, field="reth p2p secret")
    x, y = _point_mul(int(raw, 16), SECP256K1_G)
    return f"{x:064x}{y:064x}"


def validator_field(validator: dict[str, Any], names: list[str]) -> str | None:
    return next(
        (v.strip() for n in names if isinstance(v := validator.get(n), str) and v.strip()),
        None,
    )


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
    consensus_address = validator_consensus_address(validator, index)
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
    return explicit or f"{runtime_base_dir}/validator-{index}/signing-key.hex"


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
    raw_hosts = (args.validator_hosts or "").split(",")
    file_lines = (
        args.validator_hosts_file.read_text().splitlines()
        if args.validator_hosts_file
        else []
    )
    values = [h.strip() for h in [*raw_hosts, *file_lines] if h.strip() and not h.lstrip().startswith("#")]
    if not values:
        raise ValueError("--generate-validators requires --validator-hosts or --validator-hosts-file")
    if any(":" in value for value in values):
        raise ValueError(
            "validator hosts must be hosts/IPs only, without ports; ports are assigned by the script"
        )
    if len(values) == 1:
        pattern = values[0]
        return [
            pattern.replace("{i}", str(index)).replace("%d", str(index))
            for index in range(expected)
        ]
    if len(values) != expected:
        raise ValueError(f"expected {expected} validator hosts, got {len(values)}")
    return values


def run_founder_identity_generation(
    *,
    chain_binary: str,
    output_dir: Path,
    count: int,
) -> None:
    subprocess.run(  # nosec B603
        [
            chain_binary,
            "dkg",
            "identities",
            "--output-dir",
            str(output_dir),
            "--validators",
            str(count),
        ],
        check=True,
    )


def update_generated_validators(
    *,
    validators_path: Path,
    hosts: list[str],
    args: argparse.Namespace,
) -> list[dict[str, Any]]:
    validators = load_json(validators_path)
    if not isinstance(validators, list) or len(validators) != len(hosts):
        raise ValueError("generated validators.json must match host count")
    stride = args.consensus_port_stride
    for index, validator in enumerate(validators):
        port = args.consensus_p2p_base_port + index * stride
        if stride < 1 or not 1 <= port < 65535:
            raise ValueError("invalid consensus port stride or port range")
        validator["p2p_address"] = f"{hosts[index]}:{port}"
        validator["reth_p2p_address"] = f"{hosts[index]}:{args.reth_p2p_base_port + index}"
    write_json(validators_path, validators)
    return validators


def generated_wallet_private_keys(output_dir: Path, count: int) -> dict[int, str]:
    return {
        index: "0x" + normalize_hex(path.read_text(), expected_len=64, field=f"validator {index} evm key")
        for index in range(count)
        if (path := output_dir / f"validator-{index}" / "evm-key.hex").exists()
    }


def generate_founder_radicle_material(
    *, keygen_binary: str, output_dir: Path, validators: list[dict[str, Any]]
) -> None:
    for index, validator in enumerate(validators):
        home = output_dir / f"validator-{index}" / "radicle"
        subprocess.run(  # nosec B603
            [keygen_binary, "radicle", "--output-dir", str(home)],
            check=True,
        )
        validator["radicle_node_id"] = "0x" + radicle_node_id_from_public_key(
            home / "keys" / "radicle.pub"
        )


def validate_founder_radicle_material(
    validators: list[dict[str, Any]], material_dir: Path, count: int
) -> None:
    if len(validators) != count:
        raise ValueError(
            f"founder validator count {len(validators)} does not match material count {count}"
        )
    seen: set[str] = set()
    for index, validator in enumerate(validators):
        configured = normalize_hex(
            str(validator.get("radicle_node_id", "")),
            expected_len=64,
            field=f"validator {index} radicle_node_id",
        )
        if configured == "00" * 32 or configured in seen:
            raise ValueError(f"invalid or duplicate Radicle NodeId for validator {index}")
        seen.add(configured)
        radicle_keys = material_dir / f"validator-{index}" / "radicle/keys"
        if not (radicle_keys / "radicle").is_file():
            raise ValueError(f"founder material is missing {radicle_keys / 'radicle'}")
        if radicle_node_id_from_public_key(radicle_keys / "radicle.pub") != configured:
            raise ValueError(
                f"validator-{index} Radicle NodeId does not match validators.json"
            )


def import_founder_material(
    source: Path,
    output_dir: Path,
    count: int,
    validators: list[dict[str, Any]],
) -> None:
    validate_founder_radicle_material(validators, source, count)
    for index in range(count):
        destination = output_dir / f"validator-{index}"
        destination.mkdir(parents=True, exist_ok=True)
        for name in ("signing-key.hex", "evm-key.hex"):
            source_path = source / f"validator-{index}" / name
            if not source_path.is_file():
                raise ValueError(f"founder material is missing {source_path}")
            destination_path = destination / name
            shutil.copyfile(source_path, destination_path)
            destination_path.chmod(stat.S_IRUSR | stat.S_IWUSR)
        shutil.copytree(
            source / f"validator-{index}" / "radicle",
            destination / "radicle",
        )


def verify_founder_material(
    *,
    chain_binary: str,
    validators_path: Path,
    material_dir: Path,
) -> None:
    subprocess.run(  # nosec B603
        [
            chain_binary,
            "dkg",
            "verify-identities",
            "--validators",
            str(validators_path),
            "--material-dir",
            str(material_dir),
        ],
        check=True,
    )


def prepare_prefunded_genesis(
    base_genesis: dict[str, Any],
    validators: list[dict[str, Any]],
    *,
    prefund_coen_units: int,
) -> dict[str, Any]:
    genesis = json.loads(json.dumps(base_genesis))
    alloc = genesis.setdefault("alloc", {})
    if prefund_coen_units != 0:
        for validator in validators:
            address, _ = validator_wallet_info(validator)
            key = normalize_hex(address, expected_len=40, field="validator address")
            alloc.setdefault(key, {}).setdefault("balance", compact_hex_balance(prefund_coen_units))
    return genesis


def write_secret_hex(path: Path, hex_value: str) -> None:
    tmp_path = path.with_name(f".{path.name}.tmp.{os.getpid()}")
    fd = os.open(tmp_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, stat.S_IRUSR | stat.S_IWUSR)
    try:
        with os.fdopen(fd, "w") as handle:
            handle.write(hex_value + "\n")
        os.replace(tmp_path, path)
    except Exception:
        tmp_path.unlink(missing_ok=True)
        raise
