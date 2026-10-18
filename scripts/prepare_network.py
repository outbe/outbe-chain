#!/usr/bin/env python3
"""
Prepare a complete four-founder Outbe network bundle.

Inputs:
  - genesis.base.json: base chain config and initial alloc
  - seed-testnet.json or equivalent runtime seed config
  - validators.json: genesis validator public keys, EVM addresses, and consensus p2p addresses

Outputs:
  - genesis.json: seeded chain state with ValidatorSet/Staking/Rewards/precompile storage
    plus the canonical block-1 TEE policy
  - reth-bootnodes.txt: stable Reth enodes for --bootnodes
  - validator-N/evm-key.hex: validator EVM key material when present in validators.json
  - validator-N/reth-p2p-secret.hex: stable Reth p2p node identity keys
  - commands/validator-N.sh: runnable node command per validator
  - commands/enclave-N.sh: matching Gramine enclave command per validator
  - network.md: human-readable launch plan with addresses, ports, and commands
"""

from __future__ import annotations

import argparse
import re
import secrets
import stat
import subprocess  # nosec B404
import sys
from pathlib import Path
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from prepare_network_commands import (  # noqa: E402
    build_network_markdown,
    write_validator_bundle,
)
from prepare_network_founders import (  # noqa: E402
    default_base_genesis,
    generate_founder_radicle_material,
    generated_wallet_private_keys,
    import_founder_material,
    load_json,
    normalize_hex,
    parse_hosts,
    prepare_prefunded_genesis,
    run_founder_identity_generation,
    sanitized_validators_for_output,
    update_generated_validators,
    validator_consensus_address,
    validator_field,
    verify_founder_material,
    write_json,
    write_secret_hex,
)

DEFAULT_PREFUND_COEN_UNITS = 10_000 * 10**18
DEVNET_CHAIN_ID = 424242
TESTNET_CHAIN_ID = 54322345
MAINNET_CHAIN_ID = 676
NETWORK_IDENTITIES = {
    "devnet": (DEVNET_CHAIN_ID, "outbe-devnet-1"),
    "testnet": (TESTNET_CHAIN_ID, "outbe-testnet-1"),
    "mainnet": (MAINNET_CHAIN_ID, "outbe-mainnet-1"),
}
DEFAULT_EPOCH_LENGTH_BLOCKS = 300
DEFAULT_DKG_PREPARE_WINDOW_BLOCKS = 30
DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS = 30
DEFAULT_GAS_LIMIT = "0x1c9c380"
_ALL_INTERFACES = "0.0.0.0"  # nosec B104


def resolve_network_profile(
    network: str | None, chain_id: int | None, tee_mode: str
) -> tuple[str, int, str]:
    if chain_id is None:
        if tee_mode == "dcap-required":
            raise ValueError("dcap-required requires an explicit --chain-id")
        chain_id = DEVNET_CHAIN_ID
    inferred = next(
        (name for name, (known_id, _) in NETWORK_IDENTITIES.items() if known_id == chain_id),
        None,
    )
    if inferred is None:
        raise ValueError(f"unknown Outbe chain id {chain_id}")
    if network is None:
        if inferred == "mainnet":
            raise ValueError("Mainnet chain id 676 requires --network mainnet")
        network = inferred
    if network not in NETWORK_IDENTITIES:
        raise ValueError(f"unknown Outbe network {network!r}")
    expected_id, chain_name = NETWORK_IDENTITIES[network]
    if chain_id != expected_id:
        raise ValueError(f"{network} requires chain id {expected_id}, not {chain_id}")
    if network == "mainnet" and tee_mode != "dcap-required":
        raise ValueError("Mainnet requires --tee-mode dcap-required")
    if tee_mode == "gramine-direct-dev" and network not in ("devnet", "testnet"):
        raise ValueError("gramine-direct-dev requires the devnet or testnet profile")
    return network, chain_id, chain_name


def validate_mainnet_options(args: argparse.Namespace) -> None:
    forbidden = (
        ("generate_validators", "--generate-validators"),
        ("include_private_keys", "--include-private-keys"),
        ("use_local_defaults", "--use-local-defaults"),
        ("force_reth_secrets", "--force-reth-secrets"),
    )
    for attribute, option in forbidden:
        if getattr(args, attribute, None):
            raise ValueError(f"Mainnet forbids {option}")


def run_seed_genesis(
    *,
    preseed_genesis: Path,
    seed: Path,
    validators: Path,
    output_genesis: Path,
) -> None:
    subprocess.run(  # nosec B603
        [
            sys.executable,
            str(SCRIPT_DIR / "seed_genesis.py"),
            "--genesis",
            str(preseed_genesis),
            "--seed",
            str(seed),
            "--validators",
            str(validators),
            "--output",
            str(output_genesis),
        ],
        check=True,
    )


