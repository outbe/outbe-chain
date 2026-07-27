import importlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS_DIR = Path(__file__).resolve().parents[1]
REPO_ROOT = SCRIPTS_DIR.parent

namespace = importlib.import_module("scripts.check_stablecoin_namespace")
seeder = importlib.import_module("scripts.seed_genesis")

scan_genesis = namespace.scan_genesis
scan_rust_addresses = namespace.scan_rust_addresses
MARKER_CODE = seeder.MARKER_CODE
STABLECOIN_ADDRESS_PREFIX = seeder.STABLECOIN_ADDRESS_PREFIX
STABLECOIN_FACTORY_ADDRESS = seeder.STABLECOIN_FACTORY_ADDRESS
STABLECOIN_POLICY_REGISTRY_ADDRESS = seeder.STABLECOIN_POLICY_REGISTRY_ADDRESS
validate_stablecoin_namespace_alloc = seeder.validate_stablecoin_namespace_alloc


class StablecoinNamespaceTests(unittest.TestCase):
    def test_declared_rust_addresses_are_collision_free(self):
        declared, builtins = scan_rust_addresses(
            REPO_ROOT / "crates/blockchain/primitives/src/addresses.rs"
        )
        self.assertGreater(declared, 30)
        self.assertEqual(builtins, 10)

    def test_clean_reserved_markers_pass(self):
        alloc = {
            STABLECOIN_FACTORY_ADDRESS: {"code": MARKER_CODE, "balance": "0x0"},
            STABLECOIN_POLICY_REGISTRY_ADDRESS: {
                "code": MARKER_CODE,
                "balance": "0x0",
            },
        }
        validate_stablecoin_namespace_alloc(alloc, require_reserved_markers=True)

    def test_dynamic_class_rejects_balance_only_allocation(self):
        address = STABLECOIN_ADDRESS_PREFIX + "00" * 18
        with self.assertRaisesRegex(ValueError, "reserved stablecoin prefix"):
            validate_stablecoin_namespace_alloc(
                {address: {"balance": "0x1"}}, require_reserved_markers=False
            )

    def test_fixed_accounts_reject_conflicting_state(self):
        conflicts = [
            {"code": "0x00"},
            {"code": MARKER_CODE, "balance": "0x1"},
            {"code": MARKER_CODE, "nonce": "0x1"},
            {"code": MARKER_CODE, "storage": {"0x00": "0x01"}},
        ]
        for account in conflicts:
            with self.subTest(account=account):
                with self.assertRaises(ValueError):
                    validate_stablecoin_namespace_alloc(
                        {STABLECOIN_FACTORY_ADDRESS: account},
                        require_reserved_markers=False,
                    )

    def test_duplicate_normalized_alloc_key_is_rejected(self):
        address = "1111111111111111111111111111111111111111"
        with self.assertRaisesRegex(ValueError, "duplicate genesis alloc address"):
            validate_stablecoin_namespace_alloc(
                {address: {}, "0x" + address.upper(): {}},
                require_reserved_markers=False,
            )

    def test_complete_seeder_output_contains_reserved_markers_and_rescans(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            genesis_path = root / "genesis.json"
            seed_path = root / "seed.json"
            output_path = root / "output.json"
            genesis_path.write_text(
                json.dumps({"config": {}, "alloc": {}, "timestamp": "0x0"}),
                encoding="utf-8",
            )
            seed_path.write_text("{}", encoding="utf-8")
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPTS_DIR / "seed_genesis.py"),
                    "--genesis",
                    str(genesis_path),
                    "--seed",
                    str(seed_path),
                    "--output",
                    str(output_path),
                ],
                cwd=REPO_ROOT,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            generated = json.loads(output_path.read_text(encoding="utf-8"))
            for address in (
                STABLECOIN_FACTORY_ADDRESS,
                STABLECOIN_POLICY_REGISTRY_ADDRESS,
            ):
                self.assertEqual(
                    generated["alloc"][address],
                    {"code": MARKER_CODE, "balance": "0x0"},
                )
            self.assertGreater(
                scan_genesis(output_path, require_reserved_markers=True), 2
            )

    def test_genesis_scanner_uses_seeder_validator(self):
        genesis = {
            "alloc": {
                STABLECOIN_FACTORY_ADDRESS: {
                    "code": MARKER_CODE,
                    "balance": "0x0",
                },
                STABLECOIN_POLICY_REGISTRY_ADDRESS: {
                    "code": MARKER_CODE,
                    "balance": "0x0",
                },
            }
        }
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "genesis.json"
            path.write_text(json.dumps(genesis), encoding="utf-8")
            self.assertEqual(scan_genesis(path, require_reserved_markers=True), 2)


if __name__ == "__main__":
    unittest.main()
