#!/usr/bin/env python3
"""Verify that release consumers match the single Outbe version pin."""

from __future__ import annotations

import argparse
import json
import re
import tomllib
from pathlib import Path
from typing import Any


CONTRACT_PATH = "release/project-toolchain-v1.json"
EXPECTED_TOP_LEVEL_KEYS = {
    "activation",
    "gramine",
    "platform",
    "rust",
    "schema_version",
    "system_packages",
    "target",
}
LOWER_GIT_SHA_RE = re.compile(r"[0-9a-f]{40}")
EXPECTED_SYSTEM_PACKAGES = (
    "build-essential",
    "clang",
    "cmake",
    "gcc-14-base",
    "git",
    "libc++-dev",
    "libc++abi-dev",
    "libgcc-s1",
    "libsgx-dcap-quote-verify",
    "libsgx-dcap-quote-verify-dev",
    "libsgx-headers",
    "libssl-dev",
    "libstdc++6",
    "pkg-config",
)


def _read_json(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return value


def load_and_validate_pin(path: Path) -> dict[str, Any]:
    pin = _read_json(path)
    if set(pin) != EXPECTED_TOP_LEVEL_KEYS:
        raise ValueError("project toolchain pin has an unsupported shape")
    if pin["schema_version"] != 1:
        raise ValueError("unsupported project toolchain pin version")
    if pin["activation"] != "pending-container-delivery":
        raise ValueError("unsupported project toolchain activation state")
    if pin["target"] != "x86_64-unknown-linux-gnu":
        raise ValueError("project toolchain supports only x86_64 Linux")
    if pin["platform"] != "linux/amd64":
        raise ValueError("project toolchain supports only linux/amd64")

    rust = pin["rust"]
    if not isinstance(rust, dict) or set(rust) != {"components", "version"}:
        raise ValueError("project Rust pin has an unsupported shape")
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", rust["version"]):
        raise ValueError("project Rust version must be exact")
    components = rust["components"]
    if (
        not isinstance(components, list)
        or not components
        or components != sorted(set(components))
        or any(not isinstance(component, str) or not component for component in components)
    ):
        raise ValueError("project Rust components must be unique, sorted names")

    gramine = pin["gramine"]
    if not isinstance(gramine, dict) or set(gramine) != {"source_commit", "version"}:
        raise ValueError("project Gramine pin has an unsupported shape")
    if not re.fullmatch(r"[0-9]+\.[0-9]+", gramine["version"]):
        raise ValueError("project Gramine version must be exact")
    if not LOWER_GIT_SHA_RE.fullmatch(gramine["source_commit"]):
        raise ValueError("project Gramine source commit must be an exact lowercase Git SHA")

    packages = pin["system_packages"]
    if (
        not isinstance(packages, dict)
        or not packages
        or any(
            not isinstance(name, str)
            or not name
            or not isinstance(version, str)
            or not version
            or any(character.isspace() for character in version)
            for name, version in packages.items()
        )
    ):
        raise ValueError("project system packages must be sorted exact version pins")
    if tuple(packages) != EXPECTED_SYSTEM_PACKAGES:
        raise ValueError("project toolchain has an unsupported system package set")
    return pin


def _require_equal(
    differences: list[str],
    *,
    label: str,
    actual: object,
    expected: object,
) -> None:
    if actual != expected:
        differences.append(f"{label}: expected {expected!r}, found {actual!r}")


def repository_differences(repo_root: Path) -> list[str]:
    pin = load_and_validate_pin(repo_root / CONTRACT_PATH)
    differences: list[str] = []
    rust = pin["rust"]
    gramine = pin["gramine"]
    packages = pin["system_packages"]

    rust_toolchain = tomllib.loads(
        (repo_root / "rust-toolchain.toml").read_text(encoding="utf-8")
    )["toolchain"]
    _require_equal(
        differences,
        label="rust-toolchain channel",
        actual=rust_toolchain.get("channel"),
        expected=rust["version"],
    )
    _require_equal(
        differences,
        label="rust-toolchain components",
        actual=sorted(rust_toolchain.get("components", [])),
        expected=rust["components"],
    )

    elf = _read_json(repo_root / "release/reproducible-elf-build-v1.json")
    _require_equal(
        differences,
        label="ELF project toolchain contract",
        actual=elf.get("project_toolchain"),
        expected=CONTRACT_PATH,
    )
    _require_equal(
        differences,
        label="ELF Rust version",
        actual=elf.get("rust_toolchain"),
        expected=rust["version"],
    )
    if f"rust:{rust['version']}-" not in elf.get("builder", {}).get("image", ""):
        differences.append("ELF builder image does not use the pinned Rust version")
    for required_input in (CONTRACT_PATH, "scripts/release/verify_project_toolchain.py"):
        if required_input not in elf.get("inputs", []):
            differences.append(f"ELF release inputs do not bind {required_input}")

    sgx = _read_json(repo_root / "release/testnet-sgx-bundle-v1.json")
    _require_equal(
        differences,
        label="SGX project toolchain contract",
        actual=sgx.get("project_toolchain"),
        expected=CONTRACT_PATH,
    )
    _require_equal(
        differences,
        label="SGX Gramine version",
        actual=sgx.get("gramine", {}).get("version"),
        expected=gramine["version"],
    )
    _require_equal(
        differences,
        label="SGX Gramine source commit",
        actual=sgx.get("gramine", {}).get("source_commit"),
        expected=gramine["source_commit"],
    )
    for required_input in (CONTRACT_PATH, "scripts/release/verify_project_toolchain.py"):
        if required_input not in sgx.get("inputs", []):
            differences.append(f"SGX release inputs do not bind {required_input}")

    qvl = _read_json(repo_root / "release/dcap-native-qvl-v1.json")
    _require_equal(
        differences,
        label="QVL Gramine version",
        actual=qvl.get("gramine_version"),
        expected=gramine["version"],
    )
    qvl_packages = {
        "libsgx-dcap-quote-verify": qvl.get("intel_dcap", {}).get(
            "runtime_package_version"
        ),
        "libsgx-dcap-quote-verify-dev": qvl.get("intel_dcap", {}).get(
            "development_package_version"
        ),
        "libsgx-headers": qvl.get("intel_dcap", {}).get("headers_package_version"),
    }
    for package, actual in qvl_packages.items():
        _require_equal(
            differences,
            label=f"QVL {package}",
            actual=actual,
            expected=packages[package],
        )
    artifact_versions = {
        artifact.get("package"): artifact.get("package_version")
        for artifact in qvl.get("artifacts", [])
    }
    for package in ("libsgx-dcap-quote-verify", "libstdc++6", "libgcc-s1"):
        _require_equal(
            differences,
            label=f"QVL artifact {package}",
            actual=artifact_versions.get(package),
            expected=packages[package],
        )

    recipe = (repo_root / "Dockerfile.project-toolchain").read_text(encoding="utf-8")
    if f"rust:{rust['version']}-" not in recipe:
        differences.append("project toolchain recipe does not use the pinned Rust version")
    if f"gramineproject/gramine:{gramine['version']}-" not in recipe:
        differences.append("project toolchain recipe does not use the pinned Gramine version")
    for package, version in packages.items():
        if f"{package}={version}" not in recipe:
            differences.append(
                f"project toolchain recipe does not install {package}={version}"
            )

    tee_build = (repo_root / "crates/system/tee/build.rs").read_text(encoding="utf-8")
    for release_pin in (
        'include_str!("../../../release/project-toolchain-v1.json")',
        'include_str!("../../../release/dcap-native-qvl-v1.json")',
    ):
        if release_pin not in tee_build:
            differences.append(f"native-DCAP build contract does not consume {release_pin}")
    for version in packages.values():
        if f'"{version}"' in tee_build:
            differences.append(
                f"native-DCAP build contract duplicates project package version {version}"
            )
    return differences


def verify_repository(repo_root: Path) -> None:
    differences = repository_differences(repo_root.resolve())
    if differences:
        detail = "\n".join(f"- {difference}" for difference in differences)
        raise ValueError(f"project toolchain version drift:\n{detail}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", type=Path, default=Path.cwd())
    args = parser.parse_args()
    verify_repository(args.repo_root)
    print(
        "project toolchain version pin: OK "
        f"(release activation: {load_and_validate_pin(args.repo_root / CONTRACT_PATH)['activation']})"
    )


if __name__ == "__main__":
    main()
