"""Inputs shared by the genesis calculation and its operator stages."""

from __future__ import annotations
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class SeedOptions:
    contracts_dir: str | None = None
    canon_dir: str | None = None
    worldwide_day: int | None = None
    fresh_metadosis: bool = False


@dataclass(frozen=True)
class SeedInputs:
    base_genesis: dict
    seed: dict
    validators: list


@dataclass(frozen=True)
class ValidatorSeedTerms:
    epoch_length_blocks: int
    epoch_start_timestamp: int
    min_stake: int
    validator_stake: int


@dataclass(frozen=True)
class StakeSeedTerms:
    min_stake: int
    validator_stake: int
