import importlib.util
import json
import pathlib
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("seed_genesis.py")
SEED_PROFILES = {
    "seed-testnet-lowstake.json": {
        "min_stake": "1000000000",
        "validator_stake": "100000000000",
    },
    "seed-testnet.json": {
        "min_stake": "100000000000",
        "validator_stake": "100000000000",
    },
    "churn-seed.json": {
        "min_stake": "1000000000",
        "validator_stake": "1000000000",
    },
}
SPEC = importlib.util.spec_from_file_location("seed_genesis", MODULE_PATH)
seed_genesis = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(seed_genesis)


class ProtocolConstantsSeedTests(unittest.TestCase):
    def test_checked_in_seed_profiles_use_six_decimal_monetary_units(self):
        for filename, staking_expected in SEED_PROFILES.items():
            with self.subTest(filename=filename):
                seed = json.loads(MODULE_PATH.with_name(filename).read_text())

                self.assertEqual(set(seed["balance"].values()), {"1000000000"})
                self.assertEqual(seed["gems"][0]["gem_load"], "1000000000")
                day = seed["metadosis"]["worldwide_days"][0]
                self.assertEqual(day["current_vwap"], "1000000")
                self.assertEqual(day["day_limit"], "500000000")
                self.assertEqual(seed["staking"]["min_stake"], staking_expected["min_stake"])
                self.assertEqual(
                    seed["staking"]["genesis_validator_stake"],
                    staking_expected["validator_stake"],
                )
                self.assertEqual(seed["oracle"]["pairs"][0]["initial_rate"], "1000000")
                self.assertEqual(seed["oracle"]["scurve_seeds"][0]["peak_price"], "1000000")

                for nod in seed.get("nods", []):
                    self.assertEqual(nod["gratis_load"], "100000")
                    self.assertEqual(nod["floor_price"], "540000")

    def test_default_usd_currency_rate_uses_scale_1e6_in_slot_60(self):
        storage = seed_genesis.StorageBuilder()

        seed_genesis.seed_oracle(storage, {})

        slot = seed_genesis.mapping_key(seed_genesis.u32_bytes(840), 60)
        self.assertEqual(storage.entries[slot], seed_genesis.hex32(36_300))

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
