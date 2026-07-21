#!/usr/bin/env python3
"""Tests for host-side build-spec validation and immutable release identity."""

from __future__ import annotations

import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]
MODULE_PATH = REPO_ROOT / "scripts/release/reproducible_build_inputs.py"
BUILD_SPEC_PATH = REPO_ROOT / "release/reproducible-elf-build-v1.json"


def load_module():
    spec = importlib.util.spec_from_file_location("reproducible_build_inputs", MODULE_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {MODULE_PATH}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


build_inputs = load_module()


class ReproducibleBuildInputsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        self.repo = Path(self.tempdir.name)
        subprocess.run(["git", "init", "-q", str(self.repo)], check=True)
        subprocess.run(
            ["git", "-C", str(self.repo), "config", "user.email", "release@example.test"],
            check=True,
        )
        subprocess.run(
            ["git", "-C", str(self.repo), "config", "user.name", "Release Test"],
            check=True,
        )
        (self.repo / "source").write_text("first\n", encoding="utf-8")
        subprocess.run(["git", "-C", str(self.repo), "add", "source"], check=True)
        subprocess.run(
            ["git", "-C", str(self.repo), "commit", "-q", "-m", "first"], check=True
        )
        self.first = subprocess.run(
            ["git", "-C", str(self.repo), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()

    def test_ambient_tags_do_not_change_commit_identity(self) -> None:
        before = build_inputs.resolve_release_identity(self.repo, None)
        subprocess.run(["git", "-C", str(self.repo), "tag", "v99.0.0"], check=True)
        after = build_inputs.resolve_release_identity(self.repo, None)
        self.assertEqual(before, after)
        self.assertEqual(after["release_tag"], f"commit-{self.first}")
        self.assertEqual(after["source_describe"], after["release_tag"])

    def test_explicit_tag_must_resolve_to_head(self) -> None:
        subprocess.run(["git", "-C", str(self.repo), "tag", "v1.0.0"], check=True)
        identity = build_inputs.resolve_release_identity(self.repo, "v1.0.0")
        self.assertEqual(identity["release_tag"], "v1.0.0")
        self.assertEqual(identity["source_describe"], "v1.0.0")

        (self.repo / "source").write_text("second\n", encoding="utf-8")
        subprocess.run(["git", "-C", str(self.repo), "commit", "-qam", "second"], check=True)
        with self.assertRaisesRegex(ValueError, "not HEAD"):
            build_inputs.resolve_release_identity(self.repo, "v1.0.0")

    def test_mutable_builder_is_rejected_by_host_loader(self) -> None:
        spec = json.loads(BUILD_SPEC_PATH.read_text(encoding="utf-8"))
        spec["builder"]["image"] = "rust:latest"
        path = self.repo / "invalid-spec.json"
        path.write_text(json.dumps(spec), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "pinned by sha256"):
            build_inputs.load_and_validate_build_spec(path)


if __name__ == "__main__":
    unittest.main()
