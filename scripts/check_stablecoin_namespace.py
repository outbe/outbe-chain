#!/usr/bin/env python3
"""Reproducible Stablecoin V1 address/namespace collision scanner."""

from __future__ import annotations

import argparse
import importlib
import json
import re
from pathlib import Path

seeder = importlib.import_module(
    "scripts.seed_genesis" if __package__ else "seed_genesis"
)
STABLECOIN_ADDRESS_PREFIX = seeder.STABLECOIN_ADDRESS_PREFIX
STABLECOIN_FACTORY_ADDRESS = seeder.STABLECOIN_FACTORY_ADDRESS
STABLECOIN_POLICY_REGISTRY_ADDRESS = seeder.STABLECOIN_POLICY_REGISTRY_ADDRESS
validate_stablecoin_namespace_alloc = seeder.validate_stablecoin_namespace_alloc

ADDRESS_LITERAL = re.compile(r'address!\("0x([0-9A-Fa-f]{40})"\)')
ETHEREUM_BUILTINS = {f"{value:040x}" for value in range(1, 11)}


def scan_rust_addresses(addresses_rs: Path) -> tuple[int, int]:
    """Checks exact-address uniqueness and stablecoin-class disjointness."""
    try:
        source = addresses_rs.read_text(encoding="utf-8")
    except OSError as error:
        raise ValueError(f"cannot read {addresses_rs}: {error}") from error

    declared = [match.lower() for match in ADDRESS_LITERAL.findall(source)]
    duplicates = sorted(
        {address for address in declared if declared.count(address) > 1}
    )
    if duplicates:
        raise ValueError(f"duplicate Rust address constants: {duplicates}")

    expected = {
        STABLECOIN_FACTORY_ADDRESS,
        STABLECOIN_POLICY_REGISTRY_ADDRESS,
    }
    missing = sorted(expected.difference(declared))
    if missing:
        raise ValueError(
            f"stablecoin fixed addresses missing from addresses.rs: {missing}"
        )

    fixed_collisions = sorted(expected.intersection(ETHEREUM_BUILTINS))
    if fixed_collisions:
        raise ValueError(
            f"stablecoin fixed address collides with Ethereum: {fixed_collisions}"
        )

    class_collisions = sorted(
        address for address in declared if address.startswith(STABLECOIN_ADDRESS_PREFIX)
    )
    if class_collisions:
        raise ValueError(
            f"declared address collides with stablecoin class "
            f"0x{STABLECOIN_ADDRESS_PREFIX}: {class_collisions}"
        )

    return len(declared), len(ETHEREUM_BUILTINS)


def scan_genesis(path: Path, *, require_reserved_markers: bool) -> int:
    """Checks one genesis alloc through the same validator used by the seeder."""
    try:
        genesis = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot parse genesis {path}: {error}") from error
    alloc = genesis.get("alloc", {})
    if not isinstance(alloc, dict):
        raise ValueError(f"genesis {path} alloc must be an object")
    validate_stablecoin_namespace_alloc(
        alloc, require_reserved_markers=require_reserved_markers
    )
    return len(alloc)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--addresses-rs",
        type=Path,
        default=Path(__file__).resolve().parents[1]
        / "crates/blockchain/primitives/src/addresses.rs",
    )
    parser.add_argument("--genesis", type=Path, action="append", default=[])
    parser.add_argument(
        "--preseed",
        action="store_true",
        help="allow absent fixed marker accounts when scanning pre-seed genesis",
    )
    args = parser.parse_args()

    declared_count, builtin_count = scan_rust_addresses(args.addresses_rs)
    print(
        f"stablecoin namespace ok: {declared_count} Rust addresses, "
        f"{builtin_count} Ethereum built-ins, prefix=0x{STABLECOIN_ADDRESS_PREFIX}"
    )
    for genesis_path in args.genesis:
        alloc_count = scan_genesis(
            genesis_path, require_reserved_markers=not args.preseed
        )
        print(f"stablecoin genesis ok: {genesis_path} ({alloc_count} alloc entries)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
