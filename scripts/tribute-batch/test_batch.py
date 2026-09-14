import copy
import json
from pathlib import Path
import stat
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey, X25519PublicKey
from cryptography.hazmat.primitives.ciphers.aead import ChaCha20Poly1305
from cryptography.hazmat.primitives.hashes import SHA256
from cryptography.hazmat.primitives.kdf.hkdf import HKDF
from eth_abi import encode
from eth_account import Account
from eth_account._utils.legacy_transactions import Transaction
from hexbytes import HexBytes
from web3 import Web3
from web3.exceptions import TransactionNotFound

from common import BatchError, CHAIN_ID, SCALE, decimal_minor, payload, read_json, save_json, validate_batch
from generate import generate, split_amount
import send


class FakeEth:
    chain_id = CHAIN_ID

    def __init__(self, batch, funder, state):
        self.rows = {row["owner"]: row for row in batch["tributes"]}
        self.balances = {funder.address: 10**18}
        self.nonces, self.receipts = {}, {}
        self.state = state
        self.enclave = X25519PrivateKey.generate()
        self.sends = []
        self.fail_after_accept = False
        self.fail_receipt_once = False
        self.bad_event = False
        self.reverted = False

    def get_block(self, block):
        return {"baseFeePerGas": 8, "number": 1, "timestamp": 100}

    def get_balance(self, owner, block):
        return self.balances.get(owner, 0)

    def get_transaction_count(self, owner, block):
        return self.nonces.get(owner, 0)

    def contract(self, address, abi):
        if address == send.TRIBUTE_ADDR:
            return SimpleNamespace(functions=SimpleNamespace(
                getTributesByOwner=lambda owner: SimpleNamespace(call=lambda: [])))
        return Web3().eth.contract(address=address, abi=abi)

    def get_transaction_receipt(self, tx_hash):
        if self.fail_receipt_once:
            self.fail_receipt_once = False
            raise ConnectionError("lost response after transaction accepted")
        try:
            return self.receipts[HexBytes(tx_hash)]
        except KeyError:
            raise TransactionNotFound("pending") from None

    def send_raw_transaction(self, raw):
        tx_hash = Web3.keccak(raw)
        saved = read_json(self.state)
        journaled = [entry for row in saved["owners"].values()
                     for entry in row["funding"] + ([row["offer"]] if "offer" in row else [])]
        assert any(HexBytes(entry["raw"]) == raw for entry in journaled), "must save before broadcasting"
        tx = Transaction.from_bytes(raw).as_dict()
        owner = Account.recover_transaction(raw)
        to = Web3.to_checksum_address(tx["to"])
        assert (tx["v"] - 35) // 2 == CHAIN_ID
        assert tx["nonce"] == self.nonces.get(owner, 0)
        assert self.balances.get(owner, 0) >= tx["value"] + tx["gas"] * tx["gasPrice"]
        assert tx["gasPrice"] == 16
        self.nonces[owner] = tx["nonce"] + 1
        self.balances[owner] -= tx["value"] + 21_000 * tx["gasPrice"]
        logs = []
        if to == send.TRIBUTE_FACTORY_ADDR:
            assert tx["value"] == 0
            factory = Web3().eth.contract(address=to, abi=send.TRIBUTE_FACTORY_ABI)
            function, args = factory.decode_function_input(tx["data"])
            assert function.fn_name == "offerTribute"
            assert args["worldwideDay"] == 20260913
            assert args["tributeCurrency"] == args["referenceCurrency"] == 840
            assert not args["excludeFromIntexIssuance"]
            assert all(args[name] == b"" for name in ("zkProof", "zkVerificationKey", "zkPublicKey", "zkMerkleRoot", "signature"))
            shared = self.enclave.exchange(X25519PublicKey.from_public_bytes(args["ephemeralPubkey"].to_bytes(32, "big")))
            key = HKDF(algorithm=SHA256(), length=32,
                       salt=b"outbe/tribute/offer-salt/v1".ljust(32, b"\0"),
                       info=b"tribute-factory-encryption").derive(shared)
            decrypted = json.loads(ChaCha20Poly1305(key).decrypt(args["nonce"], args["cipherText"], None))
            assert decrypted == payload(self.rows[owner])
            amount = int(decrypted["amount_base"]) * SCALE + int(decrypted["amount_atto"])
            logs = [{"address": send.TRIBUTE_ADDR,
                     "topics": [send.ISSUED_TOPIC, HexBytes(bytes.fromhex(owner[2:]).rjust(32, b"\0"))],
                     "data": HexBytes(encode(["uint256", "uint32", "uint256", "uint16", "uint256"],
                                             [len(self.sends) + 1, 20260913, amount + int(self.bad_event), 840, amount * 2]))}]
        else:
            assert to in self.rows and tx["data"] == b""
            self.balances[to] = self.balances.get(to, 0) + tx["value"]
        self.sends.append((owner, to))
        self.receipts[tx_hash] = {"transactionHash": tx_hash, "status": 0 if self.reverted else 1, "logs": logs}
        if self.fail_after_accept:
            self.fail_after_accept = False
            self.fail_receipt_once = True
        return tx_hash


