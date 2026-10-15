"""Compare repository release consumers with the validated project toolchain pin."""

from __future__ import annotations

from typing import Any


def require_equal(
    differences: list[str], *, label: str, actual: object, expected: object
) -> None:
    if actual != expected:
        differences.append(f"{label}: expected {expected!r}, found {actual!r}")


def rust_differences(toolchain: dict, rust: dict, differences: list[str]) -> None:
    require_equal(
        differences,
        label="rust-toolchain channel",
        actual=toolchain.get("channel"),
        expected=rust["version"],
    )
    require_equal(
        differences,
        label="rust-toolchain components",
        actual=sorted(toolchain.get("components", [])),
        expected=rust["components"],
    )


def elf_differences(
    elf: dict, pin: dict, contract_path: str, apt_sources_path: str, differences: list[str]
) -> None:
    require_equal(
        differences,
        label="ELF project toolchain contract",
        actual=elf.get("project_toolchain"),
        expected=contract_path,
    )
    require_equal(
        differences,
        label="ELF Rust version",
        actual=elf.get("rust_toolchain"),
        expected=pin["rust"]["version"],
    )
    builder = elf.get("builder", {})
    require_equal(
        differences,
        label="ELF builder recipe",
        actual=builder.get("recipe"),
        expected="Dockerfile.project-toolchain",
    )
    base_images = builder.get("base_images", [])
    if not any(f"rust:{pin['rust']['version']}-" in image for image in base_images):
        differences.append("ELF builder images do not use the pinned Rust version")
    if not any(f"gramineproject/gramine:{pin['gramine']['version']}-" in image for image in base_images):
        differences.append("ELF builder images do not use the pinned Gramine version")
    require_equal(
        differences,
        label="ELF builder system packages",
        actual=builder.get("system_packages"),
        expected=[f"{name}={version}" for name, version in pin["system_packages"].items()],
    )
    for required_input in (
        contract_path,
        apt_sources_path,
        "release/dcap-native-qvl-v1.json",
        "scripts/release/verify_dcap_native_qvl.py",
        "scripts/release/verify_project_toolchain.py",
    ):
        if required_input not in elf.get("inputs", []):
            differences.append(f"ELF release inputs do not bind {required_input}")


def sgx_differences(
    profiles: dict, gramine: dict, contract_path: str, differences: list[str]
) -> None:
    for network, sgx in profiles.items():
        require_equal(
            differences,
            label=f"{network} SGX project toolchain contract",
            actual=sgx.get("project_toolchain"),
            expected=contract_path,
        )
        require_equal(
            differences,
            label=f"{network} SGX Gramine version",
            actual=sgx.get("gramine", {}).get("version"),
            expected=gramine["version"],
        )
        require_equal(
            differences,
            label=f"{network} SGX Gramine source commit",
            actual=sgx.get("gramine", {}).get("source_commit"),
            expected=gramine["source_commit"],
        )
        for required_input in (contract_path, "scripts/release/verify_project_toolchain.py"):
            if required_input not in sgx.get("inputs", []):
                differences.append(f"{network} SGX release inputs do not bind {required_input}")
    for field in (
        "bundle_version",
        "gramine",
        "install_root",
        "platform",
        "sealed_state_schema",
        "sgx",
        "spec_version",
    ):
        require_equal(
            differences,
            label=f"SGX physical profile parity for {field}",
            actual=profiles["mainnet"].get(field),
            expected=profiles["testnet"].get(field),
        )


def qvl_differences(qvl: dict, pin: dict, differences: list[str]) -> None:
    require_equal(
        differences,
        label="QVL Gramine version",
        actual=qvl.get("gramine_version"),
        expected=pin["gramine"]["version"],
    )
    packages = pin["system_packages"]
    qvl_packages = {
        "libsgx-dcap-quote-verify": qvl.get("intel_dcap", {}).get("runtime_package_version"),
        "libsgx-dcap-quote-verify-dev": qvl.get("intel_dcap", {}).get("development_package_version"),
        "libsgx-headers": qvl.get("intel_dcap", {}).get("headers_package_version"),
    }
    for package, actual in qvl_packages.items():
        require_equal(differences, label=f"QVL {package}", actual=actual, expected=packages[package])
    artifact_versions = {
        artifact.get("package"): artifact.get("package_version")
        for artifact in qvl.get("artifacts", [])
    }
    for package in ("libsgx-dcap-quote-verify", "libstdc++6", "libgcc-s1"):
        require_equal(
            differences,
            label=f"QVL artifact {package}",
            actual=artifact_versions.get(package),
            expected=packages[package],
        )


def recipe_differences(
    recipe: str, pin: dict, base_images: list, apt_sources_path: str, differences: list[str]
) -> None:
    for name, tool in pin["zk_tools"].items():
        archive_instruction = (
            f"ADD --checksum=sha256:{tool['archive_sha256']} \\\n"
            f"    {tool['archive_url']} \\\n"
        )
        if archive_instruction not in recipe:
            differences.append(f"project toolchain recipe does not checksum-pin {name}")
    if f"rust:{pin['rust']['version']}-" not in recipe:
        differences.append("project toolchain recipe does not use the pinned Rust version")
    if f"gramineproject/gramine:{pin['gramine']['version']}-" not in recipe:
        differences.append("project toolchain recipe does not use the pinned Gramine version")
    for package, version in pin["system_packages"].items():
        if f"{package}={version}" not in recipe:
            differences.append(f"project toolchain recipe does not install {package}={version}")
    for image in base_images:
        if f"FROM {image} " not in recipe:
            differences.append(f"project toolchain recipe does not consume {image}")
    if "FROM toolchain AS builder" not in recipe or "FROM scratch AS artifacts" not in recipe:
        differences.append("project toolchain recipe does not own the release artifact stages")
    if f"COPY {apt_sources_path} /etc/apt/sources.list.d/ubuntu.sources" not in recipe:
        differences.append("project toolchain recipe does not install the frozen APT sources")


def apt_source_differences(sources: str, differences: list[str]) -> None:
    uris = [
        line.split(":", 1)[1].strip()
        for line in sources.splitlines()
        if line.startswith("URIs:")
    ]
    if not uris:
        differences.append("frozen APT sources declare no URI")
    for uri in uris:
        if not uri.startswith("https://snapshot.ubuntu.com/ubuntu/"):
            differences.append(f"frozen APT sources use a mutable archive URI: {uri}")
    for host in ("archive.ubuntu.com", "security.ubuntu.com", "ports.ubuntu.com"):
        if any(host in line for line in sources.splitlines() if not line.startswith("#")):
            differences.append(f"frozen APT sources still reach the mutable host {host}")


def tee_build_differences(source: str, packages: dict[str, Any], differences: list[str]) -> None:
    for release_pin in (
        'include_str!("../../../release/project-toolchain-v1.json")',
        'include_str!("../../../release/dcap-native-qvl-v1.json")',
    ):
        if release_pin not in source:
            differences.append(f"native-DCAP build contract does not consume {release_pin}")
    for version in packages.values():
        if f'"{version}"' in source:
            differences.append(f"native-DCAP build contract duplicates project package version {version}")
