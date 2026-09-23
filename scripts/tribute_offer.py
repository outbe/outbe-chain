#!/usr/bin/env python3
"""Submit an encrypted Tribute offer to outbe-chain without the Rust CLI.

Byte-for-byte port of `outbe-cli tribute offer`:

  1. read the DKG-derived offer public key from the TeeRegistry (0xEE0A)
  2. (optional) auto-detect the WorldwideDay currently in OFFERING via Metadosis
  3. encrypt the payload to the offer key:
        ephemeral X25519 ECDHE
        -> HKDF-SHA256(salt=OFFER_HKDF_SALT, info="tribute-factory-encryption")
        -> ChaCha20Poly1305 (empty AAD)
     identical to outbe_tee_enclave::crypto::ecdhe_offer_decrypt
  4. ABI-encode + sign (legacy EIP-155) + send `offerTribute` to the
     TributeFactory (0x1100). The enclave decrypts it inside execution.

ZK verification is mandatory: the offer must carry the combined Tribute proof,
the L2 Merkle root it commits to and the network's BLS signature over that root,
plus the circuit chain id/version the proof verifies under. The proof is produced
on the L2 - this script never generates one. `tribute_draft_id`, `su_hash`(es),
`--amount` and `--amount-micro` must be the values that proof and the caller's L2
attestation bind: the enclave folds them into the `nft_hash` the node checks
against the proof's public input (and against the registered L2 chain).

Deps:  pip install web3 cryptography

Examples:
  # auto-pick the OFFERING day, default amount 100 / currency 840 (USD)
  python3 scripts/tribute_offer.py \
      --rpc https://rpc.testnet.outbe.net \
      --private-key 0x<KEY> \
      --zk-proof 0x<COMBINED_PROOF> --zk-merkle-root 0x<ROOT> \
      --signature 0x<BLS_SIG> --chain-id 57005 --version 1.1.0 \
      --tribute-draft-id 0x<32-byte draft id> --su-hash 0x<32-byte su hash>

  # explicit day
  python3 scripts/tribute_offer.py --rpc https://rpc.testnet.outbe.net \
      --private-key 0x<KEY> --day 20260601 --amount 100 --currency 840 \
      --zk-proof 0x<COMBINED_PROOF> --zk-merkle-root 0x<ROOT> \
      --signature 0x<BLS_SIG> --chain-id 57005 --version 1.1.0 \
      --tribute-draft-id 0x<DRAFT_ID> --su-hash 0x<SU_HASH>

  # deliberate negative offer (the node rejects it): empty proof/root/signature
  python3 scripts/tribute_offer.py ... --zk-proof 0x --zk-merkle-root 0x \
      --signature 0x --chain-id 57005 --version 1.1.0 \
      --tribute-draft-id 0x<DRAFT_ID> --su-hash 0x<SU_HASH>
"""

import argparse
import json
import os
import re
import sys
import time

from cryptography.hazmat.primitives.asymmetric.x25519 import (
    X25519PrivateKey,
    X25519PublicKey,
)
from cryptography.hazmat.primitives.ciphers.aead import ChaCha20Poly1305
from cryptography.hazmat.primitives.kdf.hkdf import HKDF
from cryptography.hazmat.primitives.hashes import SHA256

from web3 import Web3
from eth_account import Account

# --- Protocol addresses (see crates/blockchain/primitives/src/addresses.rs) ---
TEE_REGISTRY_ADDR = Web3.to_checksum_address("0x000000000000000000000000000000000000EE0A")
METADOSIS_ADDR = Web3.to_checksum_address("0x000000000000000000000000000000000000100E")
TRIBUTE_FACTORY_ADDR = Web3.to_checksum_address("0x0000000000000000000000000000000000001100")
TRIBUTE_ADDR = Web3.to_checksum_address("0x0000000000000000000000000000000000001101")

