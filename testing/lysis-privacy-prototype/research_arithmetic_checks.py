"""Exact algebra checks for the research report; no cryptographic benchmark."""

import json
import random


def ceil_div(a, b):
    return (a + b - 1) // b


def original(a, d, age, maximum, scale, slots):
    efficiency = 0 if d == 0 else a * scale // d
    rcfi = age * efficiency // scale
    return 1 if maximum == 0 else 1 + min(rcfi * slots // maximum, slots - 1)


def comparison_form(a, d, age, maximum, scale, slots):
    if d == 0 or age == 0 or maximum == 0:
        return 1
    lo, hi = 0, slots - 1
    while lo < hi:
        mid = (lo + hi + 1) // 2
        rcfi_threshold = ceil_div(mid * maximum, slots)
        efficiency_threshold = ceil_div(rcfi_threshold * scale, age)
        if a * scale >= efficiency_threshold * d:
            lo = mid
        else:
            hi = mid - 1
    return 1 + lo


def main():
    count = 0
    for scale in [1, 3, 10]:
        for d in range(21):
            for a in range(d + 1):
                for age in range(13):
                    for maximum in range(13):
                        args = (a, d, age, maximum, scale, 8)
                        assert original(*args) == comparison_form(*args), args
                        count += 1
    rng = random.Random(20260911)
    for _ in range(20000):
        d = rng.randrange(1, 1 << 400)
        a = rng.randrange(d + 1)
        age = rng.randrange(1, 526583689924471619585)
        maximum = rng.randrange(1, 526583689924471619585)
        args = (a, d, age, maximum, 10**18, 4096)
        assert original(*args) == comparison_form(*args), args
        count += 1

    amax = ((1 << 64) * 10**6 - 1) * 10**6
    smax = 10**9 * amax
    q_baby = 2736030358979909402780800718157159386076813972158567259200215660948447373041
    q_bn = 21888242871839275222246405745257275088548364400416034343698204186575808495617
    assert smax < q_baby < q_bn
    assert (((1 << 32) - 1) * amax).bit_length() == 136
    report = {
        "scope": "Integer formula equivalence and sizing arithmetic only; no MPC, proof, or throughput benchmark",
        "fidelity_equivalence_cases": count,
        "fidelity_limitation": "Public time weights; mathematical nonnegative integers. Existing U256 overflow/failure semantics are not tested.",
        "amax_bits": amax.bit_length(),
        "smax_billion_bits": smax.bit_length(),
        "source_aggregate_fits_both_candidate_fields": True,
        "admissions_per_second_single_50h_window": 10**9 / (50 * 3600),
        "admissions_per_second_one_billion_per_day_steady": 10**9 / 86400,
        "primary_shards_256": 10**9 // 256,
        "raw_32byte_commitments_bytes": 32 * 10**9,
        "raw_64byte_share_pairs_per_holder_bytes": 64 * 10**9,
        "raw_64byte_share_pairs_16_holders_bytes": 64 * 16 * 10**9,
        "baseline_6_threshold_extra_coefficient_commitments_bytes": 32 * 5 * 10**9,
        "uniform_50h_arrival_hourly_moves_live_records": (sum(range(1, 50)) / 50 + 12) * 10**9,
    }
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
