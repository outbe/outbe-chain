"""Independently reconstruct finite P0 vector hashes/framing with Python stdlib.

The Rust check verifies BLS. This checks bytes, not production parsers or finality.
"""
import hashlib
import json
from pathlib import Path

root = Path(__file__).resolve().parents[3]
base = root / "plans/radicle-patch-vote"
lines = (base / "evidence/outbe-chain-pjz.1/design-check.log").read_text().splitlines()
single = {}
vectors = []
for line in lines:
    if line.startswith("VECTOR kind="):
        vectors.append(dict(item.split("=", 1) for item in line[7:].split()))
    elif line.startswith("VECTOR ") and "=" in line:
        key, value = line[7:].split("=", 1)
        single[key] = value

sha = lambda b: hashlib.sha256(b).digest()
git = lambda kind, b: hashlib.sha1(kind.encode() + b" " + str(len(b)).encode() + b"\0" + b).digest()
doc = (base / "p0-check/vectors/identity.json").read_bytes()
assert single["doc_utf8"].encode() == doc
assert json.dumps(json.loads(doc), sort_keys=True, separators=(",", ":")).encode() == doc
namespace = (base / "p0-check/vectors/repository-key.txt").read_text()
alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
number = 0
assert namespace.startswith("z")
for char in namespace[1:]:
    number = number * 58 + alphabet.index(char)
decoded = number.to_bytes(98, "big")
public = decoded[2:]
assert decoded[:2] == bytes.fromhex("eb01") and len(public) == 96
assert json.loads(doc)["delegates"] == ["did:key:" + namespace]
assert bytes.fromhex(single["repository_key_v2"]) == b"\x01" + public
assert single["repository_namespace"] == namespace
assert bytes.fromhex(single["upgrade_control_v1"]) == bytes([3, 2])
assert bytes.fromhex(single["upgrade_ack_control_v1"]) == bytes([4, 2])
repo = git("blob", doc)
tree = bytes.fromhex(single["cob_tree_raw"])
tree_oid = git("tree", tree)
metadata = bytes.fromhex(single["cob_unsigned_commit"])
assert metadata.startswith(b"tree " + tree_oid.hex().encode() + b"\n")
assert b"\r" not in metadata and b"\0" not in metadata and metadata.endswith(b"\n")
refs = bytes.fromhex(single["refs_canonical"])
ref_lines = [line.split(b" ") for line in refs.splitlines()]
assert [name for _, name in ref_lines] == sorted(name for _, name in ref_lines)
assert ref_lines[0][0].decode() == single["cob_signed_commit_oid"]
assert [int(v["kind"]) for v in vectors] == [1, 2, 3]
for row in vectors:
    kind = int(row["kind"])
    fields = {key: bytes.fromhex(value) for key, value in row.items() if key != "kind"}
    payload, meta, context = fields["native"], fields["metadata_hash"], fields["context"]
    assert len(fields["job"]) == 238 and fields["job"][:4] == b"OPJ1"
    assert context == sha(fields["job"])
    expected = b"RDB1" + bytes([kind]) + repo + context + meta + len(payload).to_bytes(4, "big") + sha(payload)
    assert len(expected) == 125 and fields["message"] == expected
    domain = b"radicle.repository.bls.v1"
    assert fields["commonware_input"] == bytes([len(domain)]) + domain + expected
    assert fields["envelope"] == expected + public + fields["signature"]
    assert len(fields["envelope"]) == 269 and len(fields["signature"]) == 48
    if kind == 1:
        assert payload == repo and meta == bytes(32)
    elif kind == 2:
        assert payload == tree_oid and meta == sha(metadata)
        head, body = metadata.split(b"\n\n", 1)
        commit = head + b"\nradicle-bls-sig " + fields["envelope"].hex().encode() + b"\n\n" + body
        assert git("commit", commit).hex() == single["cob_signed_commit_oid"]
    else:
        assert payload == refs and meta == sha(bytes([0x44]) * 20)
print("PASS independent byte reconstruction: DID/key wire, canonical fixture doc, actual Git OIDs, all three OPJ1/RDB1/envelope/Commonware frames, COB metadata and signature header")