class BatchTests(unittest.TestCase):
    def test_400_wallets_exact_150k_unique_amounts_and_micro_remainders(self):
        wallets, batch = generate(400, decimal_minor("150000"), 20260913)
        accounts = validate_batch(wallets, batch)
        self.assertEqual(len(accounts), 400)
        self.assertEqual(batch["total_usd"], "150000.000000")
        values = [int(row["amount_base"]) * SCALE + int(row["amount_atto"]) for row in batch["tributes"]]
        self.assertEqual(sum(values), 150000 * SCALE)
        self.assertEqual(len(set(values)), 400)
        self.assertTrue(all(0 <= int(row["amount_atto"]) < SCALE for row in batch["tributes"]))
        self.assertTrue(any(int(row["amount_atto"]) for row in batch["tributes"]))
        self.assertNotIn("private_key", json.dumps(batch))
        self.assertEqual(sorted(split_amount(10, 4)), [1, 2, 3, 4])
        with self.assertRaises(BatchError):
            split_amount(9, 4)
        self.assertEqual(decimal_minor("1.000001"), 1_000_001)
        for value in ("1.0000001", "-1", "1e18", "0", "NaN"):
            with self.assertRaises(BatchError):
                decimal_minor(value)

    def test_validation_rejects_atto_1e18_and_mismatched_inputs(self):
        wallets, batch = generate(3, 10 * SCALE, 20260913)
        for field, value in (("amount_atto", "1000000000000000000"), ("amount_atto", "1000000"),
                             ("amount_atto", "01"), ("amount_base", 1), ("worldwide_day", 20260914),
                             ("exclude_from_intex_issuance", True)):
            changed = copy.deepcopy(batch)
            changed["tributes"][0][field] = value
            with self.assertRaises(BatchError):
                validate_batch(wallets, changed)
        changed = copy.deepcopy(wallets)
        changed["accounts"][0]["private_key"] = changed["accounts"][1]["private_key"]
        with self.assertRaises(BatchError):
            validate_batch(changed, batch)
        changed = copy.deepcopy(batch)
        changed["total_usd"] = "999"
        with self.assertRaises(BatchError):
            validate_batch(wallets, changed)

    def test_private_files_are_not_overwritten(self):
        wallets, _ = generate(1, SCALE, 20260913)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "wallets.json"
            save_json(path, wallets)
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            with self.assertRaises(BatchError):
                save_json(path, {})
            self.assertEqual(read_json(path), wallets)

    def run_sender(self, scenario):
        wallets, batch = generate(3, decimal_minor("1500.123456"), 20260913)
        accounts = validate_batch(wallets, batch)
        funder = Account.create()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state.json"
            eth = FakeEth(batch, funder, path)
            w3 = SimpleNamespace(eth=eth)
            with patch.object(send, "offering", return_value=(True, [2, 1, 0, 0, 0, 9999999999])), \
                 patch.object(send, "offer_key", return_value=eth.enclave.public_key().public_bytes_raw()):
                scenario(w3, batch, accounts, path, funder, eth)

    def test_signed_funding_and_encrypted_tribute_resume_without_duplicates(self):
        def scenario(w3, batch, accounts, path, funder, eth):
            eth.fail_after_accept = True
            with self.assertRaises(ConnectionError):
                send.Sender(w3, batch, accounts, path, funder).run()
            self.assertEqual(len(eth.sends), 1)
            send.Sender(w3, batch, accounts, path, funder).run()
            self.assertEqual(len(eth.sends), 6)
            send.Sender(w3, batch, accounts, path, funder).run()
            self.assertEqual(len(eth.sends), 6)
            self.assertEqual(len(read_json(path)["owners"]), 3)
            self.assertTrue(all("tribute_id" in row for row in read_json(path)["owners"].values()))
        self.run_sender(scenario)

    def test_closed_offering_dry_run_and_missing_funds_do_not_broadcast(self):
        def scenario(w3, batch, accounts, path, funder, eth):
            with patch.object(send, "offering", return_value=(False, [0, 0, 0, 0, 0, 9999999999])):
                send.Sender(w3, batch, accounts, path).run(dry_run=True)
                with self.assertRaises(BatchError):
                    send.Sender(w3, batch, accounts, path, funder).run()
            with self.assertRaises(BatchError):
                send.Sender(w3, batch, accounts, path).run()
            self.assertFalse(path.exists())
            self.assertEqual(eth.sends, [])
        self.run_sender(scenario)

    def test_wrong_chain_and_mismatched_receipt_stop_batch(self):
        def scenario(w3, batch, accounts, path, funder, eth):
            eth.chain_id = 1
            with self.assertRaises(BatchError):
                send.Sender(w3, batch, accounts, path, funder).run()
            self.assertEqual(eth.sends, [])
            eth.chain_id = CHAIN_ID
            eth.bad_event = True
            with self.assertRaises(BatchError):
                send.Sender(w3, batch, accounts, path, funder).run()
            self.assertEqual(len(eth.sends), 2)
            self.assertNotIn("tribute_id", next(iter(read_json(path)["owners"].values())))
        self.run_sender(scenario)

    def test_reverted_funding_is_not_reported_as_success(self):
        def scenario(w3, batch, accounts, path, funder, eth):
            eth.reverted = True
            with self.assertRaisesRegex(BatchError, "reverted"):
                send.Sender(w3, batch, accounts, path, funder).run()
            self.assertEqual(len(eth.sends), 1)
        self.run_sender(scenario)


if __name__ == "__main__":
    unittest.main()
