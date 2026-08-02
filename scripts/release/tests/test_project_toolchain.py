#!/usr/bin/env python3
"""Repository contract tests for the single Outbe project toolchain image."""

from __future__ import annotations

import importlib.util
import json
import re
import shutil
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]
TEE_BUILD_SCRIPT = REPO_ROOT / "crates/system/tee/build.rs"
DOCKERFILE = REPO_ROOT / "Dockerfile.project-toolchain"
PIN_PATH = REPO_ROOT / "release/project-toolchain-v1.json"
VERIFIER_PATH = REPO_ROOT / "scripts/release/verify_project_toolchain.py"


def load_verifier():
    spec = importlib.util.spec_from_file_location("verify_project_toolchain", VERIFIER_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {VERIFIER_PATH}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ProjectToolchainContractTests(unittest.TestCase):
    def test_project_declares_one_version_pin_without_registry_delivery(self) -> None:
        pin = json.loads(PIN_PATH.read_text(encoding="utf-8"))

        self.assertEqual(pin["schema_version"], 1)
        self.assertEqual(pin["activation"], "pending-container-delivery")
        self.assertEqual(pin["target"], "x86_64-unknown-linux-gnu")
        self.assertEqual(pin["platform"], "linux/amd64")
        self.assertEqual(pin["rust"]["version"], "1.96.0")
        self.assertEqual(pin["gramine"]["version"], "1.9")
        self.assertEqual(
            pin["system_packages"]["libsgx-dcap-quote-verify"],
            "1.26.100.1-noble1",
        )
        self.assertEqual(pin["system_packages"]["libsgx-headers"], "2.29.100.1-noble1")
        self.assertNotIn("image", pin)
        self.assertNotIn("registry", pin)
        self.assertNotIn("digest", pin)

    def test_repository_consumers_follow_the_project_version_pin(self) -> None:
        load_verifier().verify_repository(REPO_ROOT)

    def test_consumer_version_drift_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for relative in (
                "Dockerfile.project-toolchain",
                "crates/system/tee/build.rs",
                "rust-toolchain.toml",
                "release/dcap-native-qvl-v1.json",
                "release/project-toolchain-v1.json",
                "release/reproducible-elf-build-v1.json",
                "release/testnet-sgx-bundle-v1.json",
            ):
                destination = root / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(REPO_ROOT / relative, destination)

            sgx_path = root / "release/testnet-sgx-bundle-v1.json"
            sgx = json.loads(sgx_path.read_text(encoding="utf-8"))
            sgx["gramine"]["version"] = "1.8"
            sgx_path.write_text(json.dumps(sgx), encoding="utf-8")

            with self.assertRaisesRegex(ValueError, "SGX Gramine version"):
                load_verifier().verify_repository(root)

    def test_incomplete_project_package_pin_fails_closed(self) -> None:
        pin = json.loads(PIN_PATH.read_text(encoding="utf-8"))
        del pin["system_packages"]["libsgx-headers"]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "project-toolchain-v1.json"
            path.write_text(json.dumps(pin), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "system package set"):
                load_verifier().load_and_validate_pin(path)

    def test_native_dcap_build_contract_reads_the_release_pins(self) -> None:
        pin = json.loads(PIN_PATH.read_text(encoding="utf-8"))
        source = TEE_BUILD_SCRIPT.read_text(encoding="utf-8")

        self.assertIn(
            'include_str!("../../../release/project-toolchain-v1.json")',
            source,
        )
        self.assertIn(
            'include_str!("../../../release/dcap-native-qvl-v1.json")',
            source,
        )
        for version in pin["system_packages"].values():
            self.assertNotIn(f'"{version}"', source)

    def test_toolchain_recipe_pins_every_critical_component(self) -> None:
        pin = json.loads(PIN_PATH.read_text(encoding="utf-8"))
        recipe = DOCKERFILE.read_text(encoding="utf-8")

        self.assertIn(f"rust:{pin['rust']['version']}-", recipe)
        self.assertIn(f"gramineproject/gramine:{pin['gramine']['version']}-", recipe)
        for package, version in pin["system_packages"].items():
            self.assertIn(f"{package}={version}", recipe)

        self.assertIn("/opt/outbe/toolchain/verify_dcap_native_qvl.py", recipe)
        self.assertIn("/opt/outbe/toolchain/project-toolchain-v1.json", recipe)
        self.assertIn("/opt/outbe/toolchain/verify_project_toolchain.py", recipe)
        self.assertIn("--root /", recipe)
        self.assertIn("ENTRYPOINT []", recipe)
        self.assertIn("rustup component add rustfmt clippy rust-src", recipe)
        self.assertIn(
            f"--toolchain {pin['rust']['version']}-x86_64-unknown-linux-gnu",
            recipe,
        )
        self.assertIn(
            f"""test "$(rustc --version | cut -d' ' -f1-2)" = "rustc {pin['rust']['version']}" """,
            recipe,
        )
        self.assertIn(
            f"""test "$(dpkg-query -W -f='${{Version}}' gramine)" = "{pin['gramine']['version']}" """,
            recipe,
        )

    def test_toolchain_recipe_has_no_mutable_or_runtime_dcap_path(self) -> None:
        recipe = DOCKERFILE.read_text(encoding="utf-8").lower()

        self.assertRegex(
            recipe.splitlines()[0],
            r"^# syntax=docker/dockerfile:1\.7@sha256:[0-9a-f]{64}$",
        )
        self.assertNotIn("apt-get upgrade", recipe)
        self.assertNotIn("apt upgrade", recipe)
        self.assertNotIn("qpl", recipe)
        self.assertNotIn("pccs", recipe)
        self.assertNotRegex(recipe, r"from\s+\S+:(latest|main|master)(?:\s|$)")
        self.assertEqual(len(re.findall(r"^from\s+", recipe, re.MULTILINE)), 2)
        for image in re.findall(r"^from\s+(\S+)", recipe, re.MULTILINE):
            self.assertRegex(image, r"@sha256:[0-9a-f]{64}$")


if __name__ == "__main__":
    unittest.main()
