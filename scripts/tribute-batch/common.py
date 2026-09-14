"""Exact amounts, batch validation and private JSON files for Rudis Tribute tools."""
import hashlib
import json
import os
from pathlib import Path
import re
import tempfile
from datetime import datetime

from eth_account import Account
from web3 import Web3

CHAIN_ID = 70860602
SCALE = 1_000_000
MAX_AMOUNT = (2**64 - 1) * SCALE + SCALE - 1


class BatchError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise BatchError(message)


def decimal_minor(text, decimals=6):
    require(isinstance(text, str) and re.fullmatch(r"[0-9]+(?:\.[0-9]+)?", text),
            "Amount must be a positive decimal string")
    whole, _, fraction = text.partition(".")
    require(len(fraction) <= decimals, f"Amount supports at most {decimals} decimals")
    value = int(whole) * 10**decimals + int(fraction.ljust(decimals, "0") or "0")
    require(value > 0, "Amount must be positive")
    return value


def display_minor(value, decimals=6):
    whole, fraction = divmod(value, 10**decimals)
    return f"{whole}.{fraction:0{decimals}d}"


def canonical_uint(text, maximum, field):
    require(isinstance(text, str) and re.fullmatch(r"0|[1-9][0-9]*", text),
            f"{field} must be a canonical unsigned decimal string")
    value = int(text)
    require(value <= maximum, f"{field} exceeds {maximum}")
    return value


def valid_wwd(day):
    require(type(day) is int and len(str(day)) == 8, "WWD must be YYYYMMDD")
    try:
        datetime.strptime(str(day), "%Y%m%d")
    except ValueError:
        raise BatchError("Invalid WWD calendar date") from None
    return day


def address(text):
    require(isinstance(text, str) and Web3.is_address(text), "Invalid wallet address")
    return Web3.to_checksum_address(text)


def account_from_key(key):
    try:
        return Account.from_key(key)
    except Exception:
        raise BatchError("Invalid private key in wallets file or funding option") from None


def hex32(value):
    return isinstance(value, str) and re.fullmatch(r"0x[0-9a-fA-F]{64}", value) is not None


def read_json(path):
    path = Path(path)
    require(path.is_file() and not path.is_symlink(), f"Not a regular JSON file: {path}")
    try:
        return json.loads(path.read_text())
    except (ValueError, UnicodeError):
        raise BatchError(f"Invalid JSON file: {path}") from None


def save_json(path, value, replace=False):
    path = Path(path)
    require(not path.is_symlink(), f"Refusing symlink: {path}")
    require(replace or not path.exists(), f"File already exists: {path}")
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd, temporary = tempfile.mkstemp(dir=path.parent, prefix=".writing-")
    try:
        with os.fdopen(fd, "w") as output:
            json.dump(value, output, indent=2, ensure_ascii=True)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        if replace:
            os.replace(temporary, path)
        else:
            os.link(temporary, path)  # Atomic publish without replacing existing secrets.
            os.unlink(temporary)
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def batch_digest(batch):
    return hashlib.sha256(json.dumps(batch, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def validate_batch(wallets, batch):
    require(isinstance(wallets, dict) and isinstance(batch, dict), "Expected JSON objects")
    for key in ("version", "batch_id", "chain_id", "wwd"):
        require(key in wallets and wallets[key] == batch.get(key), f"Batch mismatch: {key}")
    require(batch["version"] == 1 and batch["chain_id"] == CHAIN_ID, "Unsupported version or chain")
    valid_wwd(batch["wwd"])
    require(hex32(batch["batch_id"]), "Invalid batch ID")
    entries = batch.get("tributes")
    require(isinstance(entries, list) and entries, "Empty or invalid Tribute list")
    require(batch.get("count") == len(entries), "Tribute count mismatch")
    require(isinstance(wallets.get("accounts"), list), "Invalid wallet list")
    keys = {}
    for row in wallets["accounts"]:
        owner = address(row["address"])
        require(owner not in keys, "Duplicate wallet")
        account = account_from_key(row["private_key"])
        require(account.address == owner, "Private key does not match wallet address")
        keys[owner] = account
    require(len(keys) == len(entries), "Wallet and Tribute counts differ")
    owners, amounts, drafts, hashes = set(), set(), set(), set()
    total = 0
    for row in entries:
        owner = address(row["owner"])
        require(owner in keys and owner not in owners, "Missing wallet or duplicate Tribute owner")
        owners.add(owner)
        require(row.get("worldwide_day") == batch["wwd"], "Tribute WWD mismatch")
        require(row.get("tribute_currency") == 840 and row.get("reference_currency") == 840,
                "This batch must use USD (840) for both currencies")
        require(row.get("exclude_from_intex_issuance") is False, "Tribute must participate in Intex")
        base = canonical_uint(row["amount_base"], 2**64 - 1, "amount_base")
        atto = canonical_uint(row["amount_atto"], SCALE - 1, "amount_atto")
        amount = base * SCALE + atto
        require(amount > 0 and amount not in amounts, "Amounts must be positive and distinct")
        require(decimal_minor(row["consumption_usd"]) == amount, "Consumption amount mismatch")
        amounts.add(amount)
        total += amount
        draft = row["tribute_draft_id"]
        require(hex32(draft) and draft.lower() not in drafts, "Invalid or duplicate draft ID")
        drafts.add(draft.lower())
        require(isinstance(row["su_hashes"], list) and row["su_hashes"], "Missing SU hashes")
        for su in row["su_hashes"]:
            require(hex32(su) and su.lower() not in hashes, "Invalid or duplicate SU hash")
            hashes.add(su.lower())
        require(row.get("wallet_addresses") == [] and row.get("sra_addresses") == [],
                "Unexpected reward addresses in batch")
    require(canonical_uint(batch["total_amount_minor"], MAX_AMOUNT * len(entries), "total_amount_minor") == total,
            "Total minor amount mismatch")
    require(decimal_minor(batch["total_usd"]) == total, "Total USD mismatch")
    return keys


def payload(row):
    return {"creator": address(row["owner"]), **{key: row[key] for key in (
        "tribute_draft_id", "amount_base", "amount_atto", "su_hashes", "wallet_addresses", "sra_addresses"
    )}}
