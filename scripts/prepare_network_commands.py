"""Command script and markdown rendering helpers for prepare_network.py."""

from __future__ import annotations

import argparse
import stat
from pathlib import Path
from typing import Any

from prepare_network_founders import (
    format_enode_host,
    generate_reth_secret_hex,
    normalize_hex,
    parse_host_port,
    reth_node_id_from_secret,
    shell_quote,
    validator_consensus_address,
    validator_field,
    validator_reth_address,
    validator_signing_key_path,
    validator_wallet_info,
    write_secret_hex,
)


def command_lines(
    *,
    args: argparse.Namespace,
    runtime_base_dir: str,
    index: int,
    ports: dict[str, Any],
) -> list[str]:
    runtime_chain_binary = args.runtime_chain_binary or args.chain_binary
    runtime_validator_dir = f"{runtime_base_dir}/validator-{index}"
    p2p_secret_runtime_path = f"{runtime_validator_dir}/reth-p2p-secret.hex"
    evm_key_path = f"{runtime_validator_dir}/evm-key.hex"
    bootnodes_runtime_path = f"{runtime_base_dir}/reth-bootnodes.txt"
    genesis_runtime_path = f"{runtime_base_dir}/genesis.json"
    datadir = f"{runtime_validator_dir}/data"
    ipc_path = f"{datadir}/reth.ipc"
    log_dir = f"{runtime_validator_dir}/logs"
    signing_key_path = ports["signing_key_path"]
    reth_p2p_port = ports["reth_port"]
    discv5_port = ports["discv5_port"]
    rpc_port = ports["rpc_port"]
    authrpc_port = ports["authrpc_port"]
    metrics_port = ports["metrics_port"]
    consensus_listen_port = ports["consensus_port"]
    tee_enclave_endpoint = ports["tee_endpoint"]

    lines = [
        'export RUST_MIN_STACK="${RUST_MIN_STACK:-16777216}"',
        "",
        f"printf '%s' \"$(tr -d '[:space:]' < {shell_quote(p2p_secret_runtime_path)})\" > {shell_quote(p2p_secret_runtime_path)}",
        "",
        f"{runtime_chain_binary} node \\",
        "  --validator \\",
        f"  --chain {shell_quote(genesis_runtime_path)} \\",
        f"  --datadir {shell_quote(datadir)} \\",
        "  --engine.persistence-threshold 0 \\",
        "  --engine.memory-block-buffer-target 0 \\",
        f"  --http --http.addr {args.http_addr} --http.port {rpc_port} \\",
        "  --http.api eth,net,web3,outbe \\",
        f"  --port {reth_p2p_port} \\",
        f"  --discovery.port {reth_p2p_port} \\",
        f"  --discovery.v5.addr {args.discv5_addr} \\",
        f"  --discovery.v5.port {discv5_port} \\",
        f"  --bootnodes \"$(grep -v '^[[:space:]]*#' {shell_quote(bootnodes_runtime_path)} | paste -sd, -)\" \\",
        f"  --p2p-secret-key {shell_quote(p2p_secret_runtime_path)} \\",
        f"  --authrpc.port {authrpc_port} \\",
        f"  --ipcpath {shell_quote(ipc_path)} \\",
        f"  --metrics {args.metrics_addr}:{metrics_port} \\",
        f"  --log.file.directory {shell_quote(log_dir)} \\",
        f"  --consensus.signing-key {shell_quote(signing_key_path)} \\",
        f"  --validator.evm-key {shell_quote(evm_key_path)} \\",
        f"  --tee-enclave-socket {shell_quote(tee_enclave_endpoint)} \\",
        f"  --tee-bootstrap-timeout-secs {args.tee_bootstrap_timeout_secs} \\",
        f"  --consensus.listen-addr {args.consensus_listen_host}:{consensus_listen_port}",
    ]
    if args.use_local_defaults:
        lines[-1] += " \\"
        lines.append("  --consensus.use-local-defaults")
    return lines


