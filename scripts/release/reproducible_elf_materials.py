from __future__ import annotations

import hashlib
from pathlib import Path
from typing import Any


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _safe_material_path(root: Path, relative: str) -> Path | None:
    candidate = Path(relative)
    if candidate.is_absolute():
        return None
    resolved = (root / candidate).resolve()
    try:
        resolved.relative_to(root.resolve())
    except ValueError:
        return None
    return resolved


def _verify_material(
    root: Path, record: dict[str, Any], label: str, differences: list[str]
) -> None:
    relative = record.get("path", "")
    path = _safe_material_path(root, relative)
    if path is None or not path.is_file() or path.is_symlink():
        differences.append(f"{label}: missing or unsafe material path: {relative}")
        return
    raw = path.read_bytes()
    if record.get("size") != len(raw):
        differences.append(f"{label}: material size mismatch: {relative}")
    if record.get("digest", {}).get("value") != _sha256(raw):
        differences.append(f"{label}: material digest mismatch: {relative}")


def _verify_checksum_file(
    output: Path, label: str, differences: list[str], expected_paths: frozenset[str]
) -> bytes:
    checksum_path = output / "SHA256SUMS"
    if not checksum_path.is_file() or checksum_path.is_symlink():
        differences.append(f"{label}: missing regular SHA256SUMS")
        return b""
    raw = checksum_path.read_bytes()
    declared: dict[str, str] = {}
    try:
        for line in raw.decode("ascii").splitlines():
            digest, relative = line.split("  ", 1)
            if len(digest) != 64 or any(
                character not in "0123456789abcdef" for character in digest
            ):
                raise ValueError(f"invalid digest for {relative}")
            if relative in declared:
                raise ValueError(f"duplicate path: {relative}")
            declared[relative] = digest
    except (UnicodeDecodeError, ValueError) as error:
        differences.append(f"{label}: invalid SHA256SUMS: {error}")
        return raw

    if frozenset(declared) != expected_paths:
        differences.append(f"{label}: SHA256SUMS does not declare the exact output matrix")
    for relative, expected in declared.items():
        path = _safe_material_path(output, relative)
        if path is None or not path.is_file() or path.is_symlink():
            differences.append(f"{label}: checksum path missing or unsafe: {relative}")
            continue
        if _sha256(path.read_bytes()) != expected:
            differences.append(f"{label}: checksum mismatch: {relative}")
    return raw


def verify_manifest_materials(
    output: Path, manifest: dict | None, repo_root: Path, label: str, differences: list[str]
) -> None:
    if manifest is None:
        return
    for record in manifest.get("inputs", []):
        _verify_material(repo_root, record, f"{label} source input", differences)
    packages = manifest.get("build", {}).get("builder", {}).get("resolved_system_packages")
    if packages is None:
        differences.append(f"{label}: missing resolved system package material")
    else:
        _verify_material(output, packages, f"{label} package inventory", differences)