# Fixed enclave offer salt + HKDF info label (must match the enclave).
# Value: outbe_tee::OFFER_HKDF_SALT = ASCII "outbe/tribute/offer-salt/v1",
# zero-padded to 32 bytes (see crates/system/tee/src/lib.rs and
# bin/outbe-tee-enclave/src/keys.rs).
OFFER_SALT = b"outbe/tribute/offer-salt/v1".ljust(32, b"\0")
HKDF_INFO = b"tribute-factory-encryption"

# WorldwideDay status values (crates/core/metadosis/src/schema.rs::status).
STATUS_OFFERING = 2

TEE_REGISTRY_ABI = json.loads(
    """[
      {"type":"function","name":"isBootstrapped","stateMutability":"view",
       "inputs":[],"outputs":[{"type":"bool"}]},
      {"type":"function","name":"tributeOfferPublicKey","stateMutability":"view",
       "inputs":[],"outputs":[{"type":"uint256"}]}
    ]"""
)

METADOSIS_ABI = json.loads(
    """[
      {"type":"function","name":"getWorldwideDaysByStatus","stateMutability":"view",
       "inputs":[{"type":"uint8","name":"status"}],
       "outputs":[{"type":"uint32[]","name":"wwds"}]}
    ]"""
)

TRIBUTE_FACTORY_ABI = json.loads(
    """[
      {"type":"function","name":"offerTribute","stateMutability":"nonpayable",
       "inputs":[
         {"type":"bytes","name":"cipherText"},
         {"type":"bytes","name":"nonce"},
         {"type":"uint256","name":"ephemeralPubkey"},
         {"type":"uint32","name":"worldwideDay"},
         {"type":"uint16","name":"tributeCurrency"},
         {"type":"uint16","name":"referenceCurrency"},
         {"type":"bool","name":"excludeFromIntexIssuance"},
         {"type":"bytes","name":"zkProof"},
         {"type":"uint32","name":"chainId"},
         {"type":"string","name":"version"},
         {"type":"bytes","name":"zkPublicKey"},
         {"type":"bytes","name":"zkMerkleRoot"},
         {"type":"bytes","name":"signature"}
       ],
       "outputs":[{"type":"uint256","name":"tributeId"}]}
    ]"""
)

TRIBUTE_ABI = json.loads(
    """[
      {"type":"function","name":"getTributesByOwner","stateMutability":"view",
       "inputs":[{"type":"address","name":"owner"}],
       "outputs":[{"type":"uint256[]"}]}
    ]"""
)


def encrypt_offer(offer_pub: bytes, plaintext: bytes):
    """Ephemeral X25519 ECDHE -> HKDF-SHA256 -> ChaCha20Poly1305 (empty AAD).

    Returns (cipher_text_with_tag, nonce_12, ephemeral_pub_32).
    """
    eph_priv = X25519PrivateKey.generate()
    eph_pub = eph_priv.public_key().public_bytes_raw()  # 32 raw bytes
    shared = eph_priv.exchange(X25519PublicKey.from_public_bytes(offer_pub))

    key = HKDF(
        algorithm=SHA256(), length=32, salt=OFFER_SALT, info=HKDF_INFO
    ).derive(shared)

    nonce = os.urandom(12)
    cipher_text = ChaCha20Poly1305(key).encrypt(nonce, plaintext, None)
    return cipher_text, nonce, eph_pub


def pick_offering_day(w3: Web3) -> int:
    md = w3.eth.contract(address=METADOSIS_ADDR, abi=METADOSIS_ABI)
    days = md.functions.getWorldwideDaysByStatus(STATUS_OFFERING).call()
    if not days:
        sys.exit("no WorldwideDay is currently in OFFERING status")
    if len(days) > 1:
        print(f"multiple OFFERING days {days}; using {days[0]}")
    return int(days[0])


def canonical_amount_base(value: str) -> str:
    if not value or not value.isascii() or not value.isdigit():
        raise argparse.ArgumentTypeError("amount_base must be a canonical unsigned u64")
    parsed = int(value)
    if str(parsed) != value or parsed > 18_446_744_073_709_551_615:
        raise argparse.ArgumentTypeError("amount_base must be a canonical unsigned u64")
    return value


