#!/usr/bin/env python3
"""Send a generated USD Tribute batch directly through Rudis JSON-RPC."""
import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
from datetime import datetime, timezone
import fcntl
import json
import os
from pathlib import Path
import sys
import time

from eth_abi import decode
from hexbytes import HexBytes
from web3 import Web3
from web3.exceptions import TransactionNotFound
from web3.middleware import ExtraDataToPOAMiddleware

from common import (BatchError, CHAIN_ID, SCALE, account_from_key, address, batch_digest,
                    decimal_minor, display_minor, payload, read_json, require, save_json, validate_batch)

# Reuse the existing Python implementation of the canonical enclave encryption recipe and ABI.
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from tribute_offer import (encrypt_offer, TEE_REGISTRY_ADDR, TEE_REGISTRY_ABI,
                           METADOSIS_ADDR, TRIBUTE_FACTORY_ADDR, TRIBUTE_FACTORY_ABI,
                           TRIBUTE_ADDR, TRIBUTE_ABI)

DAY_ABI = [{"type": "function", "name": "getWorldwideDay", "stateMutability": "view",
            "inputs": [{"name": "wwd", "type": "uint32"}],
            "outputs": [{"type": value} for value in
                        ("uint8", "uint8", "uint64", "uint64", "uint64", "uint64", "uint64", "uint256", "uint256")]}]
ISSUED_TOPIC = Web3.keccak(text="TributeIssued(address,uint256,uint32,uint256,uint16,uint256)")


def chain_check(w3):
    require(w3.eth.chain_id == CHAIN_ID, f"Expected Rudis chain {CHAIN_ID}")


def gas_price(w3):
    # Avoid compounding a tip-inclusive eth_gasPrice suggestion on our own transactions.
    return max(1, int(w3.eth.get_block("latest")["baseFeePerGas"]) * 2)


def offering(w3, day):
    block = w3.eth.get_block("latest")
    record = w3.eth.contract(address=METADOSIS_ADDR, abi=DAY_ABI).functions.getWorldwideDay(day).call(
        block_identifier=block["number"])
    is_open = record[0] == 2 and record[4] <= block["timestamp"] < record[5]
    return is_open, record


def offer_key(w3):
    registry = w3.eth.contract(address=TEE_REGISTRY_ADDR, abi=TEE_REGISTRY_ABI)
    require(registry.functions.isBootstrapped().call(), "TEE registry is not bootstrapped")
    key = registry.functions.tributeOfferPublicKey().call().to_bytes(32, "big")
    require(any(key), "TEE offer key is zero")
    return key


def calldata(w3, row, key):
    plaintext = json.dumps(payload(row), separators=(",", ":")).encode()
    cipher, nonce, ephemeral = encrypt_offer(key, plaintext)
    factory = w3.eth.contract(address=TRIBUTE_FACTORY_ADDR, abi=TRIBUTE_FACTORY_ABI)
    return factory.encode_abi("offerTribute", args=[
        cipher, nonce, int.from_bytes(ephemeral, "big"), row["worldwide_day"],
        row["tribute_currency"], row["reference_currency"], False,
        b"", b"", b"", b"", b"",  # Fresh ordinary owners are not registered L2 operators.
    ])


def signed_transaction(w3, account, to, value, data, gas, price):
    chain_check(w3)
    require(w3.eth.get_balance(account.address, "pending") >= value + gas * price,
            f"Insufficient gas funds for {account.address}")
    tx = {"chainId": CHAIN_ID, "nonce": w3.eth.get_transaction_count(account.address, "pending"),
          "to": to, "value": value, "data": data, "gas": gas, "gasPrice": price}
    signed = account.sign_transaction(tx)
    return {"hash": signed.hash.to_0x_hex(), "raw": signed.raw_transaction.to_0x_hex()}


