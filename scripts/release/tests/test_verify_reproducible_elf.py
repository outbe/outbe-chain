#!/usr/bin/env python3
"""Tests for the two-build reproducibility verifier."""

from __future__ import annotations

import importlib.util
import json
import shutil
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]


def load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


generator = load_module(
    "release_manifest_for_verifier_tests",
    REPO_ROOT / "scripts/release/generate_release_manifest.py",
)
verifier = load_module(
    "verify_reproducible_elf",
    REPO_ROOT / "scripts/release/verify_reproducible_elf.py",
)


class ReproducibleElfVerifierTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        root = Path(self.tempdir.name)
        self.first = root / "first"
        self.second = root / "second"
        (self.first / "bin").mkdir(parents=True)

        self.build_spec = json.loads(
            (REPO_ROOT / "release/reproducible-elf-build-v1.json").read_text(encoding="utf-8")
        )
        for index, artifact in enumerate(self.build_spec["artifacts"]):
            (self.first / "bin" / artifact["name"]).write_bytes(
                b"\x7fELF" + bytes([index]) + b"deterministic-test"
            )
        manifest = generator.build_manifest(
            build_spec=self.build_spec,
            source_root=REPO_ROOT,
            artifact_dir=self.first / "bin",
            release_tag="test-rebuild",
            source_commit="a" * 40,
            source_date_epoch=1_784_000_000,
            lifecycle="build-candidate",
            verification_gates=[],
        )
        (self.first / "release-manifest.json").write_bytes(generator.canonical_json(manifest))
        shutil.copytree(self.first, self.second)

    def test_identical_outputs_pass_with_per_artifact_evidence(self) -> None:
        evidence = verifier.verify_outputs(self.first, self.second, REPO_ROOT)
        self.assertEqual(evidence["result"], "passed")
        self.assertEqual(len(evidence["artifacts"]), 5)
        self.assertEqual(evidence["differences"], [])

    def test_changed_artifact_fails_with_named_difference(self) -> None:
        with (self.second / "bin/outbe-cli").open("ab") as output:
            output.write(b"changed")
        evidence = verifier.verify_outputs(self.first, self.second, REPO_ROOT)
        self.assertEqual(evidence["result"], "failed")
        self.assertTrue(any("outbe-cli" in difference for difference in evidence["differences"]))

    def test_embedded_builder_path_is_rejected_even_when_builds_match(self) -> None:
        for output in (self.first, self.second):
            path = output / "bin/outbe-feeder"
            path.write_bytes(path.read_bytes() + b"/workspace/secret")
            manifest = generator.build_manifest(
                build_spec=self.build_spec,
                source_root=REPO_ROOT,
                artifact_dir=output / "bin",
                release_tag="test-rebuild",
                source_commit="a" * 40,
                source_date_epoch=1_784_000_000,
                lifecycle="build-candidate",
                verification_gates=[],
            )
            (output / "release-manifest.json").write_bytes(generator.canonical_json(manifest))
        evidence = verifier.verify_outputs(self.first, self.second, REPO_ROOT)
        self.assertEqual(evidence["result"], "failed")
        self.assertTrue(any("forbidden absolute build path" in item for item in evidence["differences"]))


if __name__ == "__main__":
    unittest.main()
