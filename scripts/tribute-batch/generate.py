#!/usr/bin/env python3
"""Generate N wallets and distinct Tribute amounts whose USD sum is exact."""
import argparse
from pathlib import Path
import secrets
import sys

from eth_account import Account
from common import (BatchError, CHAIN_ID, MAX_AMOUNT, SCALE, decimal_minor, display_minor,
                    require, save_json, valid_wwd, validate_batch)


def split_amount(total, count):
    require(type(count) is int and count > 0, "Count must be positive")
    # Adding ranks to sorted nonnegative shares guarantees all amounts differ.
    minimum = count * (count + 1) // 2
    require(total >= minimum, "Total is too small for N distinct positive six-decimal amounts")
    remainder = total - minimum
    weights = [secrets.randbelow(1_000_000) + 1 for _ in range(count)]
    weight_sum = sum(weights)
    shares = [remainder * weight // weight_sum for weight in weights]
    for i in range(remainder - sum(shares)):
        shares[i] += 1
    amounts = [share + rank for rank, share in enumerate(sorted(shares), 1)]
    require(max(amounts) <= MAX_AMOUNT, "An amount exceeds Tribute's u64 base domain")
    secrets.SystemRandom().shuffle(amounts)
    return amounts


def generate(count, total, wwd):
    valid_wwd(wwd)
    amounts = split_amount(total, count)
    metadata = {"version": 1, "batch_id": "0x" + secrets.token_hex(32), "chain_id": CHAIN_ID, "wwd": wwd}
    wallets = {**metadata, "accounts": []}
    batch = {**metadata, "count": count, "total_usd": display_minor(total),
             "total_amount_minor": str(total), "tributes": []}
    for amount in amounts:
        account = Account.create(secrets.token_bytes(32))
        wallets["accounts"].append({"address": account.address, "private_key": "0x" + account.key.hex()})
        base, atto = divmod(amount, SCALE)
        batch["tributes"].append({
            "owner": account.address, "worldwide_day": wwd,
            "tribute_currency": 840, "reference_currency": 840,
            "amount_base": str(base), "amount_atto": str(atto), "consumption_usd": display_minor(amount),
            "exclude_from_intex_issuance": False,
            "tribute_draft_id": "0x" + secrets.token_hex(32), "su_hashes": ["0x" + secrets.token_hex(32)],
            "wallet_addresses": [], "sra_addresses": [],
        })
    validate_batch(wallets, batch)
    return wallets, batch


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--count", type=int, required=True)
    parser.add_argument("--total-usd", required=True, help="Exact total, e.g. 150000 or 150000.123456")
    parser.add_argument("--wwd", type=int, required=True)
    parser.add_argument("--out-dir", type=Path, help="New directory; defaults to output/WWD next to this script")
    args = parser.parse_args()
    output = args.out_dir or Path(__file__).resolve().parent / "output" / str(args.wwd)
    require(not output.exists(), f"Output directory already exists: {output}")
    wallets, batch = generate(args.count, decimal_minor(args.total_usd), args.wwd)
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    save_json(output / "wallets.json", wallets)
    save_json(output / "tributes.json", batch)
    print(f"Generated {args.count} wallets and distinct Tribute amounts; total {batch['total_usd']} USD; WWD {args.wwd}")
    print(f"Wallets and private keys: {output / 'wallets.json'}")
    print(f"Tributes: {output / 'tributes.json'}")


if __name__ == "__main__":
    try:
        main()
    except (BatchError, OSError) as error:
        print(f"Error: {error}", file=sys.stderr)
        sys.exit(1)
