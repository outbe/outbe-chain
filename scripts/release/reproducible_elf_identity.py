from __future__ import annotations

import subprocess
from pathlib import Path
from typing import Any


def verify_version_evidence(
    output: Path, manifest: dict | None, label: str, differences: list[str], verifier: Any
) -> bytes:
    path = output / "metadata/outbe-chain.version.txt"
    if not path.is_file() or path.is_symlink():
        differences.append(f"{label}: missing regular outbe-chain version evidence")
        return b""
    raw = path.read_bytes()
    if manifest is not None:
        try:
            version_differences = verifier.verify_version_text(
                raw.decode("utf-8"),
                source_commit=manifest["release"]["source"]["commit"],
                source_date_epoch=manifest["build"]["source_date_epoch"],
                target=manifest["build"]["target"],
                profile=manifest["build"]["profile"],
            )
        except UnicodeDecodeError:
            differences.append(f"{label}: version evidence is not UTF-8")
        else:
            differences.extend(
                f"{label}: version identity mismatch: {difference}"
                for difference in version_differences
            )
    return raw


def verify_git_identity(repo_root: Path, identity: dict[str, Any], differences: list[str]) -> None:
    try:
        head = subprocess.run(
            ["git", "-C", str(repo_root), "rev-parse", "--verify", "HEAD^{commit}"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        status = subprocess.run(
            ["git", "-C", str(repo_root), "status", "--porcelain=v1", "--untracked-files=all"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    except subprocess.CalledProcessError as error:
        differences.append(f"source identity check failed: {error}")
    else:
        if head != identity["source_commit"]:
            differences.append(
                f"source checkout HEAD {head} does not match manifest {identity['source_commit']}"
            )
        if status:
            differences.append("source checkout is dirty during independent verification")


def build_identity(
    manifest: dict | None, repo_root: Path, check_git_identity: bool, differences: list[str]
) -> dict[str, Any]:
    if manifest is None:
        return {}
    identity = {
        "release_tag": manifest["release"]["tag"],
        "source_commit": manifest["release"]["source"]["commit"],
        "source_date_epoch": manifest["build"]["source_date_epoch"],
        "target": manifest["build"]["target"],
        "profile": manifest["build"]["profile"],
        "builder_base_images": manifest["build"]["builder"]["base_images"],
        "builder_recipe": manifest["build"]["builder"]["recipe"],
    }
    if check_git_identity:
        verify_git_identity(repo_root, identity, differences)
    return identity
