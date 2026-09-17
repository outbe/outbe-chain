#!/usr/bin/env python3
"""Keep Cargo data on the self-hosted runner's persistent work-directory mount."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time


MAX_AGE_SECONDS = 7 * 24 * 60 * 60
CACHE_NAME = re.compile(r"[0-9a-f]{24}")
# Include the workflow to invalidate on container digest / build flag changes.
# Cargo.lock is deliberately omitted: Cargo handles changed dependencies while
# retaining the unchanged ones. Keep paths stable between runs of the same PR.
BUILD_INPUTS = (
    ".github/workflows/ci.yml",
    "Dockerfile.project-toolchain",
    "release/project-toolchain-v1.json",
    "rust-toolchain.toml",
    ".cargo/config.toml",
)


def cache_root() -> Path:
    if os.environ.get("RUNNER_ENVIRONMENT") != "self-hosted":
        raise ValueError("local Rust caching requires a self-hosted runner")
    repository_id = os.environ["GITHUB_REPOSITORY_ID"]
    if not repository_id.isdecimal():
        raise ValueError("GITHUB_REPOSITORY_ID must be numeric")
    # In container jobs RUNNER_TEMP is /__w/_temp. Its parent is the persistent
    # _work bind mount, unique to each runner installation. Do not put the cache
    # in _temp (runner cleanup) or the checkout (git clean -ffdx).
    work = Path(os.environ["RUNNER_TEMP"]).resolve().parent
    return work / "_outbe_rust_cache" / "v1" / repository_id


def cache_key(workspace: Path) -> str:
    identity = {
        name: os.environ[name]
        for name in ("GITHUB_WORKFLOW", "GITHUB_JOB", "GITHUB_REF")
    }
    # Separate PR merge refs from main and from other PRs; do not reuse artifacts
    # written by a PR in a subsequent privileged main/release build.
    identity["rustc"] = subprocess.check_output(
        ["rustc", "--version", "--verbose"], text=True
    )
    identity["flags"] = {
        name: value
        for name, value in os.environ.items()
        if name in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CC", "CXX", "CFLAGS", "CXXFLAGS")
        or name.startswith("CARGO_PROFILE_")
    }
    identity["inputs"] = {
        name: hashlib.sha256((workspace / name).read_bytes()).hexdigest()
        for name in BUILD_INPUTS
    }
    return hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()[:24]


def size_bytes(directory: Path) -> int:
    # du counts allocated disk blocks, and does not follow symlinks by default.
    output = subprocess.check_output(["du", "-sk", str(directory)], text=True)
    return int(output.split()[0]) * 1024


def prune(root: Path, keep: Path | None = None) -> None:
    limit_gib = int(os.environ.get("OUTBE_RUST_CACHE_MAX_GIB", "100"))
    if limit_gib <= 0:
        raise ValueError("OUTBE_RUST_CACHE_MAX_GIB must be positive")
    limit = limit_gib * 1024**3
    entries = []
    for entry in root.iterdir():
        if not CACHE_NAME.fullmatch(entry.name) or entry.is_symlink() or not entry.is_dir():
            continue
        marker = entry / ".last-used"
        last_used = marker.stat().st_mtime if marker.exists() else entry.stat().st_mtime
        if entry != keep and time.time() - last_used > MAX_AGE_SECONDS:
            print(f"Expiring local Rust cache: {entry}", flush=True)
            shutil.rmtree(entry)
        else:
            entries.append((last_used, entry, size_bytes(entry)))
    total = sum(size for _, _, size in entries)
    for _, entry, size in sorted(entries):
        if total <= limit:
            break
        if entry == keep:
            continue
        print(f"Evicting local Rust cache to enforce {limit_gib} GiB limit: {entry}", flush=True)
        shutil.rmtree(entry)
        total -= size
    print(f"Local Rust cache usage: {total / 1024**3:.2f} / {limit_gib} GiB", flush=True)


def append_variables(file_variable: str, values: dict[str, str]) -> None:
    with Path(os.environ[file_variable]).open("a", encoding="utf-8") as stream:
        for name, value in values.items():
            if "\n" in value or "\r" in value:
                raise ValueError(f"invalid newline in {name}")
            stream.write(f"{name}={value}\n")


def setup() -> None:
    workspace = Path(os.environ["GITHUB_WORKSPACE"]).resolve()
    root = cache_root()
    if workspace == root or workspace in root.parents:
        raise ValueError("the cache must be outside the checkout")
    root.mkdir(parents=True, exist_ok=True)
    cache = root / cache_key(workspace)
    if cache.is_symlink():
        raise ValueError("cache directory must not be a symlink")
    target = workspace / "target"
    if target.exists() or target.is_symlink():
        raise ValueError("setup must run immediately after checkout, with target/ absent")
    warm = cache.is_dir()
    (cache / "cargo").mkdir(parents=True, exist_ok=True)
    (cache / "target").mkdir(exist_ok=True)
    (cache / ".last-used").touch()
    prune(root, keep=cache)
    # Preserve target/debug, target/tests and target/llvm-cov-target for scripts,
    # trybuild and coverage. Checkout removes only this symlink on the next run.
    target.symlink_to(cache / "target", target_is_directory=True)
    append_variables("GITHUB_ENV", {
        "CARGO_HOME": str(cache / "cargo"),
        "CARGO_TARGET_DIR": str(target),
        "CARGO_INCREMENTAL": "0",
        "OUTBE_RUST_CACHE_DIR": str(cache),
    })
    print(f"Local Rust cache ({'warm' if warm else 'cold'}): {cache}", flush=True)


def finish() -> None:
    root = cache_root()
    cache = Path(os.environ["OUTBE_RUST_CACHE_DIR"])
    if cache.parent != root or not CACHE_NAME.fullmatch(cache.name) or cache.is_symlink():
        raise ValueError("refusing to maintain a cache outside the managed directory")
    if cache.is_dir():
        (cache / ".last-used").touch()
        print(f"Current job cache: {size_bytes(cache) / 1024**3:.2f} GiB", flush=True)
    # One job runs per runner installation; other installations have separate
    # work-directory mounts. At this point no job uses these build artifacts.
    # A single oversized cache can also be evicted instead of filling the disk.
    prune(root)


if __name__ == "__main__":
    if sys.argv[1:] == ["setup"]:
        setup()
    elif sys.argv[1:] == ["finish"]:
        finish()
    else:
        raise SystemExit("usage: local_rust_cache.py {setup|finish}")