def confirmed(w3, saved, timeout):
    raw = HexBytes(saved["raw"])
    tx_hash = Web3.keccak(raw)
    require(tx_hash == HexBytes(saved["hash"]), "Saved transaction hash mismatch")
    def receipt():
        try:
            return w3.eth.get_transaction_receipt(tx_hash)
        except TransactionNotFound:
            return None
    result = receipt()
    if result is None:
        chain_check(w3)
        try:
            returned = w3.eth.send_raw_transaction(raw)
        except Exception:
            print(f"Broadcast not acknowledged; checking saved hash {tx_hash.to_0x_hex()}", flush=True)
        else:
            require(HexBytes(returned) == tx_hash, "RPC returned an unexpected transaction hash")
        deadline = time.monotonic() + timeout
        while result is None and time.monotonic() < deadline:
            result = receipt()
            if result is None:
                time.sleep(1)
    require(result is not None, f"Pending {tx_hash.to_0x_hex()}; rerun the same command to resume")
    require(HexBytes(result["transactionHash"]) == tx_hash, "Receipt hash mismatch")
    require(result["status"] == 1, f"Transaction reverted: {tx_hash.to_0x_hex()}; inspect before retrying")
    return result


def issued_event(receipt, row):
    matches = [log for log in receipt["logs"]
               if address(log["address"]) == TRIBUTE_ADDR and log["topics"]
               and HexBytes(log["topics"][0]) == ISSUED_TOPIC]
    require(len(matches) == 1, "Receipt must contain exactly one TributeIssued event")
    log = matches[0]
    require(len(log["topics"]) == 2, "Invalid TributeIssued topics")
    owner = address("0x" + HexBytes(log["topics"][1])[-20:].hex())
    token, day, amount, currency, nominal = decode(
        ["uint256", "uint32", "uint256", "uint16", "uint256"], HexBytes(log["data"]))
    require(owner == address(row["owner"]) and day == row["worldwide_day"]
            and amount == int(row["amount_base"]) * SCALE + int(row["amount_atto"])
            and currency == 840 and nominal > 0, "TributeIssued differs from the requested Tribute")
    return f"0x{token:064x}"