def run_tee_genesis(
    args: argparse.Namespace,
    seeded_genesis: Path,
    output_genesis: Path,
) -> None:
    cmd = [
        args.chain_binary,
        "tee",
        "genesis",
        "--input",
        str(seeded_genesis),
        "--output",
        str(output_genesis),
        "--mode",
        args.tee_mode,
    ]
    required = {
        "--mrenclave": args.mrenclave,
        "--mrsigner": args.mrsigner,
        "--isv-prod-id": args.isv_prod_id,
        "--minimum-isv-svn": args.minimum_isv_svn,
        "--minimum-tcb-evaluation-data-number": args.minimum_tcb_evaluation_data_number,
    }
    if args.tee_mode == "dcap-required":
        missing = [name for name, value in required.items() if value is None]
        if missing:
            raise ValueError(
                "dcap-required genesis needs exact release values: " + ", ".join(missing)
            )
        for name, value in required.items():
            cmd.extend([name, str(value)])
    elif any(value is not None for value in required.values()):
        raise ValueError("gramine-direct-dev forbids DCAP measurement arguments")
    subprocess.run(cmd, check=True)  # nosec B603


def generate_dev_sgx_signing_key(path: Path) -> None:
    subprocess.run(  # nosec B603, B607
        ["openssl", "genrsa", "-3", "-out", str(path), "3072"],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    path.chmod(stat.S_IRUSR | stat.S_IWUSR)


def _build_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Prepare a complete four-founder Outbe network bundle"
    )
    parser.add_argument("--genesis-base", type=Path)
    parser.add_argument("--seed", required=True, type=Path)
    parser.add_argument("--validators", type=Path)
    parser.add_argument("--founder-material-dir", type=Path)
    parser.add_argument("--generate-validators", type=int)
    parser.add_argument("--validator-hosts")
    parser.add_argument("--validator-hosts-file", type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--chain-binary", default="outbe-chain")
    parser.add_argument("--keygen-binary", default="outbe-keygen")
    parser.add_argument("--runtime-chain-binary")
    parser.add_argument(
        "--tee-mode",
        required=True,
        choices=("dcap-required", "gramine-direct-dev"),
    )
    parser.add_argument("--enclave-image", required=True)
    parser.add_argument(
        "--runtime-enclave-binary",
        default="/usr/local/bin/outbe-tee-enclave-mock",
    )
    parser.add_argument("--tee-enclave-host", default="127.0.0.1")
    parser.add_argument("--tee-enclave-base-port", type=int, default=17000)
    parser.add_argument("--tee-bootstrap-timeout-secs", type=int, default=300)
    parser.add_argument("--mrenclave")
    parser.add_argument("--mrsigner")
    parser.add_argument("--isv-prod-id", type=int)
    parser.add_argument("--minimum-isv-svn", type=int)
    parser.add_argument("--minimum-tcb-evaluation-data-number", type=int)
    parser.add_argument("--runtime-base-dir")
    parser.add_argument("--prefund-coen-units", type=int, default=DEFAULT_PREFUND_COEN_UNITS)
    parser.add_argument("--chain-id", type=int)
    parser.add_argument("--network", choices=tuple(NETWORK_IDENTITIES))
    parser.add_argument("--epoch-length-blocks", type=int, default=DEFAULT_EPOCH_LENGTH_BLOCKS)
    parser.add_argument("--dkg-prepare-window-blocks", type=int, default=DEFAULT_DKG_PREPARE_WINDOW_BLOCKS)
    parser.add_argument("--dkg-activation-grace-blocks", type=int, default=DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS)
    parser.add_argument("--gas-limit", default=DEFAULT_GAS_LIMIT)
    parser.add_argument("--consensus-p2p-base-port", type=int, default=30400)
    parser.add_argument("--consensus-port-stride", type=int, default=1)
    parser.add_argument("--reth-p2p-base-port", type=int, default=30303)
    parser.add_argument("--reth-discv5-base-port", type=int, default=31303)
    parser.add_argument("--rpc-base-port", type=int, default=8545)
    parser.add_argument("--authrpc-base-port", type=int, default=8551)
    parser.add_argument("--metrics-base-port", type=int, default=9101)
    parser.add_argument("--http-addr", default=_ALL_INTERFACES)
    parser.add_argument("--metrics-addr", default=_ALL_INTERFACES)
    parser.add_argument("--discv5-addr", default=_ALL_INTERFACES)
    parser.add_argument("--consensus-listen-host", default=_ALL_INTERFACES)
    parser.add_argument("--use-local-defaults", action="store_true")
    parser.add_argument("--include-private-keys", action="store_true")
    parser.add_argument("--force-reth-secrets", action="store_true")
    return parser


def _generate_founder_material(
    args: argparse.Namespace, output_dir: Path
) -> tuple[Path, dict[int, str]]:
    if args.generate_validators != 4:
        raise ValueError(
            "Bootstrap profile requires exactly 4 founding validators; "
            "--generate-validators must be 4"
        )
    hosts = parse_hosts(args, args.generate_validators)
    run_founder_identity_generation(
        chain_binary=args.chain_binary,
        output_dir=output_dir,
        count=args.generate_validators,
    )
    validators_path = output_dir / "validators.json"
    validators = update_generated_validators(
        validators_path=validators_path,
        hosts=hosts,
        args=args,
    )
    generate_founder_radicle_material(
        keygen_binary=args.keygen_binary,
        output_dir=output_dir,
        validators=validators,
    )
    write_json(validators_path, validators)
    return validators_path, generated_wallet_private_keys(
        output_dir, args.generate_validators
    )


def _import_existing_founder_material(
    args: argparse.Namespace, output_dir: Path
) -> Path:
    validators_path = args.validators
    external_validators = load_json(validators_path)
    if not isinstance(external_validators, list):
        raise ValueError("validators.json must contain a JSON array")
    verify_founder_material(
        chain_binary=args.chain_binary,
        validators_path=validators_path,
        material_dir=args.founder_material_dir,
    )
    import_founder_material(
        args.founder_material_dir, output_dir, 4, external_validators
    )
    return validators_path


def _prepare_founders(
    args: argparse.Namespace, output_dir: Path
) -> tuple[list[dict[str, Any]], Path, dict[int, str]]:
    if bool(args.validators) == bool(args.generate_validators):
        raise ValueError("provide either --validators or --generate-validators")
    if args.validators and args.founder_material_dir is None:
        raise ValueError("--validators requires --founder-material-dir for runnable founder commands")
    if args.generate_validators and args.founder_material_dir is not None:
        raise ValueError("--founder-material-dir is only valid with --validators")

    if args.generate_validators:
        validators_path, wallet_private_keys = _generate_founder_material(
            args, output_dir
        )
    else:
        validators_path = _import_existing_founder_material(args, output_dir)
        wallet_private_keys = {}

    validators = load_json(validators_path)
    if not isinstance(validators, list) or len(validators) != 4:
        raise ValueError("Bootstrap profile requires exactly 4 founding validators")
    for index, validator in enumerate(validators):
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
    return validators, copied_validators_path, wallet_private_keys


def _build_genesis(
    args: argparse.Namespace,
    output_dir: Path,
    validators: list[dict[str, Any]],
    copied_validators_path: Path,
) -> Path:
    if args.genesis_base:
        base_genesis = load_json(args.genesis_base)
    else:
        base_genesis = default_base_genesis(args)
        write_json(output_dir / "genesis.base.json", base_genesis)
    configured_chain_id = base_genesis.get("config", {}).get("chainId")
    if configured_chain_id != args.chain_id:
        raise ValueError(
            f"genesis chainId {configured_chain_id!r} does not match --chain-id {args.chain_id}"
        )
    preseed_genesis = prepare_prefunded_genesis(
        base_genesis, validators, prefund_coen_units=args.prefund_coen_units
    )
    preseed_path = output_dir / "genesis.prefund.json"
    seeded_genesis_path = output_dir / "genesis.seeded.json"
    genesis_path = output_dir / "genesis.json"
    write_json(preseed_path, preseed_genesis)
    run_seed_genesis(
        preseed_genesis=preseed_path,
        seed=args.seed,
        validators=copied_validators_path,
        output_genesis=seeded_genesis_path,
    )
    run_tee_genesis(args, seeded_genesis_path, genesis_path)
    if args.tee_mode == "gramine-direct-dev":
        generate_dev_sgx_signing_key(output_dir / "test-sgx-signing-key.pem")
    return genesis_path


def main() -> None:
    args = _build_arg_parser().parse_args()
    network, args.chain_id, network_name = resolve_network_profile(
        args.network, args.chain_id, args.tee_mode
    )
    if network == "mainnet":
        validate_mainnet_options(args)
    if args.tee_mode == "dcap-required" and re.fullmatch(
        r"[^\s]+@sha256:[0-9a-f]{64}", args.enclave_image
    ) is None:
        raise ValueError(
            "dcap-required needs --enclave-image as an immutable image digest "
            "(name@sha256:<64 lowercase hex>)"
        )
    if args.tee_bootstrap_timeout_secs <= 0:
        raise ValueError("--tee-bootstrap-timeout-secs must be > 0")

    output_dir = args.output_dir
    if output_dir.exists():
        raise ValueError(f"refusing to reuse existing output directory: {output_dir}")
    output_dir.mkdir(parents=True)
    deployment_scope = secrets.token_hex(8)
    write_secret_hex(output_dir / "deployment-scope", deployment_scope)

    validators, copied_validators_path, wallet_private_keys = _prepare_founders(
        args, output_dir
    )
    genesis_path = _build_genesis(
        args, output_dir, validators, copied_validators_path
    )
    runtime_base_dir = args.runtime_base_dir or str(output_dir)
    bundle = write_validator_bundle(
        args=args,
        output_dir=output_dir,
        validators=validators,
        deployment_scope=deployment_scope,
    )
    bundle.update(
        {
            "validators": validators,
            "genesis_path": genesis_path,
            "copied_validators_path": copied_validators_path,
            "runtime_base_dir": runtime_base_dir,
            "network_name": network_name,
            "chain_id": args.chain_id,
            "include_private_keys": args.include_private_keys,
            "wallet_private_keys": wallet_private_keys,
        }
    )
    network_path = output_dir / "network.md"
    network_path.write_text(build_network_markdown(bundle))
    write_json(
        output_dir / "network-identity.json",
        {"network": network, "chainId": args.chain_id, "chainName": network_name},
    )


if __name__ == "__main__":
    main()
