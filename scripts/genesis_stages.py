"""Build the base genesis and apply a declarative seed profile."""

from __future__ import annotations
from genesis_inputs import SeedInputs
from genesis_inputs import SeedOptions
import importlib.util
import io
import json
import tempfile
import time
from contextlib import redirect_stdout
from copy import deepcopy
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from genesis_yaml import load_yaml

from genesis_network import (
    BASE_PROFILE_PATH,
    DEFAULT_CHAIN_ID,
    DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS,
    DEFAULT_DKG_PREPARE_WINDOW_BLOCKS,
    DEFAULT_EPOCH_LENGTH_BLOCKS,
    DEFAULT_GAS_LIMIT,
    DEFAULT_PREFUND_COEN_UNITS,
    MAX_GENESIS_AGE_SECONDS,
    SCRIPT_DIR,
    SEED_SECTIONS,
    deep_merge,
)


def build_base_genesis(config: dict[str, Any]) -> dict[str, Any]:
    timestamp = int(config.get("timestamp", int(time.time())))
    age = int(time.time()) - timestamp
    if age > MAX_GENESIS_AGE_SECONDS and not config.get("allow_stale_timestamp"):
        raise ValueError(
            f"`timestamp: {timestamp}` is {age // 3600}h in the past. The TEE lease "
            f"runs from the genesis timestamp, so block 1 would fail with "
            f"'requested lease is already expired'. Drop the key to stamp now, or "
            f"set `allow_stale_timestamp: true` to reproduce an existing genesis."
        )
    # An explicit genesisTime pins the ValidatorSet epoch start. Without it, the
    # seeder uses the wall clock instead. Then the genesis hash, which the TEE
    # registrations sign, changes on every run.
    genesis_time = datetime.fromtimestamp(timestamp, tz=timezone.utc).strftime(
        "%Y-%m-%dT%H:%M:%SZ"
    )
    return {
        "config": {
            "chainId": int(config.get("chain_id", DEFAULT_CHAIN_ID)),
            "genesisTime": genesis_time,
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
            "epochLengthBlocks": int(
                config.get("epoch_length_blocks", DEFAULT_EPOCH_LENGTH_BLOCKS)
            ),
            "dkgPrepareWindowBlocks": int(
                config.get(
                    "dkg_prepare_window_blocks", DEFAULT_DKG_PREPARE_WINDOW_BLOCKS
                )
            ),
            "dkgActivationGraceBlocks": int(
                config.get(
                    "dkg_activation_grace_blocks", DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS
                )
            ),
        },
        "nonce": "0x0",
        "timestamp": hex(timestamp),
        "extraData": "0x",
        "gasLimit": str(config.get("gas_limit", DEFAULT_GAS_LIMIT)),
        "difficulty": "0x0",
        "mixHash": "0x" + "00" * 32,
        "coinbase": "0x" + "00" * 20,
        "alloc": {},
    }


def prefund_validators(
    genesis: dict[str, Any], config: dict[str, Any], validators: list[dict[str, Any]]
) -> None:
    prefund = int(config.get("prefund_coen_units", DEFAULT_PREFUND_COEN_UNITS))
    if prefund == 0:
        return
    alloc = genesis.setdefault("alloc", {})
    for validator in validators:
        alloc.setdefault(validator["address"], {}).setdefault("balance", hex(prefund))


def build_seed(config: dict[str, Any]) -> dict[str, Any]:
    """Protocol sections for the seeder: the baseline profile with this
    network's overrides merged on top."""
    baseline = load_yaml(BASE_PROFILE_PATH)
    seed = {
        section: baseline[section] for section in SEED_SECTIONS if section in baseline
    }
    for section in SEED_SECTIONS:
        if section in config:
            seed[section] = deep_merge(seed.get(section), config[section])
    return seed


