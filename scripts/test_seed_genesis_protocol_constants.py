import importlib.util
import pathlib
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("seed_genesis.py")
SPEC = importlib.util.spec_from_file_location("seed_genesis", MODULE_PATH)
seed_genesis = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(seed_genesis)


class ProtocolConstantsSeedTests(unittest.TestCase):
    def test_optional_seed_profile_is_copied_to_genesis_config(self):
        genesis = {"config": {"chainId": 1}, "alloc": {}}
        profile = {
            "schemaVersion": 1,
            "metadosis": {"formingPeriodSeconds": 60},
            "ocomp": {"computeVoteWindowBlocks": 120},
        }

        seed_genesis.seed_protocol_constants(genesis, {"protocol_constants": profile})

        self.assertEqual(genesis["config"]["outbeProtocol"], profile)

    def test_absent_profile_preserves_existing_genesis_config(self):
        existing = {"metadosis": {"formingPeriodSeconds": 60}}
        genesis = {"config": {"outbeProtocol": existing}, "alloc": {}}

        seed_genesis.seed_protocol_constants(genesis, {})

        self.assertEqual(genesis["config"]["outbeProtocol"], existing)

    def test_conflicting_profile_and_non_object_values_are_rejected(self):
        genesis = {
            "config": {"outbeProtocol": {"metadosis": {"formingPeriodSeconds": 60}}},
            "alloc": {},
        }
        with self.assertRaisesRegex(ValueError, "conflicts"):
            seed_genesis.seed_protocol_constants(
                genesis,
                {"protocol_constants": {"metadosis": {"formingPeriodSeconds": 61}}},
            )
        with self.assertRaisesRegex(ValueError, "JSON object"):
            seed_genesis.seed_protocol_constants(
                {"config": {}, "alloc": {}}, {"protocol_constants": []}
            )

    def test_fresh_metadosis_removes_only_preseeded_worldwide_days(self):
        seed = {
            "metadosis": {"worldwide_days": [{"wwd": 20260807}], "other": 7},
            "oracle": {"scurve_seeds": [{"peak_day": 20260807}]},
        }

        seed_genesis.clear_seeded_metadosis_days(seed)

        self.assertEqual(seed["metadosis"]["worldwide_days"], [])
        self.assertEqual(seed["metadosis"]["other"], 7)
        self.assertEqual(seed["oracle"]["scurve_seeds"], [{"peak_day": 20260807}])

    def test_nod_materialization_fifo_is_initialized_without_seeded_nods(self):
        storage = seed_genesis.StorageBuilder()

        seed_genesis.seed_nod_materialization_fifo(storage)

        self.assertEqual(storage.entries[seed_genesis.hex32(19)], seed_genesis.hex32(1))
        self.assertEqual(storage.entries[seed_genesis.hex32(20)], seed_genesis.hex32(1))


if __name__ == "__main__":
    unittest.main()