def enclave_command_lines(
    *,
    args: argparse.Namespace,
    runtime_base_dir: str,
    validator_index: int,
    deployment_scope: str,
) -> list[str]:
    runtime_validator_dir = f"{runtime_base_dir}/validator-{validator_index}"
    endpoint = f"{args.tee_enclave_host}:{args.tee_enclave_base_port + validator_index}"
    container_name = f"outbe-{deployment_scope}-enclave-{validator_index}"
    chain_id_hex = f"0x{args.chain_id:064x}"
    common = [
        f"mkdir -p {shell_quote(runtime_validator_dir + '/tee')}",
        "",
        "docker run --rm \\",
        f"  --name {shell_quote(container_name)} \\",
        "  --network host \\",
        "  --log-driver local --log-opt max-size=10m --log-opt max-file=3 \\",
    ]
    if args.tee_mode == "dcap-required":
        common.insert(1, "test -S /var/run/aesmd/aesm.socket")
        common.extend(
            [
                "  --device /dev/sgx_enclave:/dev/sgx_enclave \\",
                "  --device /dev/sgx_provision:/dev/sgx_provision \\",
                "  -v /var/run/aesmd/aesm.socket:/var/run/aesmd/aesm.socket \\",
                f"  -v {shell_quote(runtime_validator_dir + '/tee')}:/var/lib/outbe/tee \\",
                f"  {shell_quote(args.enclave_image)} \\",
                f"  --socket {shell_quote(endpoint)} \\",
                "  --tee-dir /var/lib/outbe/tee \\",
                f"  --chain-id {chain_id_hex}",
            ]
        )
        return common

    common.extend(
        [
            "  --security-opt seccomp=unconfined \\",
            f"  -v {shell_quote(args.runtime_enclave_binary)}:/app/outbe-tee-enclave:ro \\",
            f"  -v {shell_quote(runtime_base_dir + '/test-sgx-signing-key.pem')}:/run/secrets/outbe-test-sgx-key.pem:ro \\",
            f"  -v {shell_quote(runtime_validator_dir + '/tee')}:/tee \\",
            f"  {shell_quote(args.enclave_image)} \\",
            f"  --socket {shell_quote(endpoint)} \\",
            f"  --dkg-seed {validator_index + 1:064x} \\",
            "  --tee-dir /tee \\",
            f"  --chain-id {chain_id_hex}",
        ]
    )
    return common


