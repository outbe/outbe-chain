#!/usr/bin/env python3
"""Fail-closed verification of the inactive V1 native-QVL artifact contract."""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
from pathlib import Path
from typing import Any


EXPECTED_ROLES = ("qvl", "cxx-runtime", "gcc-runtime")
EXPECTED_STATUS = "inactive-until-i9"
EXPECTED_DCAP = {
    "runtime_package": "libsgx-dcap-quote-verify",
    "runtime_package_version": "1.26.100.1-noble1",
    "development_package": "libsgx-dcap-quote-verify-dev",
    "development_package_version": "1.26.100.1-noble1",
    "headers_package": "libsgx-headers",
    "headers_package_version": "2.29.100.1-noble1",
    "collateral_major_version": 3,
    "collateral_minor_version": 1,
    "qve_report_info": "null",
    "qve_enabled": False,
    "tvl_enabled": False,
    "qpl_or_pccs_during_consensus": False,
}
EXPECTED_ARTIFACTS = (
    {
        "role": "qvl",
        "package": "libsgx-dcap-quote-verify",
        "package_version": "1.26.100.1-noble1",
        "path": "/usr/lib/x86_64-linux-gnu/libsgx_dcap_quoteverify.so.1.13.103.0",
        "install_name": "libsgx_dcap_quoteverify.so.1",
        "size": 5322424,
        "sha256": "4745bc5b46cbdc17a78119ae2db08f54b86ff9077c5ab480f378741396365aef",
        "elf_build_id": "663c0acf2b4673c22c66f01112c5f38d856fd5a5",
    },
    {
        "role": "cxx-runtime",
        "package": "libstdc++6",
        "package_version": "14.2.0-4ubuntu2~24.04.1",
        "path": "/usr/lib/x86_64-linux-gnu/libstdc++.so.6.0.33",
        "install_name": "libstdc++.so.6",
        "size": 2592224,
        "sha256": "1fd75fe70354a416d75aef22bcae68c47bd25d20e2d0568c30b1a9838cf62f11",
    },
    {
        "role": "gcc-runtime",
        "package": "libgcc-s1",
        "package_version": "14.2.0-4ubuntu2~24.04.1",
        "path": "/usr/lib/x86_64-linux-gnu/libgcc_s.so.1",
        "install_name": "libgcc_s.so.1",
        "size": 183024,
        "sha256": "d93224d2b0dab4247598be683adca02f5cf00586f99c187579cd7e92058fb7cb",
    },
)


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


def installed_package_version(package: str) -> str:
    process = subprocess.run(
        ["dpkg-query", "--show", "--showformat=${Version}", package],
        check=False,
        capture_output=True,
        text=True,
    )
    if process.returncode != 0:
        raise ValueError(f"required native-QVL package is not installed: {package}")
    return process.stdout


def verify_installed_packages(dcap: dict[str, Any]) -> None:
    packages = {
        dcap["runtime_package"]: dcap["runtime_package_version"],
        dcap["development_package"]: dcap["development_package_version"],
        dcap["headers_package"]: dcap["headers_package_version"],
    }
    packages.update(
        {
            artifact["package"]: artifact["package_version"]
            for artifact in EXPECTED_ARTIFACTS
        }
    )
    for package, expected_version in packages.items():
        if installed_package_version(package) != expected_version:
            raise ValueError(
                f"native-QVL package version mismatch: {package} "
                f"(expected {expected_version})"
            )


def verify_manifest(
    manifest: dict[str, Any], root: Path, *, check_packages: bool = False
) -> None:
    if manifest.get("schema_version") != 1:
        raise ValueError("unsupported native-QVL manifest schema")
    if manifest.get("status") != EXPECTED_STATUS:
        raise ValueError(f"native-QVL status must be {EXPECTED_STATUS}")
    if manifest.get("target") != "x86_64-unknown-linux-gnu":
        raise ValueError("native-QVL V1 target must be x86_64-unknown-linux-gnu")
    if manifest.get("gramine_version") != "1.9":
        raise ValueError("native-QVL V1 requires exact Gramine 1.9")

    dcap = manifest.get("intel_dcap")
    if not isinstance(dcap, dict):
        raise ValueError("native-QVL manifest is missing intel_dcap")
    if dcap != EXPECTED_DCAP:
        raise ValueError("native-QVL Intel DCAP metadata does not match the exact V1 contract")
    if check_packages:
        verify_installed_packages(dcap)

    artifacts = manifest.get("artifacts")
    if not isinstance(artifacts, list):
        raise ValueError("native-QVL manifest artifacts must be an array")
    if artifacts != list(EXPECTED_ARTIFACTS):
        raise ValueError("native-QVL artifact metadata does not match the exact V1 contract")
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
        verify_manifest(
            load_manifest(args.manifest),
            args.root,
            check_packages=args.root.resolve() == Path("/"),
        )
    except (OSError, ValueError, json.JSONDecodeError) as error:
        parser.error(str(error))
    print("native-QVL artifact contract verified")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
