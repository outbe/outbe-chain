#!/usr/bin/env python3
"""Independent, standard-library-only reference for bounded Lysis Program V1."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
from pathlib import Path
from typing import Any, Iterable

U256_MOD = 1 << 256
I256_MAX = (1 << 255) - 1
SCALE = 10**18


class ReferenceFailure(Exception):
    """A deterministic typed Lysis V1 failure."""

    def __init__(
        self,
        kind: str,
        *,
        ordinal: int | None = None,
        currency: int | None = None,
    ) -> None:
        super().__init__(kind)
        self.kind = kind
        self.ordinal = ordinal
        self.currency = currency

    def envelope(self) -> dict[str, Any]:
        error: dict[str, Any] = {"kind": self.kind}
        if self.ordinal is not None:
            error["ordinal"] = self.ordinal
        if self.currency is not None:
            error["currency"] = self.currency
        return {"status": "FAILURE", "error": error}


def wrap_u256(value: int) -> int:
    return value % U256_MOD


def checked_add_u256(left: int, right: int, failure: ReferenceFailure) -> int:
    result = left + right
    if result >= U256_MOD:
        raise failure
    return result


def trunc_signed_div(numerator: int, denominator: int) -> int:
    if denominator == 0:
        raise ZeroDivisionError("signed division by zero")
    quotient = abs(numerator) // abs(denominator)
    return -quotient if (numerator < 0) != (denominator < 0) else quotient


def to_i256(value: int) -> int:
    if value > I256_MAX:
        raise ReferenceFailure("ARITHMETIC")
    return value


def fp_root(value: int, exponent_denominator: int) -> int:
    if value == 0:
        return 0
    if value == SCALE:
        return SCALE
    target = value * (SCALE ** (exponent_denominator - 1))
    low = 1
    high = max(value, SCALE)
    while low < high:
        middle = low + (high - low + 1) // 2
        if middle**exponent_denominator > target:
            high = middle - 1
        else:
            low = middle
    if low >= U256_MOD:
        raise ReferenceFailure("ARITHMETIC")
    return low


def fallback_tau(group_count: int, tribute_count: int) -> int:
    group_root = fp_root(wrap_u256(group_count * SCALE), 5)
    tribute_root = fp_root(wrap_u256(tribute_count * SCALE), 10)
    return wrap_u256(group_root * tribute_root) // SCALE


def policy_tau(populations: list[int], tribute_count: int) -> list[int]:
    group_count = len(populations)
    tau = [0] * (group_count + 1)
    for index in range(1, group_count):
        current = populations[index]
        previous = populations[index - 1]
        if current != 0 and previous != 0:
            half_index = wrap_u256((2 * index - 1) * SCALE) // 2
            numerator = fp_root(half_index, 5)
            current_root = fp_root(wrap_u256(current * SCALE), 10)
            previous_root = fp_root(wrap_u256(previous * SCALE), 10)
            divisor = min(current_root, previous_root)
            tau[index] = (
                wrap_u256(numerator * SCALE) // divisor
                if divisor
                else fallback_tau(group_count, tribute_count)
            )
        else:
            tau[index] = fallback_tau(group_count, tribute_count)
    middle_sum = 0
    for value in tau[1:group_count]:
        middle_sum = wrap_u256(middle_sum + value)
    policy_c = SCALE * 2 // 10
    tau[0] = wrap_u256(policy_c * middle_sum) // SCALE
    tau[group_count] = wrap_u256((SCALE - policy_c) * middle_sum) // SCALE
    return tau


def distribution(
    shares: list[int],
    populations: list[int],
    tribute_count: int,
    target_fraction: int,
    maximum_fraction: int,
) -> list[int]:
    if not populations:
        return [0]
    if len(populations) == 1:
        return [target_fraction]

    tau = policy_tau(populations, tribute_count)
    tau_total = 0
    for value in tau:
        tau_total = wrap_u256(tau_total + value)
    masses = [
        wrap_u256(value * SCALE) // tau_total if tau_total else 0 for value in tau
    ]

    cumulative = [0] * (len(shares) + 1)
    running = 0
    for index, value in enumerate(shares):
        running = wrap_u256(running + value)
        cumulative[index + 1] = running
    cumulative[-1] = SCALE

    expected = 0
    expected_square = 0
    for mass, value in zip(masses, cumulative):
        expected = wrap_u256(
            expected + wrap_u256(mass * value) // SCALE
        )
        squared = wrap_u256(wrap_u256(mass * value) * value)
        expected_square = wrap_u256(
            expected_square + squared // wrap_u256(SCALE * SCALE)
        )
    expected_term = wrap_u256(expected * expected) // SCALE
    variance = max(expected_square - expected_term, 0)

    fraction_ratio = (
        wrap_u256(target_fraction * SCALE) // maximum_fraction
        if maximum_fraction
        else 0
    )
    expected_i = to_i256(expected)
    beta_numerator = to_i256(fraction_ratio) - expected_i
    beta_denominator = to_i256(variance)
    maximum_i = to_i256(maximum_fraction)

    fractions = [0] * len(shares)
    for index in range(1, len(shares) + 1):
        tail_sum = 0
        for tail in range(index, len(masses)):
            difference = to_i256(cumulative[tail]) - expected_i
            beta_term = (
                trunc_signed_div(beta_numerator * difference, beta_denominator)
                if beta_denominator > 0
                else 0
            )
            factor = SCALE + beta_term
            tail_sum += trunc_signed_div(to_i256(masses[tail]) * factor, SCALE)
        result = trunc_signed_div(maximum_i * tail_sum, SCALE)
        fractions[index - 1] = 0 if result <= 0 else result

    weighted_total = 0
    for fraction, share in zip(fractions, shares):
        weighted_total = wrap_u256(
            weighted_total + wrap_u256(fraction * share) // SCALE
        )
    if weighted_total > target_fraction:
        fractions = [
            wrap_u256(value * target_fraction) // weighted_total
            for value in fractions
        ]
    return fractions


def fraction_table(
    tributes: list[dict[str, Any]], total_nominal: int, allocation: int
) -> list[dict[str, Any]]:
    groups: dict[int, list[int]] = {}
    for ordinal, tribute in enumerate(tributes):
        groups.setdefault(int(tribute["f1"]), []).append(ordinal)
    leagues = sorted(groups)
    shares: list[int] = []
    populations: list[int] = []
    for league in leagues:
        group_nominal = 0
        for ordinal in groups[league]:
            group_nominal = wrap_u256(
                group_nominal + int(tributes[ordinal]["nominal"])
            )
        shares.append(wrap_u256(group_nominal * SCALE) // total_nominal)
        populations.append(len(groups[league]))
    share_sum = 0
    for value in shares:
        share_sum = wrap_u256(share_sum + value)
    if shares and share_sum < SCALE:
        shares[-1] = wrap_u256(shares[-1] + SCALE - share_sum)
    target = wrap_u256(allocation * SCALE) // total_nominal
    maximum = wrap_u256(target * 2)
    fractions = distribution(shares, populations, len(tributes), target, maximum)
    return [
        {
            "league": league,
            "share": str(share),
            "population": population,
            "fraction": str(fraction),
        }
        for league, share, population, fraction in zip(
            leagues, shares, populations, fractions
        )
    ]


def _validate_and_sort(case_input: dict[str, Any]) -> list[dict[str, Any]]:
    worldwide_day = int(case_input["worldwide_day"])
    tributes = sorted(case_input["tributes"], key=lambda item: item["tribute_id"])
    if not tributes:
        raise ReferenceFailure("EMPTY_INPUT")
    previous: str | None = None
    for ordinal, tribute in enumerate(tributes):
        raw_id = tribute["tribute_id"]
        if (
            len(raw_id) != 72
            or raw_id != raw_id.lower()
            or int(raw_id[:8], 16) != worldwide_day
            or int(tribute["worldwide_day"]) != worldwide_day
        ):
            raise ReferenceFailure("WORLDWIDE_DAY_MISMATCH", ordinal=ordinal)
        if previous == raw_id:
            raise ReferenceFailure("DUPLICATE_INPUT", ordinal=ordinal)
        previous = raw_id
    return tributes


def evaluate(case: dict[str, Any]) -> dict[str, Any]:
    """Evaluate one JSON case without any native-code dependency."""

    case_input = case["input"]
    try:
        tributes = _validate_and_sort(case_input)
        observations: list[dict[str, Any]] = []
        total_nominal = 0
        for ordinal, tribute in enumerate(tributes):
            first = tribute.get("f1")
            if first is None:
                raise ReferenceFailure("FIDELITY_FIRST_UNAVAILABLE", ordinal=ordinal)
            observations.append(
                {
                    "kind": "FIDELITY",
                    "ordinal": ordinal,
                    "phase": "FIRST",
                    "owner": tribute["owner"],
                    "league": int(first),
                }
            )
            total_nominal = checked_add_u256(
                total_nominal,
                int(tribute["nominal"]),
                ReferenceFailure("TOTAL_NOMINAL_OVERFLOW", ordinal=ordinal),
            )
        if total_nominal == 0:
            raise ReferenceFailure("ZERO_TOTAL_NOMINAL")

        allocation = int(case_input["gratis_allocation"])
        table = fraction_table(tributes, total_nominal, allocation)
        fractions = {int(row["league"]): int(row["fraction"]) for row in table}

        mandatory_price = case_input.get("mandatory_price_840")
        if mandatory_price is None:
            raise ReferenceFailure("MANDATORY_ORACLE_UNAVAILABLE", currency=840)
        mandatory_price = int(mandatory_price)
        if mandatory_price == 0:
            raise ReferenceFailure("ZERO_ENTRY_PRICE", currency=840)
        observations.append(
            {
                "kind": "ORACLE",
                "ordinal": None,
                "currency": 840,
                "entry_price": str(mandatory_price),
            }
        )

        remaining = allocation
        actions: list[dict[str, Any]] = []
        contributors: dict[str, int] = {}
        for ordinal, tribute in enumerate(tributes):
            fraction = fractions[int(tribute["f1"])]
            load = wrap_u256(int(tribute["nominal"]) * fraction) // SCALE
            if load == 0:
                raise ReferenceFailure("ZERO_GRATIS_LOAD", ordinal=ordinal)
            if load > remaining:
                raise ReferenceFailure(
                    "GRATIS_LOAD_EXCEEDS_REMAINING", ordinal=ordinal
                )
            remaining -= load

            currency = int(tribute["reference_currency"])
            if currency == 840:
                entry_price = mandatory_price
            else:
                conditional = tribute.get("conditional_price")
                if conditional is None:
                    raise ReferenceFailure(
                        "CONDITIONAL_ORACLE_UNAVAILABLE",
                        ordinal=ordinal,
                        currency=currency,
                    )
                entry_price = int(conditional)
                if entry_price == 0:
                    raise ReferenceFailure(
                        "ZERO_ENTRY_PRICE", ordinal=ordinal, currency=currency
                    )
                observations.append(
                    {
                        "kind": "ORACLE",
                        "ordinal": ordinal,
                        "currency": currency,
                        "entry_price": str(entry_price),
                    }
                )

            floor_price = (
                wrap_u256(
                    max(int(tribute["tribute_price"]), entry_price) * 108
                )
                // 100
            )
            second = tribute.get("f2")
            if second is None:
                raise ReferenceFailure("FIDELITY_SECOND_UNAVAILABLE", ordinal=ordinal)
            observations.append(
                {
                    "kind": "FIDELITY",
                    "ordinal": ordinal,
                    "phase": "SECOND",
                    "owner": tribute["owner"],
                    "league": int(second),
                }
            )
            if int(second) != int(tribute["f1"]):
                raise ReferenceFailure("FIDELITY_MISMATCH", ordinal=ordinal)
            cost = wrap_u256(entry_price * load) // SCALE
            if (
                not bool(tribute.get("nod_target_available", True))
                or int(tribute["owner"], 16) == 0
            ):
                raise ReferenceFailure("INVALID_NOD_TARGET", ordinal=ordinal)

            actions.append(
                {
                    "source_tribute_id": tribute["tribute_id"],
                    "owner": tribute["owner"],
                    "worldwide_day": int(tribute["worldwide_day"]),
                    "league": int(second),
                    "floor_price": str(floor_price),
                    "gratis_load": str(load),
                    "entry_price": str(entry_price),
                    "cost": str(cost),
                    "issuance_currency": int(tribute["issuance_currency"]),
                    "reference_currency": currency,
                    "issued_at": int(case_input["logical_evaluation_time"]),
                }
            )
            if not bool(tribute.get("excluded", False)):
                owner = tribute["owner"]
                contributors[owner] = checked_add_u256(
                    contributors.get(owner, 0),
                    int(tribute["nominal"]),
                    ReferenceFailure("CONTRIBUTOR_OVERFLOW", ordinal=ordinal),
                )

        return {
            "status": "SUCCESS",
            "total_nominal": str(total_nominal),
            "gratis_allocation": str(allocation),
            "remaining_gratis": str(remaining),
            "group_table": table,
            "nod_actions": actions,
            "contributors": [
                {"owner": owner, "nominal": str(nominal)}
                for owner, nominal in sorted(contributors.items())
            ],
            "observations": observations,
        }
    except ReferenceFailure as failure:
        return failure.envelope()


def _reject_duplicate_keys(pairs: Iterable[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def load_cases(path: Path) -> list[dict[str, Any]]:
    cases: list[dict[str, Any]] = []
    identifiers: set[str] = set()
    for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not line.strip():
            continue
        case = json.loads(line, object_pairs_hook=_reject_duplicate_keys)
        identifier = case["case_id"]
        if identifier in identifiers:
            raise ValueError(f"duplicate case_id {identifier!r} at line {line_number}")
        identifiers.add(identifier)
        cases.append(case)
    return cases


def canonical_json(value: Any) -> bytes:
    return json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=True
    ).encode("ascii")


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def render(cases_path: Path) -> int:
    for case in load_cases(cases_path):
        rendered = dict(case)
        rendered["expected"] = evaluate(case)
        print(canonical_json(rendered).decode("ascii"))
    return 0


def write(cases_path: Path) -> int:
    rendered = []
    for case in load_cases(cases_path):
        updated = dict(case)
        updated["expected"] = evaluate(case)
        rendered.append(canonical_json(updated))
    payload = b"\n".join(rendered) + b"\n"
    temporary = cases_path.with_suffix(".jsonl.tmp")
    with temporary.open("xb") as output:
        output.write(payload)
        output.flush()
        os.fsync(output.fileno())
    temporary.replace(cases_path)
    return 0


def check(cases_path: Path, manifest_path: Path, script_path: Path) -> int:
    cases = load_cases(cases_path)
    mismatches: list[str] = []
    expected_outputs: list[dict[str, Any]] = []
    for case in cases:
        actual = evaluate(case)
        expected_outputs.append(actual)
        if case.get("expected") != actual:
            mismatches.append(case["case_id"])

    manifest = json.loads(
        manifest_path.read_text(encoding="utf-8"),
        object_pairs_hook=_reject_duplicate_keys,
    )
    integrity_errors: list[str] = []
    if manifest.get("case_count") != len(cases):
        integrity_errors.append("case_count")
    if manifest.get("case_file_sha256") != sha256_file(cases_path):
        integrity_errors.append("case_file_sha256")
    if manifest.get("reference_sha256") != sha256_file(script_path):
        integrity_errors.append("reference_sha256")

    report = {
        "status": "PASS" if not mismatches and not integrity_errors else "FAIL",
        "case_count": len(cases),
        "case_ids": [case["case_id"] for case in cases],
        "expected_outputs_sha256": hashlib.sha256(
            b"\n".join(canonical_json(value) for value in expected_outputs) + b"\n"
        ).hexdigest(),
        "reference_sha256": sha256_file(script_path),
        "case_file_sha256": sha256_file(cases_path),
        "mismatches": mismatches,
        "integrity_errors": integrity_errors,
    }
    print(canonical_json(report).decode("ascii"))
    return 0 if report["status"] == "PASS" else 1


def parse_args(argv: list[str]) -> argparse.Namespace:
    repository = Path(__file__).resolve().parents[2]
    default_vectors = repository / "crates/core/lysis/vectors/lysis-v1"
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--render", action="store_true")
    mode.add_argument("--write", action="store_true")
    parser.add_argument(
        "--cases", type=Path, default=default_vectors / "cases.jsonl"
    )
    parser.add_argument(
        "--manifest", type=Path, default=default_vectors / "manifest.json"
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    arguments = parse_args(sys.argv[1:] if argv is None else argv)
    if arguments.render:
        return render(arguments.cases)
    if arguments.write:
        return write(arguments.cases)
    return check(arguments.cases, arguments.manifest, Path(__file__).resolve())


if __name__ == "__main__":
    raise SystemExit(main())
