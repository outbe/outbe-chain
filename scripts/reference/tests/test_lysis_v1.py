"""Tests for the independent Lysis V1 reference implementation."""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


REPOSITORY = Path(__file__).resolve().parents[3]
REFERENCE_PATH = REPOSITORY / "scripts/reference/lysis_v1.py"
SPEC = importlib.util.spec_from_file_location("lysis_v1_reference", REFERENCE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("cannot load Lysis V1 reference")
lysis_v1 = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(lysis_v1)


class LysisV1ReferenceTests(unittest.TestCase):
    def test_signed_division_truncates_toward_zero(self) -> None:
        self.assertEqual(lysis_v1.trunc_signed_div(7, 3), 2)
        self.assertEqual(lysis_v1.trunc_signed_div(-7, 3), -2)
        self.assertEqual(lysis_v1.trunc_signed_div(7, -3), -2)

    def test_single_league_uses_exact_target_fraction(self) -> None:
        self.assertEqual(
            lysis_v1.distribution(
                [lysis_v1.SCALE],
                [1],
                1,
                32 * lysis_v1.SCALE // 100,
                64 * lysis_v1.SCALE // 100,
            ),
            [32 * lysis_v1.SCALE // 100],
        )

    def test_frozen_corpus_is_reference_reproducible(self) -> None:
        vectors = REPOSITORY / "crates/core/lysis/vectors/lysis-v1"
        self.assertEqual(
            lysis_v1.check(
                vectors / "cases.jsonl",
                vectors / "manifest.json",
                REFERENCE_PATH,
            ),
            0,
        )


if __name__ == "__main__":
    unittest.main()
