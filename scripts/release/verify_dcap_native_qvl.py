#!/usr/bin/env python3
"""Fail-closed verification of the inactive V1 native-QVL artifact contract."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any


EXPECTED_ROLES = ("qvl", "cxx-runtime", "gcc-runtime")


def load_manifest(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError("native-QVL manifest must be a JSON object")
    return value


def artifact_path(root: Path, configured: str) -> Path:
    path = Path(configured)
    if not path.is_absolute():
        raise ValueError(f"native-QVL artifact path must be absolute: {configured}")
    root = root.resolve()
    candidate = (root / path.relative_to("/")).resolve()
    if candidate != root and root not in candidate.parents:
        raise ValueError(f"native-QVL artifact escapes root: {configured}")
    return candidate


def verify_manifest(manifest: dict[str, Any], root: Path) -> None:
    if manifest.get("schema_version") != 1:
        raise ValueError("unsupported native-QVL manifest schema")
    if manifest.get("target") != "x86_64-unknown-linux-gnu":
        raise ValueError("native-QVL V1 target must be x86_64-unknown-linux-gnu")
    if manifest.get("gramine_version") != "1.9":
        raise ValueError("native-QVL V1 requires exact Gramine 1.9")

    dcap = manifest.get("intel_dcap")
    if not isinstance(dcap, dict):
        raise ValueError("native-QVL manifest is missing intel_dcap")
    expected_boundary = {
        "collateral_major_version": 3,
        "collateral_minor_version": 1,
        "qve_report_info": "null",
        "qve_enabled": False,
        "tvl_enabled": False,
        "qpl_or_pccs_during_consensus": False,
    }
    for field, expected in expected_boundary.items():
        if dcap.get(field) != expected:
            raise ValueError(f"native-QVL boundary mismatch for {field}")

    artifacts = manifest.get("artifacts")
    if not isinstance(artifacts, list):
        raise ValueError("native-QVL manifest artifacts must be an array")
    roles = tuple(artifact.get("role") for artifact in artifacts)
    if roles != EXPECTED_ROLES:
        raise ValueError(f"native-QVL artifact roles must be {EXPECTED_ROLES}")

    seen_paths: set[str] = set()
    seen_install_names: set[str] = set()
    for artifact in artifacts:
        configured = artifact.get("path")
        install_name = artifact.get("install_name")
        expected_size = artifact.get("size")
        expected_sha256 = artifact.get("sha256")
        if not isinstance(configured, str) or configured in seen_paths:
            raise ValueError("native-QVL artifact path is missing or duplicated")
        if not isinstance(install_name, str) or install_name in seen_install_names:
            raise ValueError("native-QVL install name is missing or duplicated")
        if not isinstance(expected_size, int) or expected_size <= 0:
            raise ValueError(f"invalid native-QVL artifact size for {configured}")
        if (
            not isinstance(expected_sha256, str)
            or len(expected_sha256) != 64
            or any(character not in "0123456789abcdef" for character in expected_sha256)
        ):
            raise ValueError(f"invalid native-QVL SHA-256 for {configured}")
        seen_paths.add(configured)
        seen_install_names.add(install_name)

        path = artifact_path(root, configured)
        try:
            payload = path.read_bytes()
        except OSError as error:
            raise ValueError(f"missing native-QVL artifact: {configured}") from error
        if len(payload) != expected_size:
            raise ValueError(f"native-QVL artifact size mismatch: {configured}")
        if hashlib.sha256(payload).hexdigest() != expected_sha256:
            raise ValueError(f"native-QVL artifact SHA-256 mismatch: {configured}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--manifest",
        type=Path,
        default=Path("release/dcap-native-qvl-v1.json"),
    )
    parser.add_argument("--root", type=Path, default=Path("/"))
    args = parser.parse_args()

    try:
        verify_manifest(load_manifest(args.manifest), args.root)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        parser.error(str(error))
    print("native-QVL artifact contract verified")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
