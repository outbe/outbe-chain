#!/usr/bin/env python3
"""Independent integer reference for the six-decimal daily COEN emission cap."""

from __future__ import annotations

import argparse
import json
from pathlib import Path


SCALE_1E6 = 1_000_000
INITIAL_DAY_EMISSION = (1 << 30) * SCALE_1E6
FLOOR_DAY_EMISSION = (1 << 26) * SCALE_1E6
FLOOR_DAY_THRESHOLD = 2_920
K_NUM = 94_952
K_DEN = 100_000_000
TAYLOR_TERMS = 32
PIN_DAYS = (0, 1, 365, 730, 1_460, 2_190, 2_919, 2_920)
DEFAULT_VECTORS = Path(__file__).with_name("vectors.json")


def day_emission_limit(day_number: int) -> int:
    if not 0 <= day_number <= 0xFFFF_FFFF:
        raise ValueError("day_number must fit u32")
    if day_number >= FLOOR_DAY_THRESHOLD:
        return FLOOR_DAY_EMISSION

    positive_sum = INITIAL_DAY_EMISSION
    negative_sum = 0
    term = INITIAL_DAY_EMISSION
    for index in range(1, TAYLOR_TERMS):
        term = term * K_NUM * day_number // (K_DEN * index)
        if term == 0:
            break
        if index % 2:
            negative_sum += term
        else:
            positive_sum += term

    reward = max(positive_sum - negative_sum, 0)
    return max(reward, FLOOR_DAY_EMISSION)


def build_vectors() -> dict[str, object]:
    days = [
        {"day": day, "emission_units": str(day_emission_limit(day))}
        for day in range(FLOOR_DAY_THRESHOLD + 1)
    ]
    return {
        "schema": "outbe-emission-scale6-v1",
        "algorithm": "alternating-taylor-amount-domain-floor-v1",
        "constants": {
            "units_per_coen": str(SCALE_1E6),
            "initial_day_emission_units": str(INITIAL_DAY_EMISSION),
            "floor_day_emission_units": str(FLOOR_DAY_EMISSION),
            "floor_day_threshold": FLOOR_DAY_THRESHOLD,
            "k_num": str(K_NUM),
            "k_den": str(K_DEN),
            "taylor_terms": TAYLOR_TERMS,
        },
        "pins": [
            {"day": day, "emission_units": str(day_emission_limit(day))}
            for day in PIN_DAYS
        ],
        "days": days,
    }


def canonical_bytes(value: object) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode()


def check_vectors(path: Path) -> None:
    actual = path.read_bytes()
    expected = canonical_bytes(build_vectors())
    if actual != expected:
        raise SystemExit(f"{path} does not match the independent emission reference")

    parsed = json.loads(actual)
    values = [int(row["emission_units"]) for row in parsed["days"]]
    if len(values) != FLOOR_DAY_THRESHOLD + 1:
        raise SystemExit("emission vector must cover every day through the floor threshold")
    if any(current > previous for previous, current in zip(values, values[1:])):
        raise SystemExit("emission vector is not monotonic non-increasing")
    if values[-1] != FLOOR_DAY_EMISSION:
        raise SystemExit("threshold vector does not equal the emission floor")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--mode", choices=("check", "generate"), default="check")
    parser.add_argument("--vectors", type=Path, default=DEFAULT_VECTORS)
    args = parser.parse_args()

    if args.mode == "generate":
        args.vectors.write_bytes(canonical_bytes(build_vectors()))
        return
    check_vectors(args.vectors)


if __name__ == "__main__":
    main()
