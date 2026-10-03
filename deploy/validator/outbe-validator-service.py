#!/usr/bin/env python3

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import socket
import stat
import sys
import time
import tomllib
import urllib.request
from collections.abc import Callable
from typing import NoReturn


OUTBE_ROOT = pathlib.Path("/opt/outbe-chain")
STORAGE_CONFIG = OUTBE_ROOT / "offchain-storage.toml"
RADICLE_CONTROL_SOCKET = (
    OUTBE_ROOT / "keys" / "radicle" / "node" / "outbe-control.sock"
)
ENCLAVE_ADDRESS = ("127.0.0.1", 17000)
LOCAL_RPC_URL = "http://127.0.0.1:8545"
DEFAULT_READINESS_TIMEOUT = 120.0
DIRECT_URL_OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def fail(message: str) -> NoReturn:
    raise SystemExit(f"outbe-validator-service: {message}")


def required_env(name: str) -> str:
    value = os.environ.get(name, "")
    if not value:
        fail(f"missing environment variable: {name}")
    return value


def exec_role(argv: list[str], *, cwd: pathlib.Path | None = None) -> NoReturn:
    if cwd is not None:
        os.chdir(cwd)
    os.execvpe(argv[0], argv, os.environ.copy())


def wait_for(
    description: str,
    probe: Callable[[], None],
    timeout: float,
) -> None:
    deadline = time.monotonic() + timeout
    last_error = "not ready"
    while True:
        try:
            probe()
            return
        except Exception as error:
            last_error = str(error)

        if time.monotonic() >= deadline:
            fail(f"timed out waiting for {description}: {last_error}")
        time.sleep(0.25)


def probe_enclave() -> None:
    with socket.create_connection(ENCLAVE_ADDRESS, timeout=2.0):
        pass


def probe_radicle() -> None:
    try:
        mode = RADICLE_CONTROL_SOCKET.stat().st_mode
    except FileNotFoundError as error:
        raise RuntimeError("Radicle control socket is absent") from error
    if not stat.S_ISSOCK(mode):
        raise RuntimeError("Radicle control path is not a socket")


def probe_validator_dependencies() -> None:
    probe_radicle()
    probe_enclave()


def probe_rpc() -> None:
    request = urllib.request.Request(
        LOCAL_RPC_URL,
        data=json.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "eth_chainId",
                "params": [],
            }
        ).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with DIRECT_URL_OPENER.open(request, timeout=2.0) as response:
        payload = json.load(response)

    if payload.get("error") is not None:
        raise RuntimeError(f"local RPC returned an error: {payload['error']}")
    chain_id = int(payload["result"], 16)
    expected_chain_id = int(required_env("OCOMP_CHAIN_ID"))
    if chain_id != expected_chain_id:
        raise RuntimeError(
            f"local RPC chain id {chain_id} does not match {expected_chain_id}"
        )


def wait_enclave(timeout: float) -> None:
    wait_for("TEE enclave", probe_enclave, timeout)


def wait_validator_dependencies(timeout: float) -> None:
    wait_for(
        "TEE enclave and Radicle control socket",
        probe_validator_dependencies,
        timeout,
    )


def wait_rpc(timeout: float) -> None:
    wait_for("local validator RPC", probe_rpc, timeout)


def verify_rocksdb_storage(path: pathlib.Path) -> None:
    try:
        with path.open("rb") as stream:
            document = tomllib.load(stream)
    except OSError as error:
        fail(f"cannot read storage configuration: {error}")
    except tomllib.TOMLDecodeError:
        fail("storage configuration is not valid TOML")

    if document.get("version") != 1:
        fail("storage configuration requires version = 1")
    if document.get("backend") != "rocksdb":
        fail("validator deployment requires RocksDB storage")

    allowed = {"version", "backend", "start_block", "rocksdb"}
    if set(document) - allowed:
        fail("storage configuration contains unknown fields")

    start_block = document.get("start_block", 1)
    if (
        isinstance(start_block, bool)
        or not isinstance(start_block, int)
        or not 0 <= start_block <= 2**64 - 1
    ):
        fail("storage start_block must be a u64")

    rocksdb = document.get("rocksdb")
    required = {"path", "secondary_path"}
    if not isinstance(rocksdb, dict) or set(rocksdb) != required:
        fail("storage configuration has an invalid rocksdb section")
    if any(
        not isinstance(value, str) or not value.strip() for value in rocksdb.values()
    ):
        fail("storage configuration has an invalid rocksdb value")


