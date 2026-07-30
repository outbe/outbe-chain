#!/usr/bin/env python3
"""Behavioral tests for the native-QVL artifact contract verifier."""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import tempfile
import unittest
from pathlib import Path


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
        for index, role in enumerate(verifier.EXPECTED_ROLES):
            payload = f"artifact-{role}".encode()
            path = f"/native/{index}"
            target = self.root / path.removeprefix("/")
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(payload)
            self.artifacts.append(
                {
                    "role": role,
                    "package": role,
                    "package_version": "1",
                    "path": path,
                    "install_name": f"{role}.so",
                    "size": len(payload),
                    "sha256": hashlib.sha256(payload).hexdigest(),
                }
            )
        self.manifest = {
            "schema_version": 1,
            "status": "inactive-until-i9",
            "target": "x86_64-unknown-linux-gnu",
            "gramine_version": "1.9",
            "intel_dcap": {
                "collateral_major_version": 3,
                "collateral_minor_version": 1,
                "qve_report_info": "null",
                "qve_enabled": False,
                "tvl_enabled": False,
                "qpl_or_pccs_during_consensus": False,
            },
            "artifacts": self.artifacts,
        }

    def test_exact_artifacts_and_boundary_pass(self) -> None:
        verifier.verify_manifest(self.manifest, self.root)

    def test_changed_artifact_fails_closed(self) -> None:
        (self.root / "native/0").write_bytes(b"substituted")
        with self.assertRaisesRegex(ValueError, "size mismatch"):
            verifier.verify_manifest(self.manifest, self.root)

    def test_qve_or_qpl_boundary_change_fails_closed(self) -> None:
        changed = copy.deepcopy(self.manifest)
        changed["intel_dcap"]["qve_enabled"] = True
        with self.assertRaisesRegex(ValueError, "qve_enabled"):
            verifier.verify_manifest(changed, self.root)

        changed = copy.deepcopy(self.manifest)
        changed["intel_dcap"]["qpl_or_pccs_during_consensus"] = True
        with self.assertRaisesRegex(ValueError, "qpl_or_pccs"):
            verifier.verify_manifest(changed, self.root)


if __name__ == "__main__":
    unittest.main()