def load_seed_genesis_module():
    spec = importlib.util.spec_from_file_location(
        "seed_genesis", SCRIPT_DIR / "seed_genesis.py"
    )
    if spec is None or spec.loader is None:
        raise ValueError(f"cannot load {SCRIPT_DIR / 'seed_genesis.py'}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def run_seed_stage(
    module,
    work_dir: Path,
    inputs: SeedInputs,
    config: dict[str, Any],
    quiet: bool = False,
) -> Path:
    """Render the declarative config into a seeded genesis.
    The calculation itself lives in seed_genesis.apply_seed: storage slot
    layout, keccak-derived mapping keys, and the active WorldwideDay resolved
    against the genesis timestamp. This function calls it directly with
    in-memory values, so nothing round-trips through an intermediate seed file.
    """
    base_genesis = inputs.base_genesis
    seed = inputs.seed
    validators = inputs.validators
    # The seeded OFFERING worldwide-day must track the genesis date, or the
    # runtime derives a different active day and metadosis wedges. An explicit
    # value always wins, including alongside fresh_metadosis. The two do
    # different things. The day retargets the S-curve peak and NOD references.
    # fresh_metadosis drops the pre-seeded OFFERING day itself.
    if "worldwide_day" in config:
        # Present-but-None means the caller decided there is no retarget, which
        # is what seed_genesis.py's CLI passes when `--worldwide-day` is absent.
        raw_day = config["worldwide_day"]
        worldwide_day = None if raw_day is None else int(raw_day)
    elif config.get("fresh_metadosis"):
        worldwide_day = None
    else:
        timestamp = int(base_genesis["timestamp"], 16)
        worldwide_day = module.timestamp_to_utc_date_key(timestamp)
    seeded = deepcopy(base_genesis)
    call = lambda: module.apply_seed(
        seeded,
        seed,
        validators,
        options=module.SeedOptions(
            contracts_dir=str(config.get("contracts_dir", SCRIPT_DIR / "contracts")),
            canon_dir=str(config.get("canon_dir", SCRIPT_DIR / "canon")),
            worldwide_day=worldwide_day,
            fresh_metadosis=bool(config.get("fresh_metadosis")),
        ),
    )
    if quiet:
        with redirect_stdout(io.StringIO()):
            call()
    else:
        call()
    # The TEE stage is the node binary's own subcommand, so the
    # seeded genesis reaches them as a file.
    seeded_path = work_dir / "genesis.seeded.json"
    seeded_path.write_text(json.dumps(seeded, indent=2))
    (work_dir / "validators.json").write_text(json.dumps(validators, indent=2))
    return seeded_path


def seed_genesis_from_config(
    *, inputs: SeedInputs, options: SeedOptions
) -> dict[str, Any]:
    """Seed one genesis from an explicit profile, without the TEE stage.
    `seed_genesis.py` uses this entry point. Thus its CLI, which the e2e harness
    and the localnet scripts call, creates its genesis through exactly the same
    code path as a yaml-driven deployment. This function uses the profile as
    given. Callers that want the baseline merged do the merge themselves. Thus a
    partial profile never silently gains sections that it did not ask for.
    """
    base_genesis = inputs.base_genesis
    seed = inputs.seed
    validators = inputs.validators
    contracts_dir = options.contracts_dir
    canon_dir = options.canon_dir
    worldwide_day = options.worldwide_day
    fresh_metadosis = options.fresh_metadosis
    module = load_seed_genesis_module()
    # This caller sets `worldwide_day` unconditionally, including to None. It
    # states the day explicitly rather than letting the genesis timestamp
    # derive it. Thus `--worldwide-day`-less runs keep the profile's own authored
    # day exactly as they did before.
    settings: dict[str, Any] = {
        "contracts_dir": contracts_dir,
        "worldwide_day": worldwide_day,
    }
    if canon_dir is not None:
        settings["canon_dir"] = canon_dir
    if fresh_metadosis:
        settings["fresh_metadosis"] = True
    with tempfile.TemporaryDirectory(prefix="outbe-seed-") as tmp:
        seeded_path = run_seed_stage(
            module=module,
            work_dir=Path(tmp),
            config=settings,
            inputs=SeedInputs(
                base_genesis=base_genesis, seed=seed, validators=validators
            ),
        )
        return json.loads(seeded_path.read_text())
