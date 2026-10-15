"""Finite cycle-free OGD1/OPB1 design vectors. Not production genesis or DKG."""
import hashlib
import struct
from pathlib import Path

sha = lambda data: hashlib.sha256(data).digest()
u64 = lambda n: struct.pack(">Q", n)
tree = hashlib.sha1(b"tree 0\0").digest()
content = sha(b"[]")
name, branch = b"governments", b"main"
description = b"Network governance"


def encode(chain_id, timestamp, description):
    assert len(description) <= 1024
    description.decode("utf-8")
    assert b"\r" not in description and b"\0" not in description
    text = lambda value: struct.pack(">H", len(value)) + value
    return (b"OGD1" + struct.pack(">H", 1) + u64(chain_id) + u64(timestamp)
            + text(name) + text(branch) + text(description) + tree + content + bytes([1, 1]))


def decode(data):
    offset = 0

    def take(size):
        nonlocal offset
        if size > len(data) - offset:
            raise ValueError("short descriptor")
        result = data[offset:offset + size]
        offset += size
        return result

    if take(4) != b"OGD1" or take(2) != b"\x00\x01":
        raise ValueError("unsupported descriptor")
    chain_id, timestamp = struct.unpack(">QQ", take(16))
    for expected in (name, branch):
        length = int.from_bytes(take(2), "big")
        if length != len(expected) or take(length) != expected:
            raise ValueError("unexpected managed name/branch")
    length = int.from_bytes(take(2), "big")
    if length > 1024:
        raise ValueError("oversized description")
    description = take(length)
    description.decode("utf-8")
    if b"\r" in description or b"\0" in description:
        raise ValueError("invalid description")
    initial_tree, content_hash = take(20), take(32)
    if initial_tree == bytes(20) or content_hash == bytes(32):
        raise ValueError("missing content commitments")
    if take(2) != bytes([1, 1]) or offset != len(data):
        raise ValueError("policy/source/trailing bytes")
    return chain_id, timestamp, description, initial_tree, content_hash


descriptor = encode(2026, 1700000000, description)
assert decode(descriptor) == (2026, 1700000000, description, tree, content)
descriptor_hash = sha(descriptor)
for invalid in (descriptor[:-1], descriptor + b"\0", b"BAD!" + descriptor[4:],
                descriptor[:5] + b"\x02" + descriptor[6:], descriptor[:-1] + b"\x02"):
    try:
        decode(invalid)
    except ValueError:
        pass
    else:
        raise AssertionError("accepted invalid descriptor")
assert sha(encode(2027, 1700000000, description)) != descriptor_hash
assert sha(encode(2026, 1700000001, description)) != descriptor_hash
assert sha(encode(2026, 1700000000, description + b"!")) != descriptor_hash

base = Path(__file__).resolve().parent
doc = (base / "vectors/identity.json").read_bytes()
rid = hashlib.sha1(b"blob " + str(len(doc)).encode() + b"\0" + doc).digest()
log = (base.parent / "evidence/outbe-chain-pjz.1/design-check.log").read_text().splitlines()
owner = bytes.fromhex(next(line.split("=", 1)[1] for line in log if line.startswith("public_key=")))
assert len(owner) == 96


def bootstrap_body(genesis_hash):
    # Full OPB1 framing fixture; players/material/manifest commitments are dummy
    # public fixtures, not a claimed authenticated DKG or accepted checkpoint.
    return (b"OPB1" + u64(2026) + genesis_hash + rid + descriptor_hash + owner
            + bytes([10]) * 32 + u64(0) + u64(1) + bytes([9]) * 32
            + u64(0) + genesis_hash + u64(1) + bytes(32) + bytes([0])
            + bytes(32) + bytes(20) + bytes(20) + bytes(20) + bytes([11]) * 32)


# Actual genesis hashes enter only AFTER seeding; there is no genesis-dependent
# argument in encode(). A hash from another genesis cannot reuse this receipt.
first, second = bootstrap_body(bytes([7]) * 32), bootstrap_body(bytes([8]) * 32)
assert len(first) == 477
assert first != second and sha(first) != sha(second)
assert descriptor_hash == sha(descriptor)
assert first[44:64] == rid and first[64:96] == descriptor_hash
print("descriptor_preimage=" + descriptor.hex())
print("descriptor_hash=" + descriptor_hash.hex())
print("initial_tree_oid=" + tree.hex())
print("initial_content_manifest_sha256=" + content.hex())
print("bootstrap_body_genesis7=" + first.hex())
print("bootstrap_receipt_hash_genesis7=" + sha(first).hex())
print("bootstrap_receipt_hash_genesis8=" + sha(second).hex())
print("PASS descriptor: known pre-genesis inputs only, exact OGD1 roundtrip/negative vectors; full OPB1 framing binds actual later genesis/RID/key")
print("VECTOR ONLY: no production genesis hashing, DKG ceremony, publication signature or finality is claimed")