def canonical_amount_micro(value: str) -> str:
    if not value or not value.isascii() or not value.isdigit():
        raise argparse.ArgumentTypeError("amount_micro must be a canonical unsigned u64 below 1000000")
    parsed = int(value)
    if str(parsed) != value or parsed >= 1_000_000:
        raise argparse.ArgumentTypeError("amount_micro must be a canonical unsigned u64 below 1000000")
    return value


_HEX = re.compile(r"\A(?:[0-9a-fA-F]{2})*\Z")


def hex_bytes_arg(value: str) -> bytes:
    """`0x`-hex flag value -> bytes. `0x` alone is empty and left to the node."""
    raw = value.removeprefix("0x")
    if not _HEX.match(raw):
        raise argparse.ArgumentTypeError("must be 0x-prefixed hex")
    return bytes.fromhex(raw)


def hex32_arg(value: str) -> bytes:
    """`0x`-hex of exactly 32 bytes - the enclave parses these as B256."""
    data = hex_bytes_arg(value)
    if len(data) != 32:
        raise argparse.ArgumentTypeError("must be exactly 32 bytes of 0x-hex")
    return data


def uint32_arg(value: str) -> int:
    parsed = int(value)
    if not 0 <= parsed <= 0xFFFF_FFFF:
        raise argparse.ArgumentTypeError("must fit uint32")
    return parsed


