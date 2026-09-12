"""Arithmetic witnesses for the applicability review; no cryptography or timing.

The program knows all inputs. It checks identities and counterexamples, not
privacy, a proof system, Byzantine tolerance, resharing, or production parity.
Run: python3 testing/lysis-privacy-prototype/check_aggregation_applicability.py
"""

import json
import random


def direct(values, fraction, price, scale):
    loads = [a * fraction // scale for a in values]
    costs = [g * price // scale for g in loads]
    return sum(loads), sum(costs)


def through_residues(values, fraction, price, scale):
    """Identity for one group sharing public fraction and price.

    In a private protocol residues and per-record intermediates must remain
    secret; this cleartext reference does not implement that protocol.
    """
    modulus = scale * scale
    first_remainders = []
    second_remainders = []
    for a in values:
        t = a % modulus
        u = ((fraction % modulus) * t) % modulus
        first_remainders.append(u % scale)
        second_remainders.append(((price % scale) * (u // scale)) % scale)
    loads_numerator = fraction * sum(values) - sum(first_remainders)
    assert loads_numerator % scale == 0
    loads = loads_numerator // scale
    costs_numerator = price * loads - sum(second_remainders)
    assert costs_numerator % scale == 0
    return loads, costs_numerator // scale


def main():
    scale = 10**6
    u256_max = (1 << 256) - 1
    assert (10**9 * u256_max).bit_length() == 286
    assert (scale * scale - 1).bit_length() == 40

    # Same nominal total, count, fraction and price; different exact totals.
    a, b = [3, 3], [2, 4]
    assert sum(a) == sum(b) == 6
    assert direct(a, scale // 2, scale, scale) == (2, 2)
    assert direct(b, scale // 2, scale, scale) == (3, 3)
    # Cost has its own floor, even when total load is identical.
    assert direct(a, scale, scale // 2, scale) == (6, 2)
    assert direct(b, scale, scale // 2, scale) == (6, 3)

    exhaustive_cases = 0
    for d in (2, 3, 5, 10):
        for x in range(16):
            for y in range(16):
                for f in range(12):
                    for p in range(12):
                        assert direct([x, y], f, p, d) == through_residues([x, y], f, p, d)
                        exhaustive_cases += 1

    rng = random.Random(20260910)  # Reproducible test data, never secret masks.
    for _ in range(1000):
        values = [rng.getrandbits(256) for _ in range(rng.randrange(1, 20))]
        fraction, price = rng.getrandbits(256), rng.getrandbits(256)
        assert direct(values, fraction, price, scale) == through_residues(values, fraction, price, scale)

    # Arithmetic correctness of late selection, assuming individual shares exist.
    # This is neither VSS nor a malicious-secure input protocol.
    ring = 1 << 288
    values = [0, 1, u256_max, 1 << 255, 123456789]
    left = [rng.randrange(ring) for _ in values]
    right = [(v - l) % ring for v, l in zip(values, left)]
    for subset in (range(5), [0, 2, 4], [1, 3]):
        result = (sum(left[i] for i in subset) + sum(right[i] for i in subset)) % ring
        assert result == sum(values[i] for i in subset)

    # Scalar field division is not integer floor on each local share.
    assert (70 + 37) % 97 == 10
    assert ((70 * 3 // 2) + (37 * 3 // 2)) % 97 != 10 * 3 // 2

    # Residues + total alone do not decide every individual zero-load condition.
    w = scale * scale
    c, d = [1, 2 * w + 1], [w + 1, w + 1]
    assert sum(c) == sum(d)
    assert [v % w for v in c] == [v % w for v in d]
    assert direct(c, 1, scale, scale) == direct(d, 1, scale, scale)
    assert any(v // scale == 0 for v in c)
    assert all(v // scale > 0 for v in d)

    print(json.dumps({
        "kind": "cleartext arithmetic validation, not a cryptographic benchmark",
        "status": "PASS",
        "exhaustive_residue_identity_cases": exhaustive_cases,
        "random_full_uint256_cases": 1000,
        "sum_bits_for_billion_uint256": 286,
        "two_floor_residue_bits": 40,
        "equal_nominal_different_loads": {"nominal_total": 6, "load_totals": [2, 3]},
        "equal_load_different_costs": {"load_total": 6, "cost_totals": [2, 3]},
        "residues_do_not_prove_all_individual_checks": True,
    }, indent=2))


if __name__ == "__main__":
    main()