def write_command_script(path: Path, lines: list[str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    content = "#!/usr/bin/env bash\nset -euo pipefail\n\n" + "\n".join(lines) + "\n"
    path.write_text(content)
    mode = path.stat().st_mode
    path.chmod(mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def _ensure_validator_keys(
    validator: dict[str, Any],
    validator_dir: Path,
    index: int,
    force_reth_secrets: bool,
) -> tuple[str, str]:
    secret_path = validator_dir / "reth-p2p-secret.hex"
    secret_from_json = validator_field(
        validator, ["reth_p2p_secret_hex", "reth_p2p_secret"]
    )
    if secret_from_json is not None:
        secret_hex = normalize_hex(secret_from_json, expected_len=64, field=f"validator {index} reth p2p secret")
    elif secret_path.exists() and not force_reth_secrets:
        secret_hex = normalize_hex(secret_path.read_text(), expected_len=64, field=f"validator {index} existing reth p2p secret")
    else:
        secret_hex = generate_reth_secret_hex()
    write_secret_hex(secret_path, secret_hex)

    address, private_key = validator_wallet_info(validator)
    evm_key_path = validator_dir / "evm-key.hex"
    if private_key:
        write_secret_hex(evm_key_path, normalize_hex(private_key, expected_len=64, field=f"validator {index} evm private_key"))
    elif evm_key_path.exists():
        write_secret_hex(evm_key_path, normalize_hex(evm_key_path.read_text(), expected_len=64, field=f"validator {index} existing evm key"))
    return secret_hex, address


def _claim_validator_endpoints(
    claimed_endpoints: dict[tuple[str, int], str],
    index: int,
    endpoints: list[tuple[str, int, str]],
) -> None:
    for endpoint_host, endpoint_port, label in endpoints:
        if not 1 <= endpoint_port <= 65535:
            raise ValueError(
                f"validator {index} {label} port is outside 1..65535: {endpoint_port}"
            )
        key = (endpoint_host, endpoint_port)
        previous = claimed_endpoints.get(key)
        if previous is not None:
            raise ValueError(
                f"endpoint collision at {endpoint_host}:{endpoint_port}: "
                f"{previous} and validator-{index} {label}"
            )
        claimed_endpoints[key] = f"validator-{index} {label}"


def write_validator_bundle(
    *,
    args: argparse.Namespace,
    output_dir: Path,
    validators: list[dict[str, Any]],
    deployment_scope: str,
) -> dict[str, Any]:
    runtime_base_dir = args.runtime_base_dir or str(output_dir)
    bootnodes: list[str] = []
    rows: list[dict[str, Any]] = []
    commands: list[tuple[int, str, list[str]]] = []
    enclave_commands: list[tuple[int, str, list[str]]] = []
    commands_dir = output_dir / "commands"
    claimed_endpoints: dict[tuple[str, int], str] = {}

    for index, validator in enumerate(validators):
        consensus_p2p = validator_consensus_address(validator, index)
        host, consensus_port = parse_host_port(consensus_p2p)
        reth_p2p = validator_reth_address(
            validator, index, reth_p2p_base_port=args.reth_p2p_base_port
        )
        reth_host, reth_port = parse_host_port(reth_p2p)
        validator_dir = output_dir / f"validator-{index}"
        (validator_dir / "tee").mkdir(parents=True, exist_ok=True)
        secret_hex, address = _ensure_validator_keys(
            validator, validator_dir, index, args.force_reth_secrets
        )
        enode = f"enode://{reth_node_id_from_secret(secret_hex)}@{format_enode_host(reth_host)}:{reth_port}"
        bootnodes.append(enode)

        rpc_port = int(validator.get("rpc_port", args.rpc_base_port + index))
        authrpc_port = int(validator.get("authrpc_port", args.authrpc_base_port + index))
        metrics_port = int(validator.get("metrics_port", args.metrics_base_port + index))
        discv5_port = int(validator.get("reth_discv5_port", args.reth_discv5_base_port + index))
        tee_port = args.tee_enclave_base_port + index
        _claim_validator_endpoints(
            claimed_endpoints,
            index,
            [
                (host, consensus_port, "consensus"),
                (reth_host, reth_port, "reth-p2p"),
                (host, rpc_port, "rpc"),
                (host, authrpc_port, "authrpc"),
                (host, metrics_port, "metrics"),
                (host, discv5_port, "discv5"),
                (host, tee_port, "tee"),
            ],
        )
        ports = {
            "signing_key_path": validator_signing_key_path(
                validator, index, runtime_base_dir=runtime_base_dir
            ),
            "reth_port": reth_port,
            "discv5_port": discv5_port,
            "rpc_port": rpc_port,
            "authrpc_port": authrpc_port,
            "metrics_port": metrics_port,
            "consensus_port": consensus_port,
            "tee_endpoint": f"{args.tee_enclave_host}:{tee_port}",
        }
        cmd = command_lines(
            args=args, runtime_base_dir=runtime_base_dir, index=index, ports=ports
        )
        script_path = commands_dir / f"validator-{index}.sh"
        write_command_script(script_path, cmd)
        commands.append((index, str(script_path), cmd))

        enclave_cmd = enclave_command_lines(
            args=args,
            runtime_base_dir=runtime_base_dir,
            validator_index=index,
            deployment_scope=deployment_scope,
        )
        enclave_script_path = commands_dir / f"enclave-{index}.sh"
        write_command_script(enclave_script_path, enclave_cmd)
        enclave_commands.append((index, str(enclave_script_path), enclave_cmd))
        public_key = normalize_hex(
            validator["public_key"], expected_len=96, field=f"validator {index} public_key"
        )
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
    return {
        "bootnodes_path": bootnodes_path,
        "commands": commands,
        "enclave_commands": enclave_commands,
        "reth_rows": rows,
    }


def _wallet_key_cell(
    private_key: str | None, include_private_keys: bool
) -> str:
    if include_private_keys and private_key:
        return f"`{private_key}`"
    if private_key:
        return "`present in generated/input key material; redacted`"
    return "not included; use the operator-owned key for this address"


_BASH_FENCE = "```bash"


def build_network_markdown(bundle: dict[str, Any]) -> str:
    validators = bundle["validators"]
    wallet_private_keys = bundle.get("wallet_private_keys") or {}
    include_private_keys = bool(bundle["include_private_keys"])
    reth_rows = bundle["reth_rows"]
    lines: list[str] = [
        "# Outbe Network Launch Plan",
        "",
        "Generated as one chain-bound four-founder deployment bundle.",
        f"Canonical network: `{bundle['network_name']}` (chain ID `{bundle['chain_id']}`).",
        "",
        "## Artifacts",
        "",
        f"- Genesis: `{bundle['genesis_path']}`",
        f"- Validators input copy: `{bundle['copied_validators_path']}`",
        f"- Reth bootnodes: `{bundle['bootnodes_path']}`",
        f"- Runtime base dir used in commands: `{bundle['runtime_base_dir']}`",
        "",
        "`validators.json` is a genesis/tooling input only. Do not pass it to node runtime; `--consensus.validators` is removed.",
        "",
        "## Bootnodes",
        "",
        "```text",
        *(row["enode"] for row in reth_rows),
        "```",
        "",
        "## Validators",
        "",
        "| # | EVM address | BLS public key | Consensus P2P | Reth P2P | RPC | Metrics |",
        "|---:|---|---|---|---|---|---|",
    ]
    for row in reth_rows:
        pk = row["public_key"]
        short_pk = f"`{pk[:12]}...{pk[-12:]}`"
        lines.append(
            f"| {row['index']} | `{row['address']}` | {short_pk} | "
            f"`{row['consensus_p2p']}` | `{row['reth_p2p']}` | "
            f"`http://{row['host']}:{row['rpc_port']}` | `{row['host']}:{row['metrics_port']}` |"
        )
    lines.extend(["", "## Wallets", "", "| # | Address | Private key |", "|---:|---|---|"])
    for index, validator in enumerate(validators):
        address, private_key = validator_wallet_info(validator)
        key_cell = _wallet_key_cell(
            private_key or wallet_private_keys.get(index), include_private_keys
        )
        lines.append(f"| {index} | `{address}` | {key_cell} |")
    lines.extend(
        [
            "",
            "## Per-Validator Commands",
            "",
            "Copy the whole generated bundle to the runtime base directory. Start each enclave command, and only then start all four founder node commands within the configured TEE bootstrap timeout.",
            "",
        ]
    )
    for (index, enclave_script_path, enclave_cmd), (_, script_path, cmd) in zip(
        bundle["enclave_commands"], bundle["commands"]
    ):
        lines.extend(
            [
                f"### Validator {index}",
                "",
                f"Enclave script: `{enclave_script_path}`",
                "",
                _BASH_FENCE,
                *enclave_cmd,
                "```",
                "",
                f"Node script: `{script_path}`",
                "",
                _BASH_FENCE,
                *cmd,
                "```",
                "",
            ]
        )
    lines.extend(
        [
            "## Checks",
            "",
            _BASH_FENCE,
            "curl -sS -H 'content-type: application/json' \\",
            "  --data '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_blockNumber\",\"params\":[]}' \\",
            "  http://<rpc-host>:<rpc-port>",
            "",
            "curl -sS -H 'content-type: application/json' \\",
            "  --data '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"net_peerCount\",\"params\":[]}' \\",
            "  http://<rpc-host>:<rpc-port>",
            "",
            "curl -sS http://<metrics-host>:<metrics-port>/metrics | rg 'outbe_reshares_completed_total|commonware_p2p_tracked_peer_set_size|outbe_consensus_reth_tip_hash_match|outbe_parent_cert_store_size'",
            "```",
            "",
        ]
    )
    return "\n".join(lines)
