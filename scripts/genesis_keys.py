"""Validator public-key discovery and identity derivation."""

from __future__ import annotations
import base64
import subprocess
from pathlib import Path
from typing import Any
from launch_bundle import _point_add

from genesis_network import DEFAULT_CONSENSUS_P2P_PORT, SECP256K1_G, SECP256K1_N


def _point_mul(scalar: int, point: tuple[int, int]) -> tuple[int, int]:
    if scalar <= 0 or scalar >= SECP256K1_N:
        raise ValueError("secp256k1 scalar out of range")
    result: tuple[int, int] | None = None
    addend: tuple[int, int] | None = point
    while scalar:
        if scalar & 1:
            result = _point_add(result, addend)
        addend = _point_add(addend, addend)
        scalar >>= 1
    if result is None:
        raise ValueError("invalid secp256k1 scalar produced the point at infinity")
    return result


def evm_address_from_key_file(path: Path, keccak256) -> str:
    raw = path.read_text().strip().removeprefix("0x")
    if len(raw) != 64:
        raise ValueError(f"{path} must hold a 32-byte secp256k1 key in hex")
    x, y = _point_mul(int(raw, 16), SECP256K1_G)
    public_key = x.to_bytes(32, "big") + y.to_bytes(32, "big")
    return "0x" + keccak256(public_key)[12:].hex()


def radicle_node_id_from_public_key(path: Path) -> str:
    try:
        algorithm, encoded, *_ = path.read_text().split()
    except (OSError, ValueError) as error:
        raise ValueError(f"invalid Radicle public key {path}") from error
    if algorithm != "ssh-ed25519":
        raise ValueError(f"Radicle public key {path} must use ssh-ed25519")
    try:
        payload = base64.b64decode(encoded, validate=True)
    except ValueError as error:
        raise ValueError(f"invalid Radicle public key encoding in {path}") from error

    def field(offset: int) -> tuple[bytes, int]:
        if offset + 4 > len(payload):
            raise ValueError(f"truncated Radicle public key {path}")
        length = int.from_bytes(payload[offset : offset + 4], "big")
        start, end = offset + 4, offset + 4 + length
        if end > len(payload):
            raise ValueError(f"truncated Radicle public key {path}")
        return payload[start:end], end

    kind, offset = field(0)
    node_id, offset = field(offset)
    if kind != b"ssh-ed25519" or len(node_id) != 32 or offset != len(payload):
        raise ValueError(f"invalid Ed25519 Radicle public key {path}")
    return node_id.hex()


def bls_public_key(keygen_binary: str, signing_key: Path) -> str:
    """Ask outbe-keygen for the MinPk public key. The BLS curve maths lives in
    the binary, and this script does not reimplement it."""
    result = subprocess.run(
        [keygen_binary, "show-pubkey", "--key", str(signing_key)],
        check=True,
        capture_output=True,
        text=True,
    )
    for line in result.stdout.splitlines():
        if line.startswith("public key:"):
            value = line.split(":", 1)[1].strip().removeprefix("0x")
            if len(value) != 96:
                raise ValueError(f"{signing_key}: BLS public key is not 48 bytes")
            return value
    raise ValueError(f"outbe-keygen printed no public key for {signing_key}")


def discover_validators(
    *,
    config: dict[str, Any],
    keys_dir: Path,
    keygen_binary: str,
    keccak256,
) -> list[dict[str, Any]]:
    """Build the founder manifest from the yaml hosts plus the key directory."""
    default_port = int(config.get("consensus_p2p_port", DEFAULT_CONSENSUS_P2P_PORT))
    validators = []
    for index, entry in enumerate(config["validators"]):
        # Founders sit on separate machines, so they share one port unless an
        # entry pins its own as `host:port`.
        host, _, explicit_port = str(entry).strip().partition(":")
        port = int(explicit_port) if explicit_port else default_port
        directory = keys_dir / f"validator-{index}"
        if not directory.is_dir():
            raise ValueError(
                f"missing key directory for validator {index}: {directory}"
            )
        signing_key = directory / "signing-key.hex"
        evm_key = directory / "evm-key.hex"
        radicle_pub = directory / "radicle" / "keys" / "radicle.pub"
        for required in (signing_key, evm_key, radicle_pub):
            if not required.is_file():
                raise ValueError(
                    f"validator {index}: {required} not found; generate the bundle "
                    f"with `outbe-keygen validator --output-dir {directory}`"
                )
        validators.append(
            {
                "address": evm_address_from_key_file(evm_key, keccak256),
                "public_key": bls_public_key(keygen_binary, signing_key),
                "radicle_node_id": "0x" + radicle_node_id_from_public_key(radicle_pub),
                "p2p_address": f"{host}:{port}",
            }
        )
    return validators
