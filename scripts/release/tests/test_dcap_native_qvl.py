#!/usr/bin/env python3
"""Behavioral tests for the native-QVL artifact contract verifier."""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest import mock


REPO_ROOT = Path(__file__).resolve().parents[3]
VERIFIER_PATH = REPO_ROOT / "scripts/release/verify_dcap_native_qvl.py"


def load_verifier():
    spec = importlib.util.spec_from_file_location("verify_dcap_native_qvl", VERIFIER_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {VERIFIER_PATH}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


verifier = load_verifier()


class NativeQvlManifestTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        self.root = Path(self.tempdir.name)
        self.artifacts = []
        for index, expected in enumerate(verifier.EXPECTED_ARTIFACTS):
            role = expected["role"]
            payload = f"artifact-{role}".encode()
            path = f"/native/{index}"
            target = self.root / path.removeprefix("/")
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(payload)
            artifact = dict(expected)
            artifact["path"] = path
            artifact["size"] = len(payload)
            artifact["sha256"] = hashlib.sha256(payload).hexdigest()
            self.artifacts.append(artifact)
        self.manifest = {
            "schema_version": 1,
            "status": verifier.EXPECTED_STATUS,
            "target": "x86_64-unknown-linux-gnu",
            "gramine_version": "1.9",
            "intel_dcap": dict(verifier.EXPECTED_DCAP),
            "artifacts": self.artifacts,
        }

    def verify_synthetic_manifest(self, manifest=None) -> None:
        with mock.patch.object(
            verifier, "EXPECTED_ARTIFACTS", tuple(self.artifacts)
        ):
            verifier.verify_manifest(manifest or self.manifest, self.root)

    def test_exact_artifacts_and_boundary_pass(self) -> None:
        self.verify_synthetic_manifest()

    def test_changed_artifact_fails_closed(self) -> None:
        (self.root / "native/0").write_bytes(b"substituted")
        with self.assertRaisesRegex(ValueError, "size mismatch"):
            self.verify_synthetic_manifest()

    def test_status_change_fails_closed(self) -> None:
        changed = copy.deepcopy(self.manifest)
        changed["status"] = "active"
        with self.assertRaisesRegex(ValueError, "status"):
            self.verify_synthetic_manifest(changed)

    def test_qve_or_qpl_boundary_change_fails_closed(self) -> None:
        changed = copy.deepcopy(self.manifest)
        changed["intel_dcap"]["qve_enabled"] = True
        with self.assertRaisesRegex(ValueError, "Intel DCAP metadata"):
            self.verify_synthetic_manifest(changed)

        changed = copy.deepcopy(self.manifest)
        changed["intel_dcap"]["qpl_or_pccs_during_consensus"] = True
        with self.assertRaisesRegex(ValueError, "Intel DCAP metadata"):
            self.verify_synthetic_manifest(changed)

    def test_package_or_build_id_change_fails_closed(self) -> None:
        changed = copy.deepcopy(self.manifest)
        changed["intel_dcap"]["development_package_version"] = "substituted"
        with self.assertRaisesRegex(ValueError, "Intel DCAP metadata"):
            self.verify_synthetic_manifest(changed)

        changed = copy.deepcopy(self.manifest)
        changed["artifacts"][0]["elf_build_id"] = "substituted"
        with self.assertRaisesRegex(ValueError, "artifact metadata"):
            self.verify_synthetic_manifest(changed)


if __name__ == "__main__":
    unittest.main()
