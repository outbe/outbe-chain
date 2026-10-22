from __future__ import annotations

import tomllib
from pathlib import Path
from typing import Any


class RepositoryChecks:
    def __init__(self, repo_root: Path, pin: dict[str, Any], checks: dict[str, Any]):
        self.root = repo_root
        self.pin = pin
        self.checks = checks
        self.differences: list[str] = []

    def equal(self, label: str, actual: object, expected: object) -> None:
        if actual != expected:
            self.differences.append(f"{label}: expected {expected!r}, found {actual!r}")

    def read_json(self, relative: str) -> dict[str, Any]:
        return self.checks["_read_json"](self.root / relative)

    def verify_elf(self) -> list[str]:
        rust = self.pin["rust"]
        gramine = self.pin["gramine"]
        packages = self.pin["system_packages"]
        rust_toolchain = tomllib.loads(
            (self.root / "rust-toolchain.toml").read_text(encoding="utf-8")
        )["toolchain"]
        self.equal("rust-toolchain channel", rust_toolchain.get("channel"), rust["version"])
        self.equal(
            "rust-toolchain components",
            sorted(rust_toolchain.get("components", [])),
            rust["components"],
        )
        elf = self.read_json("release/reproducible-elf-build-v1.json")
        self.equal(
            "ELF project toolchain contract",
            elf.get("project_toolchain"),
            self.checks["CONTRACT_PATH"],
        )
        self.equal("ELF Rust version", elf.get("rust_toolchain"), rust["version"])
        elf_builder = elf.get("builder", {})
        self.equal("ELF builder recipe", elf_builder.get("recipe"), "Dockerfile.project-toolchain")
        elf_base_images = elf_builder.get("base_images", [])
        if not any(f"rust:{rust['version']}-" in image for image in elf_base_images):
            self.differences.append("ELF builder images do not use the pinned Rust version")
        if not any(
            f"gramineproject/gramine:{gramine['version']}-" in image for image in elf_base_images
        ):
            self.differences.append("ELF builder images do not use the pinned Gramine version")
        self.equal(
            "ELF builder system packages",
            elf_builder.get("system_packages"),
            [f"{name}={version}" for name, version in packages.items()],
        )
        for required_input in (
            self.checks["CONTRACT_PATH"],
            self.checks["APT_SOURCES_PATH"],
            "release/dcap-native-qvl-v1.json",
            "scripts/release/verify_dcap_native_qvl.py",
            "scripts/release/verify_project_toolchain.py",
        ):
            if required_input not in elf.get("inputs", []):
                self.differences.append(f"ELF release inputs do not bind {required_input}")
        return elf_base_images

    def verify_sgx(self) -> None:
        gramine = self.pin["gramine"]
        sgx_profiles = {
            network: self.read_json(f"release/{network}-sgx-bundle-v1.json")
            for network in ("testnet", "mainnet")
        }
        for network, sgx in sgx_profiles.items():
            self.equal(
                f"{network} SGX project toolchain contract",
                sgx.get("project_toolchain"),
                self.checks["CONTRACT_PATH"],
            )
            self.equal(
                f"{network} SGX Gramine version",
                sgx.get("gramine", {}).get("version"),
                gramine["version"],
            )
            self.equal(
                f"{network} SGX Gramine source commit",
                sgx.get("gramine", {}).get("source_commit"),
                gramine["source_commit"],
            )
            for required_input in (
                self.checks["CONTRACT_PATH"],
                "scripts/release/verify_project_toolchain.py",
            ):
                if required_input not in sgx.get("inputs", []):
                    self.differences.append(
                        f"{network} SGX release inputs do not bind {required_input}"
                    )
        for field in (
            "bundle_version",
            "gramine",
            "install_root",
            "platform",
            "sealed_state_schema",
            "sgx",
            "spec_version",
        ):
            self.equal(
                f"SGX physical profile parity for {field}",
                sgx_profiles["mainnet"].get(field),
                sgx_profiles["testnet"].get(field),
            )

    def verify_qvl(self) -> None:
        packages = self.pin["system_packages"]
        qvl = self.read_json("release/dcap-native-qvl-v1.json")
        self.equal(
            "QVL Gramine version", qvl.get("gramine_version"), self.pin["gramine"]["version"]
        )
        qvl_packages = {
            "libsgx-dcap-quote-verify": qvl.get("intel_dcap", {}).get("runtime_package_version"),
            "libsgx-dcap-quote-verify-dev": qvl.get("intel_dcap", {}).get(
                "development_package_version"
            ),
            "libsgx-headers": qvl.get("intel_dcap", {}).get("headers_package_version"),
        }
        for package, actual in qvl_packages.items():
            self.equal(f"QVL {package}", actual, packages[package])
        artifact_versions = {
            artifact.get("package"): artifact.get("package_version")
            for artifact in qvl.get("artifacts", [])
        }
        for package in ("libsgx-dcap-quote-verify", "libstdc++6", "libgcc-s1"):
            self.equal(f"QVL artifact {package}", artifact_versions.get(package), packages[package])

    def verify_recipe(self, elf_base_images: list[str]) -> None:
        rust = self.pin["rust"]
        gramine = self.pin["gramine"]
        recipe = (self.root / "Dockerfile.project-toolchain").read_text(encoding="utf-8")
        if f"rust:{rust['version']}-" not in recipe:
            self.differences.append("project toolchain recipe does not use the pinned Rust version")
        if f"gramineproject/gramine:{gramine['version']}-" not in recipe:
            self.differences.append(
                "project toolchain recipe does not use the pinned Gramine version"
            )
        for package, version in self.pin["system_packages"].items():
            if f"{package}={version}" not in recipe:
                self.differences.append(
                    f"project toolchain recipe does not install {package}={version}"
                )
        for image in elf_base_images:
            if f"FROM {image} " not in recipe:
                self.differences.append(f"project toolchain recipe does not consume {image}")
        if "FROM toolchain AS builder" not in recipe or "FROM scratch AS artifacts" not in recipe:
            self.differences.append(
                "project toolchain recipe does not own the release artifact stages"
            )
        if (
            f"COPY {self.checks['APT_SOURCES_PATH']} /etc/apt/sources.list.d/ubuntu.sources"
            not in recipe
        ):
            self.differences.append(
                "project toolchain recipe does not install the frozen APT sources"
            )

    def verify_sources(self) -> None:
        apt_sources = (self.root / self.checks["APT_SOURCES_PATH"]).read_text(encoding="utf-8")
        uris = [
            line.split(":", 1)[1].strip()
            for line in apt_sources.splitlines()
            if line.startswith("URIs:")
        ]
        if not uris:
            self.differences.append("frozen APT sources declare no URI")
        for uri in uris:
            if not uri.startswith(self.checks["APT_SNAPSHOT_HOST"]):
                self.differences.append(f"frozen APT sources use a mutable archive URI: {uri}")
        for host in self.checks["MUTABLE_APT_HOSTS"]:
            if any(host in line for line in apt_sources.splitlines() if not line.startswith("#")):
                self.differences.append(f"frozen APT sources still reach the mutable host {host}")

    def verify_tee(self) -> None:
        tee_build = (self.root / "crates/system/tee/build.rs").read_text(encoding="utf-8")
        for release_pin in (
            'include_str!("../../../release/project-toolchain-v1.json")',
            'include_str!("../../../release/dcap-native-qvl-v1.json")',
        ):
            if release_pin not in tee_build:
                self.differences.append(
                    f"native-DCAP build contract does not consume {release_pin}"
                )
        for version in self.pin["system_packages"].values():
            if f'"{version}"' in tee_build:
                self.differences.append(
                    f"native-DCAP build contract duplicates project package version {version}"
                )

    def run(self) -> list[str]:
        images = self.verify_elf()
        self.verify_sgx()
        self.verify_qvl()
        self.verify_recipe(images)
        self.verify_sources()
        self.verify_tee()
        return self.differences
