#!/usr/bin/env python3
"""Independent Decimal reference for the founder daily COEN emission curve."""

from __future__ import annotations

import argparse
import json
from decimal import Decimal, ROUND_FLOOR, getcontext
from pathlib import Path


SCALE_1E6 = 1_000_000
INITIAL_DAY_EMISSION = (1 << 28) * SCALE_1E6
FLOOR_DAY_EMISSION = (1 << 26) * SCALE_1E6
FLOOR_DAY_THRESHOLD = 3_072
PIN_DAYS = (
    0,
    1,
    365,
    512,
    730,
    1_024,
    1_025,
    1_460,
    2_048,
    2_190,
    2_919,
    2_920,
    3_071,
    3_072,
)
DEFAULT_VECTORS = Path(__file__).with_name("vectors.json")

getcontext().prec = 100


def tanh(value: Decimal) -> Decimal:
    exp_twice_value = (value * 2).exp()
    return (exp_twice_value - 1) / (exp_twice_value + 1)


# Founder formula. Keep these as expressions so this reference verifies the
# requested curve rather than copying constants produced by the Rust code.
P = Decimal(26) * Decimal(2) ** 26 / Decimal(3)
K1 = Decimal(128)
K2 = K1 * 3
A = (P - Decimal(2) ** 28) / tanh(Decimal(512) / (2 * K1))
O1 = (Decimal(2) ** 28 + P) / 2 - A / 2
D = (P - Decimal(2) ** 26) / tanh(Decimal(1024) / (2 * K2))
O2 = (P + Decimal(2) ** 26) / 2 + D / 2


def day_emission_limit(day_number: int) -> int:
    if not 0 <= day_number <= 0xFFFF_FFFF:
        raise ValueError("day_number must fit u32")
    if day_number >= FLOOR_DAY_THRESHOLD:
        return FLOOR_DAY_EMISSION

    day = Decimal(day_number)
    if day_number <= 1_024:
        emission = O1 + A / (1 + (-(day - 512) / K1).exp())
    else:
        emission = O2 - D / (1 + (-(day - 2048) / K2).exp())

    emission_units = int(
        (emission * SCALE_1E6).to_integral_value(rounding=ROUND_FLOOR)
    )
    return max(emission_units, FLOOR_DAY_EMISSION)


def build_vectors() -> dict[str, object]:
    days = [
        {"day": day, "emission_units": str(day_emission_limit(day))}
        for day in range(FLOOR_DAY_THRESHOLD + 1)
    ]
    return {
        "schema": "outbe-emission-scale6-v1",
        "algorithm": "founder-two-phase-sigmoid-decimal-floor-v1",
        "constants": {
            "units_per_coen": str(SCALE_1E6),
            "initial_day_emission_units": str(INITIAL_DAY_EMISSION),
            "floor_day_emission_units": str(FLOOR_DAY_EMISSION),
            "floor_day_threshold": FLOOR_DAY_THRESHOLD,
            "p": "26 * 2**26 / 3",
            "k1": "128",
            "k2": "K1 * 3",
            "phase_one_midpoint_day": 512,
            "phase_split_day": 1_024,
            "phase_two_midpoint_day": 2_048,
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
    if any(current < previous for previous, current in zip(values, values[1:1_025])):
        raise SystemExit("emission vector decreases before the phase-one peak")
    if any(current > previous for previous, current in zip(values[1_024:], values[1_025:])):
        raise SystemExit("emission vector increases after the phase-one peak")
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
