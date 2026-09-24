"""Build orchestration tests; these do not execute or attest an enclave."""

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "upgrade_builder", Path(__file__).with_name("build_enclave_upgrade_artifacts.py")
)
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)


class UpgradeArtifactsTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        root = Path(self.temporary.name)
        self.repo = root / "repo"
        self.repo.mkdir()
        self.git("init", "-q")
        self.git("config", "user.name", "Build Test")
        self.git("config", "user.email", "build@example.invalid")
        (self.repo / "Cargo.toml").write_text('[workspace.package]\nversion = "0.1.0"\n')
        (self.repo / "Cargo.lock").write_text('version = "0.1.0"\n')
        (self.repo / ".gitignore").write_text('/target/\n')
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")
        base = self.repo / "target/release"
        base.mkdir(parents=True)
        for name in ("outbe-chain", "outbe-tee-enclave"):
            (base / name).write_bytes(b"unchanged base release")
        tools = root / "tools"
        tools.mkdir()
        cargo = tools / "cargo"
        cargo.write_text('''#!/usr/bin/env python3
import os, pathlib, re, sys
version = re.search(r'version = "([^"]+)"', pathlib.Path('Cargo.toml').read_text())[1]
package = sys.argv[sys.argv.index('--bin') + 1]
assert os.environ['CARGO_BUILD_JOBS'] == '4'
assert os.environ['RAYON_NUM_THREADS'] == '4'
target = pathlib.Path(os.environ['CARGO_TARGET_DIR']) / 'release'
target.mkdir(parents=True, exist_ok=True)
(target / package).write_text(package + ':' + version)
(target / package).chmod(0o755)
pathlib.Path('Cargo.lock').write_text('version = "' + version + '"\\n')
''')
        cargo.chmod(0o755)
        rustup = tools / "rustup"
        rustup.write_text("#!/usr/bin/env python3\nimport pathlib, sys\n"
                          "print(pathlib.Path(__file__).parent / sys.argv[-1])\n")
        rustup.chmod(0o755)
        self.path = str(tools) + os.pathsep + os.environ["PATH"]

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.repo, text=True).strip()

    def test_versions_provenance_and_base_artifacts_stay_separate(self):
        commit = self.git("rev-parse", "HEAD")
        with patch.dict(os.environ, PATH=self.path):
            builder.build(self.repo, 4)
        for directory, package, version in (
            ("enclave-0.2", "outbe-tee-enclave", "0.2.0"),
            ("enclave-0.3", "outbe-tee-enclave", "0.3.0"),
            ("node-0.3", "outbe-chain", "0.3.0"),
        ):
            output = self.repo / "target/e2e-upgrades" / directory
            binary = output / package
            self.assertEqual(binary.read_text(), package + ":" + version)
            metadata = json.loads((output / "build.json").read_text())
            self.assertEqual(metadata["source_commit"], commit)
            self.assertEqual(metadata["workspace_version"], version)
            self.assertEqual(metadata["binary_sha256"], builder.sha256(binary))
            self.assertIn('+version = "' + version + '"', metadata["source_diff"])
        self.assertEqual(self.git("status", "--porcelain"), "")
        for name in ("outbe-chain", "outbe-tee-enclave"):
            self.assertEqual((self.repo / "target/release" / name).read_bytes(),
                             b"unchanged base release")

    def test_dirty_source_is_rejected_before_any_replacement_build(self):
        (self.repo / "Cargo.toml").write_text("uncommitted source")
        with self.assertRaisesRegex(RuntimeError, "commit the source checkout"):
            builder.build(self.repo, 4)
        self.assertFalse((self.repo / "target/e2e-upgrades").exists())


if __name__ == "__main__":
    unittest.main()
