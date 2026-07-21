#!/usr/bin/env python3
"""Behavioral tests for the versioned Outbe ReleaseManifest contract."""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

from jsonschema import Draft202012Validator, ValidationError


REPO_ROOT = Path(__file__).resolve().parents[3]
GENERATOR_PATH = REPO_ROOT / "scripts/release/generate_release_manifest.py"
SCHEMA_PATH = REPO_ROOT / "release/release-manifest-v1.schema.json"
BUILD_SPEC_PATH = REPO_ROOT / "release/reproducible-elf-build-v1.json"


def load_generator():
    spec = importlib.util.spec_from_file_location("release_manifest", GENERATOR_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {GENERATOR_PATH}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


release_manifest = load_generator()


class ReleaseManifestTests(unittest.TestCase):
    maxDiff = None

    def setUp(self) -> None:
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        self.root = Path(self.tempdir.name)
        self.artifact_dir = self.root / "artifacts"
        self.artifact_dir.mkdir()
        self.input_dir = self.root / "source"
        self.input_dir.mkdir()

        self.spec = {
            "spec_version": 1,
            "target": "x86_64-unknown-linux-gnu",
            "profile": "release",
            "rust_toolchain": "1.96.0",
            "builder": {
                "id": "https://github.com/outbe/outbe-chain/reproducible-elf-builder/v1",
                "image": "rust:1.96.0-bookworm@sha256:" + "1" * 64,
                "debian_snapshot": "20260501T000000Z",
                "system_packages": ["clang=1.0", "cmake=1.0"],
            },
            "environment": {
                "cflags": "-ffile-prefix-map=/workspace=/usr/src/outbe-chain",
                "cxxflags": "-ffile-prefix-map=/workspace=/usr/src/outbe-chain",
                "locale": "C",
                "timezone": "UTC",
                "rustflags": ["--remap-path-prefix=/workspace=.", "-C", "link-arg=-Wl,--build-id=sha1"],
                "zero_ar_date": "1",
            },
            "cargo": {"auditable": False, "locked": True},
            "inputs": ["Cargo.lock", "rust-toolchain.toml"],
            "artifacts": [
                {
                    "name": "outbe-chain",
                    "package": "outbe-chain",
                    "role": "node",
                    "classification": "production",
                    "features": [],
                    "install_profiles": ["full-node", "validator"],
                },
                {
                    "name": "outbe-tee-enclave",
                    "package": "outbe-tee-enclave",
                    "role": "tee-enclave",
                    "classification": "production",
                    "features": [],
                    "install_profiles": ["full-node", "validator"],
                },
            ],
        }
        (self.input_dir / "Cargo.lock").write_bytes(b"locked\n")
        (self.input_dir / "rust-toolchain.toml").write_bytes(b"1.96.0\n")
        (self.artifact_dir / "outbe-chain").write_bytes(b"chain-elf")
        (self.artifact_dir / "outbe-tee-enclave").write_bytes(b"enclave-elf")

    def build(self, **overrides):
        kwargs = {
            "build_spec": self.spec,
            "source_root": self.input_dir,
            "artifact_dir": self.artifact_dir,
            "release_tag": "v0.1.0-test",
            "source_commit": "a" * 40,
            "source_date_epoch": 1_784_000_000,
            "lifecycle": "build-candidate",
            "verification_gates": [],
        }
        kwargs.update(overrides)
        return release_manifest.build_manifest(**kwargs)

    def test_manifest_is_canonical_and_validates_against_schema(self) -> None:
        manifest = self.build()
        schema = json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))
        Draft202012Validator.check_schema(schema)
        Draft202012Validator(schema).validate(manifest)

        first = release_manifest.canonical_json(manifest)
        second = release_manifest.canonical_json(self.build())
        self.assertEqual(first, second)
        self.assertEqual(
            hashlib.sha256(first).hexdigest(),
            "ba39c0ca4c2acba8f5d1ee0e06c68988a1c5b59d7bb42e1d81cf6db52fc7aa4c",
        )
        self.assertTrue(first.endswith(b"\n"))
        self.assertNotIn(str(self.root).encode(), first)

    def test_missing_artifact_fails_closed(self) -> None:
        (self.artifact_dir / "outbe-chain").unlink()
        with self.assertRaisesRegex(ValueError, "missing release artifact: outbe-chain"):
            self.build()

    def test_production_enclave_rejects_mock_feature(self) -> None:
        self.spec["artifacts"][1]["features"] = ["mock"]
        with self.assertRaisesRegex(ValueError, "production enclave.*mock"):
            self.build()

    def test_input_path_cannot_escape_source_root(self) -> None:
        self.spec["inputs"] = ["../outside"]
        with self.assertRaisesRegex(ValueError, "input path escapes source root"):
            self.build()

    def test_changed_source_identity_changes_canonical_manifest(self) -> None:
        first = release_manifest.canonical_json(self.build())
        second = release_manifest.canonical_json(self.build(source_commit="b" * 40))
        self.assertNotEqual(first, second)

    def test_signed_tee_artifact_requires_complete_measurement_identity(self) -> None:
        manifest = self.build()
        signed = copy.deepcopy(manifest["artifacts"][1])
        signed.update(
            {
                "kind": "archive",
                "media_type": "application/x-tar",
                "name": "outbe-tee-enclave-sgx-bundle",
                "path": "release/outbe-tee-enclave-sgx.tar",
                "tee": {
                    "authorization_scope": "testnet",
                    "isv_prod_id": 1,
                    "isv_svn": 1,
                    "mock": False,
                    "mrenclave": "a" * 64,
                    "mrsigner": "b" * 64,
                    "sealed_state_schema": 1,
                    "stage": "signed",
                },
            }
        )
        manifest["artifacts"].append(signed)
        schema = json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))
        Draft202012Validator(schema).validate(manifest)

        del signed["tee"]["mrsigner"]
        with self.assertRaises(ValidationError):
            Draft202012Validator(schema).validate(manifest)


class RepositoryBuildSpecTests(unittest.TestCase):
    def test_spec_declares_exact_current_release_elf_matrix(self) -> None:
        spec = json.loads(BUILD_SPEC_PATH.read_text(encoding="utf-8"))
        artifacts = spec["artifacts"]
        self.assertEqual(
            [artifact["name"] for artifact in artifacts],
            [
                "outbe-chain",
                "outbe-cli",
                "outbe-keygen",
                "outbe-feeder",
                "outbe-tee-enclave",
            ],
        )
        enclave = artifacts[-1]
        self.assertEqual(enclave["classification"], "production")
        self.assertNotIn("mock", enclave["features"])
        self.assertIs(spec["cargo"]["auditable"], False)
        self.assertIn("release/reproducible-verifier-requirements.txt", spec["inputs"])
        self.assertIn("scripts/release/verify_reproducible_elf.py", spec["inputs"])


if __name__ == "__main__":
    unittest.main()