def main() -> None:
    ap = argparse.ArgumentParser(description="Submit an encrypted Tribute offer")
    ap.add_argument("--rpc", required=True, help="JSON-RPC endpoint URL")
    ap.add_argument("--private-key", required=True, help="signer private key (hex)")
    ap.add_argument("--day", type=int, default=None,
                    help="WorldwideDay (YYYYMMDD); auto-detect OFFERING if omitted")
    ap.add_argument(
        "--amount",
        default="100",
        help="canonical unsigned amount_base in whole units; must match the proof's draft",
    )
    ap.add_argument(
        "--amount-micro",
        default="0",
        help="six-decimal remainder in [0,999999]; must match the proof's draft",
    )
    ap.add_argument("--currency", type=int, default=840, help="ISO 4217 code (840=USD)")
    ap.add_argument("--exclude-from-intex-issuance", action="store_true",
                    help="set the excludeFromIntexIssuance flag")
    ap.add_argument("--zk-proof", required=True, type=hex_bytes_arg,
                    help="combined Tribute proof bytes (0x-hex): 4-byte public-input word "
                         "count, public inputs, proof; 0x is a deliberate negative "
                         "offer the node rejects")
    ap.add_argument("--zk-merkle-root", required=True, type=hex_bytes_arg,
                    help="L2 Merkle root the proof's public input commits to (0x-hex)")
    ap.add_argument("--signature", required=True, type=hex_bytes_arg,
                    help="BLS MinSig signature over --zk-merkle-root by the network "
                         "key registered in the L2Registry (0x-hex)")
    ap.add_argument("--chain-id", required=True, type=uint32_arg,
                    help="L2 chain id the proof verifies under; must be the caller's "
                         "registered L2")
    ap.add_argument("--version", required=True,
                    help="exact circuit version enabled for that chain, e.g. 1.1.0")
    ap.add_argument("--tribute-draft-id", required=True, type=hex32_arg,
                    help="32-byte TributeDraft id the proof and the caller's L2 "
                         "attestation bind (0x-hex)")
    ap.add_argument("--su-hash", required=True, action="append", type=hex32_arg,
                    help="32-byte SpendingUnit hash bound by the proof (0x-hex, repeatable)")
    ap.add_argument("--gas", type=int, default=8_000_000, help="explicit gas limit")
    ap.add_argument("--wait", action="store_true", help="wait for the receipt")
    args = ap.parse_args()
    amount_base = canonical_amount_base(args.amount)
    amount_micro = canonical_amount_micro(args.amount_micro)

    w3 = Web3(Web3.HTTPProvider(args.rpc))
    acct = Account.from_key(args.private_key)
    creator = acct.address
    print(f"signer: {creator}")

    # 1. offer key from the TeeRegistry
    reg = w3.eth.contract(address=TEE_REGISTRY_ADDR, abi=TEE_REGISTRY_ABI)
    if not reg.functions.isBootstrapped().call():
        sys.exit("TeeRegistry is not bootstrapped - no offer key to encrypt to")
    offer_pub_u256 = reg.functions.tributeOfferPublicKey().call()
    offer_pub = int(offer_pub_u256).to_bytes(32, "big")
    print(f"offer key (DKG-derived): 0x{offer_pub.hex()}")

    # 2. day
    day = args.day if args.day is not None else pick_offering_day(w3)
    print(f"worldwide_day: {day}")

    # 3. plaintext payload - draft id + su hashes must be the proof-bound values,
    #    since the enclave folds them into the nft_hash checked against the proof.
    #    worldwide_day + currency travel as cleartext ABI args, not in here.
    payload = {
        "creator": creator,
        "tribute_draft_id": "0x" + args.tribute_draft_id.hex(),
        "amount_base": amount_base,
        "amount_micro": amount_micro,
        "su_hashes": ["0x" + su_hash.hex() for su_hash in args.su_hash],
        "wallet_addresses": [],
        "sra_addresses": [],
    }
    plaintext = json.dumps(payload, separators=(",", ":")).encode()

    # 4. encrypt to the offer key
    cipher_text, nonce, eph_pub = encrypt_offer(offer_pub, plaintext)

    # 5. build + sign + send offerTribute (msg.value MUST be 0). The ZK gate
    #    (proof, root, BLS signature, circuit selector) is the node's call: the
    #    values go through verbatim, including empty ones for negative offers.
    factory = w3.eth.contract(address=TRIBUTE_FACTORY_ADDR, abi=TRIBUTE_FACTORY_ABI)
    tx = factory.functions.offerTribute(
        cipher_text,
        nonce,
        int.from_bytes(eph_pub, "big"),
        int(day),
        int(args.currency),
        int(args.currency),
        args.exclude_from_intex_issuance,
        args.zk_proof,
        args.chain_id,
        args.version,
        b"",  # zkPublicKey: the combined proof carries its own
        args.zk_merkle_root,
        args.signature,
    ).build_transaction(
        {
            "from": creator,
            "value": 0,
            "nonce": w3.eth.get_transaction_count(creator),
            "gas": args.gas,  # estimateGas can't simulate the in-enclave decrypt
            "gasPrice": w3.eth.gas_price,
            "chainId": w3.eth.chain_id,
        }
    )
    signed = acct.sign_transaction(tx)
    tx_hash = w3.eth.send_raw_transaction(signed.raw_transaction)
    print(f"offerTribute tx: {tx_hash.hex()}")
    print(f"  creator={creator} worldwide_day={day} "
          f"currency={args.currency} amount_base={amount_base}")
    print(f"  l2_chain_id={args.chain_id} circuit_version={args.version} "
          f"tribute_draft_id=0x{args.tribute_draft_id.hex()}")

    if not args.wait:
        print(f"verify once mined: getTributesByOwner({creator}) on {TRIBUTE_ADDR}")
        return

    print("waiting for receipt...")
    rcpt = w3.eth.wait_for_transaction_receipt(tx_hash, timeout=180)
    print(f"status: {rcpt.status} (block {rcpt.blockNumber}, gas {rcpt.gasUsed})")
    if rcpt.status != 1:
        sys.exit("offer reverted - inspect the revert reason via the node")

    time.sleep(1)
    tribute = w3.eth.contract(address=TRIBUTE_ADDR, abi=TRIBUTE_ABI)
    owned = tribute.functions.getTributesByOwner(creator).call()
    print(f"tributes owned by {creator}: {len(owned)}")
    for tid in owned:
        print(f"  - {tid}")


if __name__ == "__main__":
    main()