@contextmanager
def locked_state(path):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    lock_path = path.with_suffix(path.suffix + ".lock")
    fd = os.open(lock_path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise BatchError("Another sender is using this batch state") from None
        yield
    finally:
        os.close(fd)


class Sender:
    def __init__(self, w3, batch, accounts, state_path, funder=None, fund_target=10**15, gas=8_000_000, timeout=120):
        self.w3, self.batch, self.accounts = w3, batch, accounts
        self.path, self.funder, self.fund_target, self.gas, self.timeout = state_path, funder, fund_target, gas, timeout
        self.state = read_json(state_path) if state_path.exists() else {
            "version": 1, "batch_digest": batch_digest(batch), "chain_id": CHAIN_ID, "owners": {}}
        require(self.state.get("version") == 1 and self.state.get("batch_digest") == batch_digest(batch)
                and self.state.get("chain_id") == CHAIN_ID, "State belongs to a different or modified batch")

    def save(self):
        save_json(self.path, self.state, replace=True)

    def run(self, dry_run=False):
        chain_check(self.w3)
        is_open, day = offering(self.w3, self.batch["wwd"])
        key = offer_key(self.w3)
        price = gas_price(self.w3)
        opening = datetime.fromtimestamp(day[4], timezone.utc).isoformat()
        print(f"WWD {self.batch['wwd']}: status={day[0]}, dayType={day[1]}, offering opens {opening}", flush=True)
        print(f"{self.batch['count']} Tributes, consumption {self.batch['total_usd']} USD; amount_atto scale=1000000", flush=True)
        if dry_run:
            with ThreadPoolExecutor(max_workers=8) as pool:
                balances = list(pool.map(lambda owner: self.w3.eth.get_balance(owner, "pending"), self.accounts))
            deficits = [max(0, max(self.fund_target, self.gas * price) - value)
                        if value < self.gas * price else 0 for value in balances]
            for row in self.batch["tributes"]:
                calldata(self.w3, row, key)
            print(f"Dry run: {sum(value > 0 for value in deficits)} wallets need gas funding; "
                  f"top-ups {display_minor(sum(deficits), 18)} RUDIS (funding gas extra)")
            print("Offering open." if is_open else "Offering is closed; normal send will stop before funding new offers.")
            print("No transactions sent and no state files written.")
            return
        for index, row in enumerate(self.batch["tributes"], 1):
            owner = address(row["owner"])
            record = self.state["owners"].setdefault(owner, {"funding": []})
            if "offer" not in record:
                chain_check(self.w3)
                require(offering(self.w3, row["worldwide_day"])[0], "WWD is not open for offering; no new transaction sent")
                existing = self.w3.eth.contract(address=TRIBUTE_ADDR, abi=TRIBUTE_ABI).functions.getTributesByOwner(owner).call()
                require(not existing, f"{owner} already owns a Tribute outside this journal; inspect before continuing")
                for funding in record["funding"]:
                    confirmed(self.w3, funding, self.timeout)
                price = gas_price(self.w3)
                balance = self.w3.eth.get_balance(owner, "pending")
                if balance < self.gas * price:
                    require(self.funder is not None, f"{owner} needs gas: provide --funding-private-key or fund the addresses first")
                    amount = max(self.fund_target, self.gas * price) - balance
                    funding = signed_transaction(self.w3, self.funder, owner, amount, "0x", 21_000, price)
                    record["funding"].append(funding)
                    self.save()
                    print(f"[{index}/{self.batch['count']}] funding {owner}: {funding['hash']}", flush=True)
                    confirmed(self.w3, funding, self.timeout)
                require(offering(self.w3, row["worldwide_day"])[0], "Offering closed before Tribute submission")
                # Fetch a fresh registry key for each newly encrypted offer; saved signed offers stay immutable.
                data = calldata(self.w3, row, offer_key(self.w3))
                record["offer"] = signed_transaction(self.w3, self.accounts[owner], TRIBUTE_FACTORY_ADDR,
                                                       0, data, self.gas, gas_price(self.w3))
                self.save()  # Signed bytes are durable BEFORE broadcast, including funding transactions.
            print(f"[{index}/{self.batch['count']}] {owner}, {row['consumption_usd']} USD: {record['offer']['hash']}", flush=True)
            receipt = confirmed(self.w3, record["offer"], self.timeout)
            record["tribute_id"] = issued_event(receipt, row)
            self.save()
        print(f"Done: {self.batch['count']} confirmed Tributes, {self.batch['total_usd']} USD consumption", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wallets", required=True, type=Path)
    parser.add_argument("--tributes", required=True, type=Path)
    parser.add_argument("--rpc-url", default="https://125.253.92.5")
    parser.add_argument("--funding-private-key", help="Optional existing funded wallet used ONLY for gas top-ups")
    parser.add_argument("--fund-rudis", default="0.001", help="Minimum funded balance per owner, in native RUDIS")
    parser.add_argument("--gas-limit", type=int, default=8_000_000)
    parser.add_argument("--receipt-timeout", type=int, default=120)
    parser.add_argument("--state", type=Path, help="Defaults to tributes.state.json next to tributes.json")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    require(args.gas_limit > 0 and args.receipt_timeout > 0, "Gas and timeout must be positive")
    wallets, batch = read_json(args.wallets), read_json(args.tributes)
    accounts = validate_batch(wallets, batch)
    funder = account_from_key(args.funding_private_key) if args.funding_private_key else None
    require(funder is None or funder.address not in accounts, "Funding wallet must be separate from generated owners")
    state = args.state or args.tributes.with_suffix(".state.json")
    require(state.resolve() not in (args.wallets.resolve(), args.tributes.resolve()), "State must not overwrite input files")
    w3 = Web3(Web3.HTTPProvider(args.rpc_url, request_kwargs={"timeout": 30}))
    # Rudis carries consensus artifacts longer than Ethereum's 32-byte extraData limit.
    w3.middleware_onion.inject(ExtraDataToPOAMiddleware, layer=0)
    def run():
        Sender(w3, batch, accounts, state, funder, decimal_minor(args.fund_rudis, 18),
               args.gas_limit, args.receipt_timeout).run(args.dry_run)
    if args.dry_run:
        run()
    else:
        with locked_state(state):
            run()


if __name__ == "__main__":
    try:
        main()
    except BatchError as error:
        print(f"Error: {error}", file=sys.stderr)
        sys.exit(1)
    except KeyboardInterrupt:
        print("Interrupted. Rerun with the same JSON files and state to resume.", file=sys.stderr)
        sys.exit(130)
    except Exception as error:
        # RPC/parser exception strings can contain request material; never print keys or raw request bodies.
        print(f"Error: {type(error).__name__}. Check inputs/RPC; saved transactions remain available for resume.", file=sys.stderr)
        sys.exit(1)
