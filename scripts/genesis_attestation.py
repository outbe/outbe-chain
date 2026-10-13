"""Bind OCOMP registrations and TEE policy to the seeded genesis."""

from __future__ import annotations
from genesis_inputs import GenesisTools, OcompPaths
import json
import shutil
import subprocess
from pathlib import Path
from typing import Any


def ensure_ocomp_registrations(
    *,
    tools: GenesisTools,
    paths: OcompPaths,
    validators: list[dict[str, Any]],
    allow_generation: bool = True,
) -> Path:
    """Collect one registration per founder into a staging directory, and mint
    the missing ones. The proof of possession signs the seeded genesis hash.
    Thus this function can produce a registration only after that hash exists."""
    chain_binary = tools.chain_binary
    keygen_binary = tools.keygen_binary
    work_dir = paths.work_dir
    seeded_genesis = paths.seeded_genesis
    keys_dir = paths.keys_dir
    validate_ocomp_registration_inventory(
        keys_dir=keys_dir,
        validator_count=len(validators),
        allow_generation=allow_generation,
    )
    bindings_path = work_dir / "ocomp-bindings-v1.json"
    subprocess.run(
        [
            chain_binary,
            "ocomp",
            "bindings",
            "--input",
            str(seeded_genesis),
            "--validators",
            str(work_dir / "validators.json"),
            "--output",
            str(bindings_path),
        ],
        check=True,
    )
    bindings = json.loads(bindings_path.read_text())
    chain_id, genesis_hash = bindings["chainId"], bindings["genesisHash"]
    staging = work_dir / "registrations"
    minted = False
    for index, validator in enumerate(validators):
        source_dir = keys_dir / f"validator-{index}"
        registration = source_dir / "ocomp-registration-v1.ocb1"
        # Marker recording which genesis hash a registration minted here signs.
        # A registration carried in from a validator's own machine has no
        # marker. The node binary validates that registration when it installs it.
        marker = source_dir / "ocomp-registration-v1.genesis-hash"
        if registration.is_file():
            if marker.is_file() and marker.read_text().strip() != str(genesis_hash):
                raise SystemExit(
                    f"validator-{index}: {registration.name} signs genesis hash "
                    f"{marker.read_text().strip()}, but this run seeds "
                    f"{genesis_hash}.\n"
                    f"Either pin the original `timestamp:` in the yaml, or delete "
                    f"{registration} and {source_dir / 'ocomp-key-v1.hex'} on every "
                    f"validator to mint a fresh pair for this genesis."
                )
            continue
        print(f"  minting OCOMP registration for validator-{index}")
        subprocess.run(
            [
                keygen_binary,
                "ocomp",
                "--output-dir",
                str(source_dir),
                "--chain-id",
                str(chain_id),
                "--genesis-hash",
                str(genesis_hash),
                "--validator-address",
                validator["address"],
                "--consensus-bls-min-pk",
                "0x" + validator["public_key"],
            ],
            check=True,
            capture_output=True,
        )
        marker.write_text(f"{genesis_hash}\n")
        minted = True
    for index in range(len(validators)):
        destination = staging / f"validator-{index}"
        destination.mkdir(parents=True)
        shutil.copy2(
            keys_dir / f"validator-{index}" / "ocomp-registration-v1.ocb1",
            destination / "ocomp-registration-v1.ocb1",
        )
    if minted:
        print(
            f"  registrations signed genesis hash {genesis_hash}; pin "
            f"`timestamp: {int(json.loads(seeded_genesis.read_text())['timestamp'], 16)}` "
            f"in the yaml to reproduce this exact genesis"
        )
    return staging


def validate_ocomp_registration_inventory(
    *, keys_dir: Path, validator_count: int, allow_generation: bool
) -> None:
    if allow_generation:
        return
    missing = [
        index
        for index in range(validator_count)
        if not (
            keys_dir / f"validator-{index}" / "ocomp-registration-v1.ocb1"
        ).is_file()
    ]
    if missing:
        rendered = ", ".join(f"validator-{index}" for index in missing)
        raise ValueError(
            "Mainnet requires operator-provided OCOMP registration files; missing "
            + rendered
        )


def run_ocomp_stage(
    *,
    chain_binary: str,
    paths: OcompPaths,
    registrations_dir: Path,
    protocol_bundle_output: Path,
) -> Path:
    work_dir = paths.work_dir
    seeded_genesis = paths.seeded_genesis
    keys_dir = paths.keys_dir
    ocomp_genesis = work_dir / "genesis.ocomp.json"
    result = subprocess.run(
        [
            chain_binary,
            "ocomp",
            "genesis",
            "--input",
            str(seeded_genesis),
            "--validators",
            str(work_dir / "validators.json"),
            "--registrations-dir",
            str(registrations_dir),
            "--output",
            str(ocomp_genesis),
            "--protocol-bundle-output",
            str(protocol_bundle_output),
        ],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        message = (result.stderr or result.stdout).strip()
        if "does not match this genesis" in message:
            raise SystemExit(
                f"{message}\n\n"
                f"An OCOMP registration signs one exact genesis hash. This run "
                f"seeds a different one - most often because `timestamp:` is not "
                f"pinned in the yaml, so each run stamps the current time.\n"
                f"Pin the timestamp the registrations were minted for, or delete "
                f"ocomp-registration-v1.ocb1 and ocomp-key-v1.hex under "
                f"{keys_dir}/validator-*/ to mint a fresh pair for this genesis."
            )
        raise SystemExit(message or "outbe-chain ocomp genesis failed")
    print(result.stdout.strip())
    return ocomp_genesis


def run_tee_stage(
    *,
    chain_binary: str,
    ocomp_genesis: Path,
    output: Path,
    config: dict[str, Any],
) -> None:
    tee = config["tee"]
    cmd = [
        chain_binary,
        "tee",
        "genesis",
        "--input",
        str(ocomp_genesis),
        "--output",
        str(output),
        "--mode",
        str(tee["mode"]),
    ]
    dcap_options = {
        "--mrenclave": tee.get("mrenclave"),
        "--mrsigner": tee.get("mrsigner"),
        "--isv-prod-id": tee.get("isv_prod_id"),
        "--minimum-isv-svn": tee.get("minimum_isv_svn"),
        "--minimum-tcb-evaluation-data-number": tee.get(
            "minimum_tcb_evaluation_data_number"
        ),
    }
    if tee["mode"] == "dcap-required":
        missing = sorted(name for name, value in dcap_options.items() if value is None)
        if missing:
            raise ValueError(
                "tee.mode dcap-required needs exact release values: "
                + ", ".join(missing)
            )
        for name, value in dcap_options.items():
            cmd.extend([name, str(value)])
    elif any(value is not None for value in dcap_options.values()):
        raise ValueError("tee.mode gramine-direct-dev forbids DCAP measurement values")
    subprocess.run(cmd, check=True)
