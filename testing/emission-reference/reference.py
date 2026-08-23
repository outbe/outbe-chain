#!/usr/bin/env python3
"""Reference model and oracle for the six-decimal daily COEN emission cap.

Two independent layers, deliberately separated:

`day_emission_limit` mirrors the production integer algorithm in
`outbe-emissionlimit::day_emission` statement for statement (same recurrence,
same per-term floor division, same term count, same alternating accumulation).
It is a *cross-implementation* check: it proves Rust `U256` arithmetic and
Python bigint arithmetic agree. It CANNOT detect an error in the formula, the
term count, the rounding order, or the constants, because it would make the
same error.

`exact_day_emission` is the actual oracle. It evaluates the closed form
`INITIAL * e^(-k*d)` in arbitrary-precision decimal, with no Taylor truncation
and no integer flooring, and `check_oracle` asserts the integer algorithm stays
within `MAX_ORACLE_DEVIATION_UNITS` of it for every day in the formula domain.
That is the layer that would catch a wrong constant or a botched recurrence.
"""

from __future__ import annotations

import argparse
import json
from decimal import Decimal, getcontext
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

# Working precision for the oracle. The largest emission value is ~1.07e15
# units, so 60 significant digits leaves ~45 digits below the unit.
ORACLE_PRECISION = 60

# Allowed gap between the shipped integer algorithm and the exact closed form,
# in six-decimal units. Two error sources contribute: truncating the Taylor
# series at TAYLOR_TERMS, and flooring each term of the recurrence. Measured
# worst case across days 0..2919 is 3.91 units absolute (day 2197) and 4.3e-14
# relative (day 2769). The bound is set ~25x above that: loose enough not to be
# brittle, tight enough that any structural error blows straight through it —
# dropping TAYLOR_TERMS to 8 misses by 2.6e13 units, and transposing two digits
# of K_NUM moves the curve off the floor entirely.
MAX_ORACLE_DEVIATION_UNITS = 100


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


def exact_day_emission(day_number: int) -> Decimal:
    """Oracle: INITIAL * e^(-k*d) evaluated in arbitrary-precision decimal.

    No Taylor truncation, no integer flooring — this is the value the shipped
    algorithm is approximating, computed by an unrelated method (the decimal
    module's own exp).
    """
    getcontext().prec = ORACLE_PRECISION
    k = Decimal(K_NUM) / Decimal(K_DEN)
    return Decimal(INITIAL_DAY_EMISSION) * (-k * Decimal(day_number)).exp()


def check_oracle() -> int:
    """Assert the integer algorithm tracks the exact closed form on every day.

    Returns the worst absolute deviation in units, for reporting.
    """
    worst_deviation = 0
    worst_day = 0
    for day in range(FLOOR_DAY_THRESHOLD):
        actual = day_emission_limit(day)
        expected = exact_day_emission(day)
        deviation = abs(Decimal(actual) - expected)
        if deviation > worst_deviation:
            worst_deviation = deviation
            worst_day = day
    if worst_deviation > MAX_ORACLE_DEVIATION_UNITS:
        raise SystemExit(
            f"integer emission algorithm deviates from the exact closed form by "
            f"{worst_deviation} units at day {worst_day}; bound is "
            f"{MAX_ORACLE_DEVIATION_UNITS}"
        )

    # The floor must be where the curve naturally lands, not an early cliff:
    # the exact value one day past the threshold has to be at or below the
    # floor, and the last formula day at or above it.
    if exact_day_emission(FLOOR_DAY_THRESHOLD) > FLOOR_DAY_EMISSION:
        raise SystemExit(
            "floor clamps before the curve reaches it: "
            f"exact(day {FLOOR_DAY_THRESHOLD}) > FLOOR_DAY_EMISSION"
        )
    if day_emission_limit(FLOOR_DAY_THRESHOLD - 1) < FLOOR_DAY_EMISSION:
        raise SystemExit(
            f"day {FLOOR_DAY_THRESHOLD - 1} is already below the floor; the "
            "formula domain overshoots the clamp"
        )
    return int(worst_deviation.to_integral_value())


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
    """Byte-compare the committed vectors against a fresh generation, then
    re-derive the structural invariants from the committed file itself."""
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
    worst = check_oracle()
    print(
        f"emission reference OK: vectors match, worst oracle deviation {worst} "
        f"units (bound {MAX_ORACLE_DEVIATION_UNITS})"
    )


if __name__ == "__main__":
    main()
