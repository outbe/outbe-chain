#!/usr/bin/env python3
"""Create a complete Outbe genesis.json from one network.yaml.
The yaml carries the minimum: which machines run the founding validators and
where their key material lives. This script derives everything else from the
key directory and writes it into the genesis in one run: public keys, Radicle
node ids, precompile storage, and the TEE policy.
    python3 scripts/create_genesis.py network.yaml
    chain_id: 424242
    keys_dir: ./keys          # keys_dir/validator-N/ per founder
    validators:
      - 10.0.0.1
      - 10.0.0.2
      - 10.0.0.3
      - 10.0.0.4
    tee:
      mode: gramine-direct-dev
Each keys_dir/validator-N/ is what `outbe-keygen validator` produces:
signing-key.hex (BLS), evm-key.hex, and radicle/keys/radicle.pub.
See scripts/network.example.yaml for every optional parameter and its
default. The yaml dialect is a strict subset (nested mappings, lists of
scalars, lists of flat mappings, ints, booleans, quoted/plain strings, `#`
comments). Anything else is a hard error that names the line.
"""

from __future__ import annotations
import argparse
import base64
import importlib.util
import io
import json
import re
import shutil
import subprocess
import sys
import tempfile
import time
from contextlib import redirect_stdout
from copy import deepcopy
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

from genesis_inputs import (
    SeedInputs,
    SeedOptions as SeedOptions,
)
from genesis_yaml import load_yaml, YamlError
from launch_bundle import _point_add
import launch_bundle  # noqa: E402  (sibling module, path set just above)
from genesis_attestation import (
    run_tee_stage,
)
from genesis_keys import (
    _point_mul,
    bls_public_key,
    discover_validators,
    evm_address_from_key_file,
    radicle_node_id_from_public_key,
)
from genesis_network import (
    BASE_PROFILE_PATH,
    DEFAULT_CHAIN_ID,
    DEFAULT_CONSENSUS_P2P_PORT,
    DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS,
    DEFAULT_DKG_PREPARE_WINDOW_BLOCKS,
    DEFAULT_EPOCH_LENGTH_BLOCKS,
    DEFAULT_GAS_LIMIT,
    DEFAULT_PREFUND_COEN_UNITS,
    DEVNET_CHAIN_ID,
    FOUNDER_COUNT,
    MAINNET_CHAIN_ID,
    MAX_GENESIS_AGE_SECONDS,
    NETWORK_IDENTITIES,
    REPO_ROOT,
    SCRIPT_DIR,
    SECP256K1_G,
    SECP256K1_N,
    SECP256K1_P,
    SEED_SECTIONS,
    TEE_KEYS,
    TESTNET_CHAIN_ID,
    TOP_LEVEL_KEYS,
    _validate_founders,
    _validate_mainnet,
    _validate_ports,
    _validate_tee,
    deep_merge,
    network_identity,
    resolve_binary,
    validate_config,
)
from genesis_stages import (
    build_base_genesis,
    build_seed,
    load_seed_genesis_module,
    prefund_validators,
    run_seed_stage,
    seed_genesis_from_config,
)


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Create a complete Outbe genesis.json from one network.yaml"
    )
    parser.add_argument("config", type=Path, help="Path to network.yaml")
    parser.add_argument(
        "--output",
        type=Path,
        help="Output genesis path (default: `output` from the yaml, else ./genesis.json)",
    )
    args = parser.parse_args()
    config_path = args.config.resolve()
    config = load_yaml(args.config)
    validate_config(config)
    network, chain_id, chain_name = network_identity(config)

    def relative_to_config(value: str) -> Path:
        candidate = Path(value)
        return candidate if candidate.is_absolute() else config_path.parent / candidate

    keys_dir = relative_to_config(str(config["keys_dir"])).resolve()
    if not keys_dir.is_dir():
        raise SystemExit(f"keys_dir is not a directory: {keys_dir}")
    output = args.output or relative_to_config(
        str(config.get("output", "genesis.json"))
    )
    if output.exists():
        raise SystemExit(f"refusing to overwrite existing genesis: {output}")
    output.parent.mkdir(parents=True, exist_ok=True)
    chain_binary = resolve_binary(config, "chain_binary", "outbe-chain")
    keygen_binary = resolve_binary(config, "keygen_binary", "outbe-keygen")
    module = load_seed_genesis_module()
    validators = discover_validators(
        config=config,
        keys_dir=keys_dir,
        keygen_binary=keygen_binary,
        keccak256=module.keccak256,
    )
    print("Founding validators:")
    for index, validator in enumerate(validators):
        print(
            f"  validator-{index}  {validator['p2p_address']}  {validator['address']}"
        )
    print()
    base_genesis = build_base_genesis(config)
    prefund_validators(base_genesis, config, validators)
    seed = build_seed(config)
    with tempfile.TemporaryDirectory(prefix="outbe-genesis-") as tmp:
        work_dir = Path(tmp)
        seeded = run_seed_stage(
            module=module,
            work_dir=work_dir,
            config=config,
            inputs=SeedInputs(
                base_genesis=base_genesis, seed=seed, validators=validators
            ),
        )
        run_tee_stage(
            chain_binary=chain_binary,
            seeded_genesis=seeded,
            output=output,
            config=config,
        )
    launch_bundle.render(
        config=config,
        validators=validators,
        genesis_path=output,
        keys_dir=keys_dir,
        repo_root=REPO_ROOT,
    )
    identity_path = output.parent / "network-identity.json"
    identity_path.write_text(
        json.dumps(
            {"network": network, "chainId": chain_id, "chainName": chain_name},
            indent=2,
        )
        + "\n"
    )
    print()
    print(f"genesis:         {output}")
    print(f"bootnodes:       {output.parent / 'reth-bootnodes.txt'}")
    print(f"launch scripts:  {output.parent}/validator-N/")
    print(f"instructions:    {output.parent / 'DEPLOY.md'}")
    print(f"network identity:{identity_path}")


if __name__ == "__main__":
    main()
