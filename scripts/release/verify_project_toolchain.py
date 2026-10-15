#!/usr/bin/env python3
"""Verify that release consumers match the single Outbe version pin."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import re
import tomllib
from pathlib import Path
from typing import Any


CONTRACT_PATH = "release/project-toolchain-v1.json"
APT_SOURCES_PATH = "release/apt/ubuntu.sources"
EXPECTED_TOP_LEVEL_KEYS = {
    "activation",
    "apt_provenance",
    "gramine",
    "host_only_packages",
    "platform",
    "rust",
    "schema_version",
    "system_packages",
    "target",
    "zk_tools",
}
SHA256_RE = re.compile(r"[0-9a-f]{64}")
EXPECTED_APT_SOURCE_PATHS = (
    "/etc/apt/sources.list.d/gramine.list",
    "/etc/apt/sources.list.d/intel-sgx.list",
    "/etc/apt/sources.list.d/ubuntu.sources",
    "/usr/share/keyrings/gramine-keyring.gpg",
    "/usr/share/keyrings/intel-sgx-deb.key",
    "/usr/share/keyrings/ubuntu-archive-keyring.gpg",
)
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
EXPECTED_HOST_ONLY_PACKAGES = ("libsgx-dcap-default-qpl",)
EXPECTED_ZK_TOOLS = ("barretenberg", "barretenberg_static", "nargo")


def _read_json(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return value


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load_and_validate_pin(path: Path) -> dict[str, Any]:
    pin = _read_json(path)
    _validate_pin_header(pin)
    _validate_apt_provenance(pin["apt_provenance"])
    _validate_rust(pin["rust"])
    _validate_gramine(pin["gramine"])
    _validate_packages(pin["system_packages"], pin["host_only_packages"])
    _validate_zk_tools(pin["zk_tools"])
    return pin


def _validate_pin_header(pin: dict[str, Any]) -> None:
    if set(pin) != EXPECTED_TOP_LEVEL_KEYS:
        raise ValueError("project toolchain pin has an unsupported shape")
    for field, expected, message in (
        ("schema_version", 1, "unsupported project toolchain pin version"),
        ("activation", "pending-container-delivery", "unsupported project toolchain activation state"),
        ("target", "x86_64-unknown-linux-gnu", "project toolchain supports only x86_64 Linux"),
        ("platform", "linux/amd64", "project toolchain supports only linux/amd64"),
    ):
        if pin[field] != expected:
            raise ValueError(message)


def _has_keys(value: Any, keys: set[str]) -> bool:
    return isinstance(value, dict) and set(value) == keys


def _validate_shape(value: Any, keys: set[str], message: str) -> None:
    if not _has_keys(value, keys):
        raise ValueError(message)


def _validate_pattern(pattern: str, value: str, message: str) -> None:
    if not re.fullmatch(pattern, value):
        raise ValueError(message)


def _validate_apt_provenance(apt: Any) -> None:
    _validate_shape(
        apt, {"deb_closure", "source_files"}, "project APT provenance has an unsupported shape"
    )
    closure = apt["deb_closure"]
    if not _valid_deb_closure(closure):
        raise ValueError("project APT package closure must have an exact digest and count")
    source_files = apt["source_files"]
    if (
        not isinstance(source_files, list)
        or any(not isinstance(source, dict) for source in source_files)
        or tuple(source.get("path") for source in source_files)
        != EXPECTED_APT_SOURCE_PATHS
        or any(
            set(source) != {"path", "sha256"}
            or not SHA256_RE.fullmatch(source.get("sha256", ""))
            for source in source_files
        )
    ):
        raise ValueError("project APT source and key files must be hash-pinned")


def _valid_deb_closure(closure: Any) -> bool:
    if not _has_keys(closure, {"count", "format", "sha256"}):
        return False
    count = closure["count"]
    if not isinstance(count, int) or isinstance(count, bool) or count <= 0:
        return False
    return (
        closure["format"] == "sha256sum-lines-sorted-by-filename-v1"
        and bool(SHA256_RE.fullmatch(closure["sha256"]))
    )


def _validate_rust(rust: Any) -> None:
    _validate_shape(rust, {"components", "version"}, "project Rust pin has an unsupported shape")
    _validate_pattern(r"[0-9]+\.[0-9]+\.[0-9]+", rust["version"], "project Rust version must be exact")
    components = rust["components"]
    if (
        not isinstance(components, list)
        or not components
        or components != sorted(set(components))
        or any(not isinstance(component, str) or not component for component in components)
    ):
        raise ValueError("project Rust components must be unique, sorted names")


def _validate_gramine(gramine: Any) -> None:
    _validate_shape(
        gramine, {"source_commit", "version"}, "project Gramine pin has an unsupported shape"
    )
    _validate_pattern(r"[0-9]+\.[0-9]+", gramine["version"], "project Gramine version must be exact")
    _validate_pattern(
        LOWER_GIT_SHA_RE.pattern, gramine["source_commit"],
        "project Gramine source commit must be an exact lowercase Git SHA",
    )


def _exact_version(version: Any) -> bool:
    return isinstance(version, str) and bool(version) and not any(
        character.isspace() for character in version
    )


def _valid_package(name: Any, version: Any) -> bool:
    return isinstance(name, str) and bool(name) and _exact_version(version)


def _validate_packages(packages: Any, host_only_packages: Any) -> None:
    if (
        not isinstance(packages, dict)
        or not packages
        or any(not _valid_package(name, version) for name, version in packages.items())
    ):
        raise ValueError("project system packages must be sorted exact version pins")
    if tuple(packages) != EXPECTED_SYSTEM_PACKAGES:
        raise ValueError("project toolchain has an unsupported system package set")
    if (
        not isinstance(host_only_packages, dict)
        or tuple(host_only_packages) != EXPECTED_HOST_ONLY_PACKAGES
        or any(not _exact_version(version) for version in host_only_packages.values())
    ):
        raise ValueError("project toolchain has an unsupported host-only package set")
    if set(host_only_packages).intersection(packages):
        raise ValueError("host-only packages must not enter the release build image")


def _valid_zk_archive(tool: Any) -> bool:
    if not _has_keys(tool, {"archive_sha256", "archive_url", "version"}):
        return False
    if not _exact_version(tool["version"]):
        return False
    if not isinstance(tool["archive_sha256"], str) or not SHA256_RE.fullmatch(tool["archive_sha256"]):
        return False
    url = tool["archive_url"]
    return (
        isinstance(url, str)
        and url.startswith("https://github.com/")
        and f"/v{tool['version']}/" in url
    )


def _validate_zk_tools(zk_tools: Any) -> None:
    if not isinstance(zk_tools, dict) or tuple(zk_tools) != EXPECTED_ZK_TOOLS:
        raise ValueError("project toolchain has an unsupported ZK tool set")
    for name, tool in zk_tools.items():
        if not _valid_zk_archive(tool):
            raise ValueError(f"project {name} tool must use an exact GitHub archive pin")
    if zk_tools["barretenberg_static"]["version"] != zk_tools["barretenberg"]["version"]:
        raise ValueError("Barretenberg CLI and static library versions must match")
    static = zk_tools["barretenberg_static"]
    if static["archive_url"] != (
        "https://github.com/AztecProtocol/barretenberg/releases/download/"
        f"v{static['version']}/barretenberg-static-amd64-linux.tar.gz"
    ):
        raise ValueError("Barretenberg static library must match the Linux amd64 release target")


def apt_ledger(deb_dir: Path) -> tuple[str, str, int]:
    """Return the closure ledger, its digest and its package count."""

    debs = sorted(deb_dir.resolve().glob("*.deb"), key=lambda path: path.name)
    if any(not path.is_file() or path.is_symlink() for path in debs):
        raise ValueError("APT package closure contains a non-regular artifact")
    ledger = "".join(f"{_sha256_file(path)}  {path.name}\n" for path in debs)
    return ledger, hashlib.sha256(ledger.encode("ascii")).hexdigest(), len(debs)


def verify_apt_inputs(*, pin_path: Path, root: Path, deb_dir: Path) -> None:
    """Verify the base APT authority and downloaded package closure before install."""

    pin = load_and_validate_pin(pin_path)
    root = root.resolve()
    for source in pin["apt_provenance"]["source_files"]:
        _verify_apt_source(source, root)

    ledger, actual_digest, count = apt_ledger(deb_dir)
    closure = pin["apt_provenance"]["deb_closure"]
    if count != closure["count"]:
        raise ValueError(
            f"APT package closure count mismatch: expected {closure['count']}, found {count}\n"
            f"resolved closure:\n{ledger}"
        )
    if actual_digest != closure["sha256"]:
        raise ValueError(
            "APT package closure digest mismatch: "
            f"expected {closure['sha256']}, found {actual_digest}\n"
            f"resolved closure:\n{ledger}"
        )


def _verify_apt_source(source: dict[str, str], root: Path) -> None:
    candidate = root / source["path"].lstrip("/")
    if not candidate.is_file() or candidate.is_symlink():
        raise ValueError(f"missing or non-regular APT provenance input: {source['path']}")
    if _sha256_file(candidate) != source["sha256"]:
        raise ValueError(f"APT provenance digest mismatch: {source['path']}")


def repository_differences(repo_root: Path) -> list[str]:
    pin = load_and_validate_pin(repo_root / CONTRACT_PATH)
    # Container consumers copy this verifier alone and only use pin/APT validation.
    helper_path = Path(__file__).with_name("project_toolchain_consumers.py")
    spec = importlib.util.spec_from_file_location("project_toolchain_consumers", helper_path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {helper_path}")
    consumers = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(consumers)
    differences: list[str] = []

    rust_toolchain = tomllib.loads(
        (repo_root / "rust-toolchain.toml").read_text(encoding="utf-8")
    )["toolchain"]
    consumers.rust_differences(rust_toolchain, pin["rust"], differences)

    elf = _read_json(repo_root / "release/reproducible-elf-build-v1.json")
    consumers.elf_differences(elf, pin, CONTRACT_PATH, APT_SOURCES_PATH, differences)

    sgx_profiles = {
        network: _read_json(repo_root / f"release/{network}-sgx-bundle-v1.json")
        for network in ("testnet", "mainnet")
    }
    consumers.sgx_differences(sgx_profiles, pin["gramine"], CONTRACT_PATH, differences)

    qvl = _read_json(repo_root / "release/dcap-native-qvl-v1.json")
    consumers.qvl_differences(qvl, pin, differences)

    recipe = (repo_root / "Dockerfile.project-toolchain").read_text(encoding="utf-8")
    consumers.recipe_differences(
        recipe, pin, elf.get("builder", {}).get("base_images", []), APT_SOURCES_PATH, differences
    )

    apt_sources = (repo_root / APT_SOURCES_PATH).read_text(encoding="utf-8")
    consumers.apt_source_differences(apt_sources, differences)

    tee_build = (repo_root / "crates/system/tee/build.rs").read_text(encoding="utf-8")
    consumers.tee_build_differences(tee_build, pin["system_packages"], differences)
    return differences


def verify_repository(repo_root: Path) -> None:
    differences = repository_differences(repo_root.resolve())
    if differences:
        detail = "\n".join(f"- {difference}" for difference in differences)
        raise ValueError(f"project toolchain version drift:\n{detail}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", type=Path, default=Path.cwd())
    parser.add_argument("--verify-apt-inputs", action="store_true")
    parser.add_argument("--emit-apt-ledger", action="store_true")
    parser.add_argument("--pin", type=Path)
    parser.add_argument("--apt-root", type=Path)
    parser.add_argument("--deb-dir", type=Path)
    args = parser.parse_args()
    if args.emit_apt_ledger:
        if args.deb_dir is None:
            parser.error("--emit-apt-ledger requires --deb-dir")
        ledger, digest, count = apt_ledger(args.deb_dir)
        print(ledger, end="")
        print(json.dumps({"count": count, "sha256": digest}))
        return
    if args.verify_apt_inputs:
        if args.pin is None or args.apt_root is None or args.deb_dir is None:
            parser.error("--verify-apt-inputs requires --pin, --apt-root and --deb-dir")
        verify_apt_inputs(pin_path=args.pin, root=args.apt_root, deb_dir=args.deb_dir)
        print("project APT source and package closure: OK")
        return
    verify_repository(args.repo_root)
    print(
        "project toolchain version pin: OK "
        f"(release activation: {load_and_validate_pin(args.repo_root / CONTRACT_PATH)['activation']})"
    )


if __name__ == "__main__":
    main()
