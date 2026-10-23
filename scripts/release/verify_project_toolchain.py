#!/usr/bin/env python3
"""Verify that release consumers match the single Outbe version pin."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import re
from pathlib import Path
from typing import Any

CONTRACT_PATH = "release/project-toolchain-v1.json"
APT_SOURCES_PATH = "release/apt/ubuntu.sources"
APT_SNAPSHOT_HOST = "https://snapshot.ubuntu.com/ubuntu/"
MUTABLE_APT_HOSTS = ("archive.ubuntu.com", "security.ubuntu.com", "ports.ubuntu.com")
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
EXPECTED_ZK_TOOLS = ("barretenberg", "nargo")


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

    _validate_apt(pin["apt_provenance"])
    _validate_rust(pin["rust"])
    _validate_gramine(pin["gramine"])
    _validate_packages(pin)
    _validate_zk_tools(pin["zk_tools"])
    return pin


def _valid_closure(closure: Any) -> bool:
    if not isinstance(closure, dict) or set(closure) != {"count", "format", "sha256"}:
        return False
    count = closure["count"]
    if not isinstance(count, int) or isinstance(count, bool) or count <= 0:
        return False
    return closure["format"] == "sha256sum-lines-sorted-by-filename-v1" and bool(
        SHA256_RE.fullmatch(closure["sha256"])
    )


def _validate_apt(apt: Any) -> None:
    if not isinstance(apt, dict) or set(apt) != {"deb_closure", "source_files"}:
        raise ValueError("project APT provenance has an unsupported shape")
    closure = apt["deb_closure"]
    if not _valid_closure(closure):
        raise ValueError("project APT package closure must have an exact digest and count")
    source_files = apt["source_files"]
    if (
        not isinstance(source_files, list)
        or any(not isinstance(source, dict) for source in source_files)
        or tuple(source.get("path") for source in source_files) != EXPECTED_APT_SOURCE_PATHS
        or any(
            set(source) != {"path", "sha256"} or not SHA256_RE.fullmatch(source.get("sha256", ""))
            for source in source_files
        )
    ):
        raise ValueError("project APT source and key files must be hash-pinned")


def _validate_rust(rust: Any) -> None:
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


def _validate_gramine(gramine: Any) -> None:
    if not isinstance(gramine, dict) or set(gramine) != {"source_commit", "version"}:
        raise ValueError("project Gramine pin has an unsupported shape")
    if not re.fullmatch(r"[0-9]+\.[0-9]+", gramine["version"]):
        raise ValueError("project Gramine version must be exact")
    if not LOWER_GIT_SHA_RE.fullmatch(gramine["source_commit"]):
        raise ValueError("project Gramine source commit must be an exact lowercase Git SHA")


def _valid_package_version(version: Any) -> bool:
    return (
        isinstance(version, str)
        and bool(version)
        and not any(character.isspace() for character in version)
    )


def _validate_packages(pin: dict[str, Any]) -> None:
    packages = pin["system_packages"]
    if (
        not isinstance(packages, dict)
        or not packages
        or any(
            not isinstance(name, str) or not name or not _valid_package_version(version)
            for name, version in packages.items()
        )
    ):
        raise ValueError("project system packages must be sorted exact version pins")
    if tuple(packages) != EXPECTED_SYSTEM_PACKAGES:
        raise ValueError("project toolchain has an unsupported system package set")
    host_only_packages = pin["host_only_packages"]
    if (
        not isinstance(host_only_packages, dict)
        or tuple(host_only_packages) != EXPECTED_HOST_ONLY_PACKAGES
        or any(not _valid_package_version(version) for version in host_only_packages.values())
    ):
        raise ValueError("project toolchain has an unsupported host-only package set")
    if set(host_only_packages).intersection(packages):
        raise ValueError("host-only packages must not enter the release build image")


def _valid_tool_archive(tool: dict[str, Any]) -> bool:
    valid_digest = isinstance(tool["archive_sha256"], str) and bool(
        SHA256_RE.fullmatch(tool["archive_sha256"])
    )
    url = tool["archive_url"]
    return (
        valid_digest
        and isinstance(url, str)
        and url.startswith("https://github.com/")
        and f"/v{tool['version']}/" in url
    )


def _validate_zk_tools(zk_tools: Any) -> None:
    if not isinstance(zk_tools, dict) or tuple(zk_tools) != EXPECTED_ZK_TOOLS:
        raise ValueError("project toolchain has an unsupported ZK tool set")
    for name, tool in zk_tools.items():
        if (
            not isinstance(tool, dict)
            or set(tool) != {"archive_sha256", "archive_url", "version"}
            or not _valid_package_version(tool["version"])
            or not _valid_tool_archive(tool)
        ):
            raise ValueError(f"project {name} tool must use an exact GitHub archive pin")


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
        candidate = root / source["path"].lstrip("/")
        if not candidate.is_file() or candidate.is_symlink():
            raise ValueError(f"missing or non-regular APT provenance input: {source['path']}")
        if _sha256_file(candidate) != source["sha256"]:
            raise ValueError(f"APT provenance digest mismatch: {source['path']}")

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


def repository_differences(repo_root: Path) -> list[str]:
    pin = load_and_validate_pin(repo_root / CONTRACT_PATH)
    path = Path(__file__).with_name("project_toolchain_repository.py")
    spec = importlib.util.spec_from_file_location("project_toolchain_repository", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.RepositoryChecks(repo_root, pin, globals()).run()


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
