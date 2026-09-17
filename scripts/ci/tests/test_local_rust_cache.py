"""Exercise cache reuse across checkout cleanup, isolation and disk retention."""

from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[1] / "local_rust_cache.py"
SPEC = importlib.util.spec_from_file_location("local_rust_cache", SCRIPT)
cache = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(cache)


class LocalRustCacheTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.work = Path(self.temp.name).resolve() / "runner-work"
        self.workspace = self.work / "outbe-chain" / "outbe-chain"
        self.workspace.mkdir(parents=True)
        (self.work / "_temp").mkdir()
        for name in cache.BUILD_INPUTS:
            path = self.workspace / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(f"fixture: {name}\n")
        self.env_file = self.work / "_temp" / "github-env"
        self.env_file.touch()
        env = mock.patch.dict(os.environ, {
            "RUNNER_ENVIRONMENT": "self-hosted",
            "RUNNER_TEMP": str(self.work / "_temp"),
            "GITHUB_WORKSPACE": str(self.workspace),
            "GITHUB_REPOSITORY_ID": "1234",
            "GITHUB_WORKFLOW": "CI",
            "GITHUB_JOB": "clippy",
            "GITHUB_REF": "refs/pull/423/merge",
            "GITHUB_ENV": str(self.env_file),
            "OUTBE_RUST_CACHE_MAX_GIB": "100",
        })
        env.start()
        self.addCleanup(env.stop)
        self.rustc = "rustc 1.96.0\nhost: x86_64-unknown-linux-gnu\ncommit-hash: fixture\n"
        real_output = subprocess.check_output

        def output(command, **kwargs):
            if command[0] == "rustc":
                return self.rustc
            return real_output(command, **kwargs)

        compiler = mock.patch.object(cache.subprocess, "check_output", side_effect=output)
        compiler.start()
        self.addCleanup(compiler.stop)

    def exports(self):
        return dict(line.split("=", 1) for line in self.env_file.read_text().splitlines())

    def make_entry(self, name, age_seconds=0):
        root = cache.cache_root()
        root.mkdir(parents=True, exist_ok=True)
        entry = root / name
        entry.mkdir()
        marker = entry / ".last-used"
        marker.touch()
        used = time.time() - age_seconds
        os.utime(marker, (used, used))
        return entry

    def test_git_clean_removes_link_but_preserves_build_and_downloads(self):
        subprocess.run(["git", "init", "--quiet", str(self.workspace)], check=True)
        subprocess.run(["git", "-C", str(self.workspace), "add", "."], check=True)
        cache.setup()
        exports = self.exports()
        cargo = Path(exports["CARGO_HOME"])
        registry = cargo / "registry"
        registry.mkdir()
        (registry / "download.crate").write_text("download")
        artifact = self.workspace / "target" / "debug" / "deps" / "fixture.rlib"
        artifact.parent.mkdir(parents=True)
        artifact.write_text("compiled dependency")
        # Include coverage and nested trybuild paths; both must survive too.
        for name in ("llvm-cov-target", "tests/trybuild"):
            directory = self.workspace / "target" / name
            directory.mkdir(parents=True)
            (directory / "fixture").write_text("artifact")
        destination = (self.workspace / "target").resolve()
        subprocess.run(
            ["git", "-C", str(self.workspace), "clean", "-ffdx"], check=True
        )
        self.assertFalse((self.workspace / "target").is_symlink())
        self.assertTrue((destination / "debug/deps/fixture.rlib").is_file())
        cache.setup()
        self.assertEqual(self.exports(), exports)
        self.assertEqual(artifact.read_text(), "compiled dependency")
        self.assertEqual((registry / "download.crate").read_text(), "download")
        self.assertTrue((self.workspace / "target/llvm-cov-target/fixture").is_file())
        self.assertTrue((self.workspace / "target/tests/trybuild/fixture").is_file())
        self.assertEqual(exports["CARGO_INCREMENTAL"], "0")
        self.assertEqual(exports["CARGO_TARGET_DIR"], str(self.workspace / "target"))
        self.assertNotIn(self.workspace, cargo.parents)

    def test_pr_main_workflow_and_job_have_separate_caches(self):
        original = cache.cache_key(self.workspace)
        for variable, value in (
            ("GITHUB_REF", "refs/heads/main"),
            ("GITHUB_REF", "refs/pull/424/merge"),
            ("GITHUB_JOB", "test"),
            ("GITHUB_WORKFLOW", "prerelease"),
        ):
            with self.subTest(variable=variable, value=value):
                with mock.patch.dict(os.environ, {variable: value}):
                    self.assertNotEqual(cache.cache_key(self.workspace), original)

    def test_runner_installations_and_repositories_have_separate_roots(self):
        original = cache.cache_root()
        with mock.patch.dict(os.environ, {"RUNNER_TEMP": str(self.work / "other/_temp")}):
            self.assertNotEqual(cache.cache_root(), original)
        with mock.patch.dict(os.environ, {"GITHUB_REPOSITORY_ID": "5678"}):
            self.assertNotEqual(cache.cache_root(), original)

    def test_toolchain_image_and_flags_invalidate_but_lockfile_reuses_directory(self):
        original = cache.cache_key(self.workspace)
        (self.workspace / "Cargo.lock").write_text("changed dependencies")
        self.assertEqual(cache.cache_key(self.workspace), original)
        with mock.patch.dict(os.environ, {"RUSTFLAGS": "-C target-cpu=generic"}):
            self.assertNotEqual(cache.cache_key(self.workspace), original)
        self.rustc += "new compiler identity"
        self.assertNotEqual(cache.cache_key(self.workspace), original)
        before = cache.cache_key(self.workspace)
        (self.workspace / ".github/workflows/ci.yml").write_text("new image digest")
        self.assertNotEqual(cache.cache_key(self.workspace), before)

    def test_setup_does_not_replace_an_existing_target(self):
        target = self.workspace / "target"
        target.mkdir()
        artifact = target / "keep"
        artifact.write_text("existing build")
        with self.assertRaisesRegex(ValueError, "target/ absent"):
            cache.setup()
        self.assertEqual(artifact.read_text(), "existing build")
        self.assertEqual(self.env_file.read_text(), "")

    def test_expiry_keeps_active_and_unmanaged_directories(self):
        expired = self.make_entry("a" * 24, cache.MAX_AGE_SECONDS + 60)
        active = self.make_entry("b" * 24, cache.MAX_AGE_SECONDS + 60)
        recent = self.make_entry("c" * 24)
        unrelated = cache.cache_root() / "unmanaged"
        unrelated.mkdir()
        symlink = cache.cache_root() / ("d" * 24)
        symlink.symlink_to(unrelated, target_is_directory=True)
        cache.prune(cache.cache_root(), keep=active)
        self.assertFalse(expired.exists())
        self.assertTrue(active.exists())
        self.assertTrue(recent.exists())
        self.assertTrue(unrelated.exists())
        self.assertTrue(symlink.is_symlink())

    def test_budget_evicts_oldest_then_oversized_active_entry_only_at_finish(self):
        old = self.make_entry("a" * 24, 120)
        recent = self.make_entry("b" * 24, 60)
        active = self.make_entry("c" * 24)
        with mock.patch.dict(os.environ, {"OUTBE_RUST_CACHE_MAX_GIB": "1"}):
            with mock.patch.object(cache, "size_bytes", return_value=600 * 1024**2):
                cache.prune(cache.cache_root(), keep=active)
                self.assertFalse(old.exists())
                self.assertFalse(recent.exists())
                self.assertTrue(active.exists())
            with mock.patch.object(cache, "size_bytes", return_value=2 * 1024**3):
                cache.prune(cache.cache_root(), keep=active)
                self.assertTrue(active.exists())
                with mock.patch.dict(os.environ, {"OUTBE_RUST_CACHE_DIR": str(active)}):
                    cache.finish()
                self.assertFalse(active.exists())

    def test_finish_rejects_unmanaged_directory(self):
        with mock.patch.dict(os.environ, {"OUTBE_RUST_CACHE_DIR": str(self.workspace)}):
            with self.assertRaisesRegex(ValueError, "outside the managed directory"):
                cache.finish()
        self.assertTrue(self.workspace.is_dir())

    def test_non_self_hosted_runner_is_rejected(self):
        with mock.patch.dict(os.environ, {"RUNNER_ENVIRONMENT": "github-hosted"}):
            with self.assertRaisesRegex(ValueError, "self-hosted"):
                cache.setup()


if __name__ == "__main__":
    unittest.main()
