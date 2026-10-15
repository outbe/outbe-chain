"""Cross-language OGD1 bytes, actual seeding and opt-in DKG window checks."""

import copy
import hashlib
import io
import json
import pathlib
import unittest
from contextlib import redirect_stdout

from genesis_seed_alloc import seed_governments_descriptor, seed_radicle_registry
from genesis_seed_codec import (
    RADICLE_REGISTRY_ADDRESS,
    StorageBuilder,
    data_slot,
    hex32,
)
from seed_genesis import apply_seed


def genesis():
    return {"config": {"chainId": 2026}, "timestamp": "0x6553f100", "alloc": {}}


def configuration():
    return {
        "max_repositories": 8,
        "governments": {
            "description": "Network governance",
            "initial_tree_oid": "0x4b825dc642cb6eb9a060e54bf8d69288fbee4904",
            "initial_content_manifest_sha256": "0x4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945",
        },
    }


class GovernmentsGenesisTests(unittest.TestCase):
    def test_seeded_words_match_p0_descriptor_and_rust_storage_fixture(self):
        base = genesis()
        storage = StorageBuilder()
        seed_radicle_registry(storage, configuration())
        seed_governments_descriptor(base, storage, configuration())
        descriptor = bytes.fromhex(base["config"]["radicleGovernments"][2:])
        self.assertEqual(
            hashlib.sha256(descriptor).hexdigest(),
            "0349a0178f4f477562b46c3c9690408ff65cdeb41d059898d38b85791fa92bcf",
        )
        self.assertEqual(
            storage.entries[hex32(10)], "0x" + hashlib.sha256(descriptor).hexdigest()
        )
        self.assertEqual(int(storage.entries[hex32(11)], 16), len(descriptor) * 2 + 1)
        stored = b"".join(
            bytes.fromhex(storage.entries[hex32(data_slot(11) + i)][2:])
            for i in range((len(descriptor) + 31) // 32)
        )
        self.assertEqual(stored[: len(descriptor)], descriptor)
        fixture = (
            pathlib.Path(__file__).parents[1]
            / "crates/system/radicleregistry/tests/fixtures/governments-genesis-storage.json"
        )
        self.assertEqual(
            json.loads(fixture.read_text()),
            {
                "config": base["config"],
                "timestamp": base["timestamp"],
                "storage": storage.entries,
            },
        )

    def test_real_seed_pipeline_is_opt_in_and_commits_descriptor(self):
        with redirect_stdout(io.StringIO()):
            enabled = apply_seed(genesis(), {"radicle_registry": configuration()})
            disabled = apply_seed(
                genesis(), {"radicle_registry": {"max_repositories": 8}}
            )
        account = enabled["alloc"][RADICLE_REGISTRY_ADDRESS]
        self.assertEqual(account["code"], "0xef")
        self.assertIn(hex32(10), account["storage"])
        self.assertIn("radicleGovernments", enabled["config"])
        self.assertNotIn("radicleGovernments", disabled["config"])
        self.assertEqual(
            disabled["alloc"][RADICLE_REGISTRY_ADDRESS]["storage"], {hex32(5): hex32(8)}
        )

    def test_dkg_window_uses_effective_prepare_and_preserves_legacy(self):
        for epoch, prepare in [(300, 30), (65, 1), (64, 0)]:
            base = genesis()
            base["config"].update(
                epochLengthBlocks=epoch, dkgPrepareWindowBlocks=prepare
            )
            seed_governments_descriptor(base, StorageBuilder(), configuration())
        for epoch, prepare in [
            (63, 0),
            (64, 1),
            (65, 2),
            (300, 300),
            (300, 999),
            (300, -1),
        ]:
            base = genesis()
            base["config"].update(
                epochLengthBlocks=epoch, dkgPrepareWindowBlocks=prepare
            )
            with (
                self.subTest(epoch=epoch, prepare=prepare),
                self.assertRaises(ValueError),
            ):
                seed_governments_descriptor(base, StorageBuilder(), configuration())
            seed_governments_descriptor(base, StorageBuilder(), {"max_repositories": 8})

    def test_invalid_descriptor_rejected_before_any_storage_write(self):
        for key, value in [
            ("description", "я" * 513),
            ("description", "x\r"),
            ("description", "x\0"),
            ("initial_tree_oid", "0x" + "00" * 20),
            ("initial_content_manifest_sha256", "0x" + "ff" * 31),
            ("delegates", []),
        ]:
            config = copy.deepcopy(configuration())
            config["governments"][key] = value
            storage = StorageBuilder()
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                seed_governments_descriptor(genesis(), storage, config)
            self.assertEqual(storage.entries, {})
        base = genesis()
        base["config"]["radicleGovernments"] = "0x00"
        with self.assertRaisesRegex(ValueError, "conflicting"):
            seed_governments_descriptor(base, StorageBuilder(), configuration())


if __name__ == "__main__":
    unittest.main()
