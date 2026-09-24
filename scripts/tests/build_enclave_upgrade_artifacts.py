#!/usr/bin/env python3
"""Build real versioned releases for the hardware enclave-upgrade E2E."""

import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def build(repo, jobs):
    def git(*args):
        return subprocess.check_output(["git", *args], cwd=repo, text=True)

    if git("status", "--porcelain").strip():
        raise RuntimeError("commit the source checkout before building upgrade releases")
    commit = git("rev-parse", "HEAD").strip()
    output = repo / "target/e2e-upgrades"
    output.mkdir(parents=True, exist_ok=True)
    # Never overwrite the base release executables used by the first network.
    target = output / "build-target"
    # Resolve the checkout's pinned toolchain before entering an archived tree.
    # PATH may contain mise shims that reject newly created temporary directories.
    toolchain = {tool: subprocess.check_output(
        ["rustup", "which", tool], cwd=repo, text=True,
    ).strip() for tool in ("cargo", "rustc", "rustdoc")}
    env = dict(os.environ, CARGO_BUILD_JOBS=str(jobs), RAYON_NUM_THREADS=str(jobs),
               CARGO_TARGET_DIR=str(target), RUSTC=toolchain["rustc"],
               RUSTDOC=toolchain["rustdoc"])
    with tempfile.TemporaryDirectory(prefix="outbe-upgrade-source-") as temporary:
        directory = Path(temporary)
        archive = directory / "source.tar"
        subprocess.run(["git", "archive", "--format=tar", "--output", str(archive),
                        commit], cwd=repo, check=True)
        source = directory / "source"
        source.mkdir()
        subprocess.run(["tar", "-xf", str(archive), "-C", str(source)], check=True)
        original = {name: (source / name).read_text() for name in ("Cargo.toml", "Cargo.lock")}
        for version in ("0.2", "0.3"):
            manifest, count = re.subn(
                r'(\[workspace\.package\]\n[^\[]*?version = ")[^"]+("\n)',
                lambda match: match[1] + version + ".0" + match[2],
                original["Cargo.toml"], count=1,
            )
            if count != 1:
                raise RuntimeError("workspace package version must occur exactly once")
            (source / "Cargo.toml").write_text(manifest)
            (source / "Cargo.lock").write_text(original["Cargo.lock"])
            packages = [("outbe-tee-enclave", "enclave-" + version, [])]
            if version == "0.3":
                packages.append(("outbe-chain", "node-0.3",
                                 ["--features", "e2e-test,test-protocol-overrides"]))
            for package, name, features in packages:
                # The intentional workspace version change updates Cargo.lock;
                # preserve its exact diff in the artifact's build record.
                command = [toolchain["cargo"], "build", "--release", "-j", str(jobs),
                           "-p", package, "--bin", package, *features]
                subprocess.run(command, cwd=source, env=env, check=True)
                destination = output / name
                destination.mkdir(exist_ok=True)
                binary = destination / package
                staged = destination / (package + ".next")
                shutil.copy2(target / "release" / package, staged)
                os.replace(staged, binary)
                source_diff = "".join(
                    "".join(difflib.unified_diff(
                        original[path].splitlines(keepends=True),
                        (source / path).read_text().splitlines(keepends=True),
                        fromfile="a/" + path, tofile="b/" + path,
                    )) for path in original
                )
                metadata = dict(source_commit=commit, workspace_version=version + ".0",
                                command=command, binary_sha256=sha256(binary),
                                cargo_lock_sha256=sha256(source / "Cargo.lock"),
                                source_diff=source_diff)
                staged_record = destination / "build.json.next"
                staged_record.write_text(json.dumps(metadata, indent=2) + "\n")
                os.replace(staged_record, destination / "build.json")
                print("BUILT", binary, metadata["binary_sha256"], flush=True)
    if git("rev-parse", "HEAD").strip() != commit or git("status", "--porcelain").strip():
        raise RuntimeError("source checkout changed during the upgrade builds")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--jobs", type=int, choices=range(1, 5), default=4)
    args = parser.parse_args()
    build(args.repo.resolve(), args.jobs)