def enclave() -> NoReturn:
    runtime = required_env("OUTBE_ENCLAVE_RUNTIME")
    chain_id = required_env("OCOMP_CHAIN_ID")
    try:
        chain_hex = f"0x{int(chain_id):064x}"
    except ValueError:
        fail("OCOMP_CHAIN_ID must be a positive decimal integer")
    arguments = [
        "--socket",
        "127.0.0.1:17000",
        "--tee-dir",
        "/var/lib/outbe/tee" if runtime == "bundled-sgx" else str(OUTBE_ROOT / "tee"),
        "--chain-id",
        chain_hex,
    ]
    if runtime == "system-gramine":
        exec_role(
            ["gramine-sgx", "outbe-tee-enclave", *arguments],
            cwd=OUTBE_ROOT,
        )
    if runtime == "bundled-sgx":
        sgx_dir = OUTBE_ROOT / "sgx"
        exec_role(
            [str(sgx_dir / "bin" / "outbe-tee-enclave-launch"), *arguments],
            cwd=sgx_dir,
        )
    fail("OUTBE_ENCLAVE_RUNTIME must be system-gramine or bundled-sgx")


def feeder() -> NoReturn:
    chain_id = required_env("OCOMP_CHAIN_ID")
    validator = required_env("OUTBE_VALIDATOR_ADDRESS")
    if re.fullmatch(r"[1-9][0-9]*", chain_id) is None:
        fail("OCOMP_CHAIN_ID must be a positive decimal integer")
    if re.fullmatch(r"0x[0-9a-fA-F]{40}", validator) is None:
        fail("OUTBE_VALIDATOR_ADDRESS must be a 20-byte hex address")

    key_path = OUTBE_ROOT / "keys" / "evm-key.hex"
    try:
        key = key_path.read_bytes()
    except OSError as error:
        fail(f"cannot read Validator EVM key: {error}")
    if re.fullmatch(rb"[0-9a-f]{64}", key) is None:
        fail(f"Validator EVM key is not canonical: {key_path}")

    public_path = OUTBE_ROOT / "feeder-public.toml"
    try:
        public = public_path.read_text()
    except OSError as error:
        fail(f"cannot read public feeder configuration: {error}")

    runtime_dir = pathlib.Path(
        os.environ.get("RUNTIME_DIRECTORY", "/run/outbe-feeder")
    )
    runtime_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
    config = runtime_dir / "feeder.toml"
    header = f"""[chain]
rpc_endpoint = "http://127.0.0.1:8545"
chain_id = {chain_id}
gasless_oracle_votes = true

[account]
private_key = {json.dumps("0x" + key.decode())}
validator_address = {json.dumps(validator)}

[health]
enabled = true
bind_address = "127.0.0.1:9002"

"""
    descriptor = os.open(config, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(descriptor, "w") as output:
        output.write(header)
        output.write(public)
        if not public.endswith("\n"):
            output.write("\n")
    exec_role([str(OUTBE_ROOT / "outbe-feeder"), "--config", str(config)])


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "role",
        choices=(
            "verify-rocksdb",
            "wait-enclave",
            "wait-validator-dependencies",
            "wait-rpc",
            "enclave",
            "feeder",
        ),
    )
    parser.add_argument("--storage-config", type=pathlib.Path, default=STORAGE_CONFIG)
    parser.add_argument(
        "--timeout",
        type=float,
        default=DEFAULT_READINESS_TIMEOUT,
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    role = args.role
    if role == "verify-rocksdb":
        verify_rocksdb_storage(args.storage_config)
        return 0
    if role == "wait-enclave":
        wait_enclave(args.timeout)
        return 0
    if role == "wait-validator-dependencies":
        wait_validator_dependencies(args.timeout)
        return 0
    if role == "wait-rpc":
        wait_rpc(args.timeout)
        return 0
    if role == "enclave":
        enclave()
    if role == "feeder":
        feeder()
    fail(f"unknown role: {role}")


if __name__ == "__main__":
    sys.exit(main())
