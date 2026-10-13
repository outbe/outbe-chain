#!/usr/bin/env python3
"""Regenerate CES2 vectors with Noir beta.22, never with the Rust implementation."""
import hashlib
import functools
import json
from pathlib import Path
import re
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
PIN = "1.0.0-beta.22+c57152f91260ecdb9faad4efc20abb14b6d2ece7"
nargo_version = subprocess.check_output(["nargo", "--version"], text=True)
if PIN not in nargo_version:
    raise RuntimeError(f"Expected nargo version containing {PIN}, got: {nargo_version.strip()}")
BASE = 0x4345533200000000
NAMES = ["BYTES_INIT", "BYTES_ABSORB", "BYTES_FINAL", "ID", "KEY", "BODY", "LEAF",
         "SMT_BASE", "SMT_NORMAL", "SMT_ZERO", "TOP_NODE", "SEALED_ROOT",
         "COLLECTION_KEY", "COLLECTION_ROOT"]
TAGS = {"TAG_" + name: BASE + i + 1 for i, name in enumerate(NAMES)}
transcripts = []


def word(value):
    return value.to_bytes(32, "big").hex()


@functools.cache
def sponge(inputs):
    assert len(inputs) <= 8
    prover = 'inputs = [' + ', '.join('"' + str(x) + '"' for x in inputs + (0,) * (8-len(inputs))) + ']\n'
    prover += f'len = "{len(inputs)}"\n'
    (REFERENCE / "Prover.toml").write_text(prover)
    result = subprocess.check_output(["nargo", "execute", "--program-dir", str(REFERENCE)], text=True)
    output = re.search(r'Circuit output: (0x[0-9a-f]+)', result)
    assert output, result
    value = int(output[1], 16)
    transcripts.append({"inputs": [word(x) for x in inputs], "output": word(value)})
    return value


def tagged(name, *inputs):
    return sponge((TAGS[name], *inputs))


def pbytes(name, data):
    chunks = [data[i:i+31] for i in range(0, len(data), 31)]
    tag = TAGS[name]
    state = tagged("TAG_BYTES_INIT", tag, len(data), len(chunks))
    for i, chunk in enumerate(chunks):
        state = tagged("TAG_BYTES_ABSORB", tag, state, i, int.from_bytes(chunk.ljust(31, b'\0'), 'big'))
    return tagged("TAG_BYTES_FINAL", tag, len(data), len(chunks), state)


def leaf(identity, payload, schema=1):
    return tagged("TAG_LEAF", 2, schema, pbytes("TAG_ID", identity), len(payload), pbytes("TAG_BODY", payload))


def varint(value):
    out = bytearray()
    while value >= 128:
        out.append((value & 127) | 128)
        value >>= 7
    out.append(value)
    return bytes(out)


def bfield(n, value):
    return varint(n * 8 + 2) + varint(len(value)) + value


def vfield(n, value):
    return varint(n * 8) + varint(value) if value else b''


def amount(n, value):
    return bfield(n, value.to_bytes(32, 'big'))


def generate():
    vectors = {"implementation": "Noir Poseidon2 permutation reference", "implementation_version": PIN,
               "parameter_source": "noir-reference/src/main.nr: length capacity, rate 3, final permutation",
               "bn254_scalar_modulus": "30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001"}
    vectors['tags'] = [{"name": name, "decimal": str(tag), "hex": '0x'+word(tag),
                        "representative_inputs": ["1", "2"], "output": word(tagged(name, 1, 2))}
                       for name, tag in TAGS.items()]
    vectors['pbytes'] = [{"object_tag": "TAG_BODY", "length": n, "input_hex": bytes(range(n)).hex(),
                          "output": word(pbytes("TAG_BODY", bytes(range(n))))}
                         for n in [0, 1, 30, 31, 32, 61, 62, 63]]
    identities = []
    for kind, owner, day in [('tribute', bytes([0x42])*20, 0x01020304),
                             ('nod_item', bytes([0x24])*20, 0x0a0b0c0d),
                             ('nod_bucket', None, 0xffffffff)]:
        digest = sponge((int.from_bytes(owner, 'big'), day)) if owner else int.from_bytes(bytes([0x33])*32, 'big')
        identity = day.to_bytes(4,'big') + bytes.fromhex(word(digest))[4:]
        entry = {"kind": kind, "worldwide_day": day, "identity_hex": identity.hex(),
                 "identity_field": word(pbytes("TAG_ID", identity))}
        if owner:
            entry['owner_hex'] = owner.hex()
            entry['digest_hex'] = word(digest)
        else:
            entry['bucket_key_hex'] = word(digest)
        identities.append(entry)
    vectors['identities'] = identities
    tribute, nod, bucket = [bytes.fromhex(x['identity_hex']) for x in identities]
    tribute_payload = (bfield(1, tribute) + bfield(2, bytes([0x42])*20) + vfield(3, 0x01020304)
                       + amount(4,1) + vfield(5,840) + amount(6,2) + vfield(7,978) + amount(8,3) + vfield(9,1))
    nod_payload = (bfield(1,nod) + bfield(2,bytes([0x24])*20) + amount(3,1) + vfield(4,0x0a0b0c0d)
                   + vfield(5,2) + bfield(7,bytes([0x33])*32)
                   + vfield(9,840) + vfield(10,978) + vfield(11,5))
    bucket_payload = bfield(1,bytes([0x33])*32) + vfield(2,0xffffffff) + amount(6,1) + vfield(7,840)
    vectors['bodies'] = [{"kind": entry['kind'], "identity_hex": entry['identity_hex'], "schema_version":1,
                          "payload_hex": payload.hex(), "stored_body_hex": (vfield(1,1)+bfield(2,payload)).hex(),
                          "leaf": word(leaf(bytes.fromhex(entry['identity_hex']),payload))}
                         for entry,payload in zip(identities,[tribute_payload,nod_payload,bucket_payload])]
    # Immutable encryption blobs use epoch BE8 followed by ciphertext and AEAD tag.
    public_key_blob = (1).to_bytes(8,'big') + bytes([0x55])*48
    amounts_blob = (1).to_bytes(8,'big') + bytes([0x66])*80
    encrypted_tribute = (bfield(1,tribute) + bfield(2,bytes([0x42])*20) + vfield(3,0x01020304)
                         + bfield(4,amounts_blob) + vfield(5,840) + bfield(6,public_key_blob)
                         + vfield(7,978) + amount(8,3) + vfield(9,1) + vfield(10,99)
                         + bfield(11,bytes([0x77])*32))
    encrypted_nod = (bfield(1,nod) + bfield(2,bytes([0x24])*20) + bfield(3,public_key_blob)
                     + bfield(4,public_key_blob) + vfield(5,99) + vfield(6,0x0a0b0c0d)
                     + vfield(7,2) + amount(8,3) + vfield(9,840) + vfield(10,978)
                     + bfield(11,bytes([0x33])*32) + vfield(12,5) + bfield(14,bytes([0x77])*32))
    for kind,identity,payload in [('tribute_v2',tribute,encrypted_tribute),('nod_item_v2',nod,encrypted_nod)]:
        vectors['bodies'].append({"kind":kind,"identity_hex":identity.hex(),"schema_version":2,
            "payload_hex":payload.hex(),"stored_body_hex":(vfield(1,2)+bfield(2,payload)).hex(),
            "leaf":word(leaf(identity,payload,2))})
    vectors['schema_variation'] = {"identity_hex": tribute.hex(), "payload_hex": tribute_payload.hex(),
        "scheme_2_schema_1_leaf":word(leaf(tribute,tribute_payload)),
        "scheme_2_schema_2_leaf":word(leaf(tribute,tribute_payload,2)), "active_schema_version":1,
        "alternate_schema_version":2, "rejected_schema_version":3}
    flipped_id = tribute[:-1] + bytes([tribute[-1]^1])
    flipped_payload = tribute_payload[:-1] + bytes([tribute_payload[-1]^1])
    vectors['bit_flips'] = {"identity":{"original_hex":tribute.hex(),"flipped_hex":flipped_id.hex(),
        "original_leaf":word(leaf(tribute,tribute_payload)), "flipped_leaf":word(leaf(flipped_id,tribute_payload))},
        "payload":{"original_hex":tribute_payload.hex(),"flipped_hex":flipped_payload.hex(),
        "original_leaf":word(leaf(tribute,tribute_payload)), "flipped_leaf":word(leaf(tribute,flipped_payload))}}
    vectors['rejection_artifacts'] = {"zero_present_leaf":word(0),
        "field_element_at_modulus":vectors['bn254_scalar_modulus'], "unsupported_commitment_scheme_version":1,
        "unsupported_schema_version":3,"empty_payload_hex":""}
    vectors['rejection_artifacts']['protobuf'] = [
        {"kind":"unknown_field","payload_hex":(tribute_payload+vfield(10,1)).hex()},
        {"kind":"duplicate_field","payload_hex":(tribute_payload+bfield(1,tribute)).hex()},
        {"kind":"non_minimal_key","payload_hex":(b'\x8a\x00'+tribute_payload[1:]).hex()},
        {"kind":"explicit_default","payload_hex":tribute_payload.replace(vfield(3,0x01020304),b'\x18\x00').hex()},
        {"kind":"empty_stored_payload","stored_body_hex":"08011200"},
        {"kind":"zero_stored_schema","stored_body_hex":"0800120101"},
    ]
    base = tagged('TAG_SMT_BASE',3,5,7)
    compact_base = tagged('TAG_SMT_BASE',4,5,7)
    # CKB bit 4 is bit 4 of byte zero, hence BE numeric bit 252.
    zero_bits = 16 << 248
    vectors['smt_operations'] = {
        'base':word(base), 'normal':word(tagged('TAG_SMT_NORMAL',4,5,7,11)),
        'compact':word(tagged('TAG_SMT_ZERO',compact_base,zero_bits,1)),
        'wrapped':word(tagged('TAG_SMT_ZERO',1,0,0)),
    }
    single_leaf_base = tagged('TAG_SMT_BASE',0,1,7)
    vectors['smt_proofs'] = [
        {"key":word(1), "leaf":word(7), "root":word(tagged('TAG_SMT_ZERO',single_leaf_base,1,0)), "proof_hex":"4c4f00"},
        {"key":word(1), "leaf":word(0), "root":word(0), "proof_hex":"4c4f00"},
    ]
    shards = json.loads((HERE / 'adr009-sharding.json').read_text())
    fixed_id = bytes.fromhex(shards['identity_hex'])
    collection_vectors = []
    for domain, entry in enumerate(shards['real'],1):
        collection_input = domain.to_bytes(2,'big') + (b'\1'+(4).to_bytes(4,'big')+fixed_id[:4] if domain==1 else b'\0'+bytes(4))
        key = pbytes('TAG_COLLECTION_KEY',collection_input)
        tree_key = tagged('TAG_KEY',2,key,pbytes('TAG_ID',fixed_id))
        entry['tree_key_hex'] = word(tree_key)
        k = entry['k']
        entry['shard_index'] = tree_key & (k - 1)
        roots = list(range(k))
        for level in range(k.bit_length() - 1):
            roots = [tagged('TAG_TOP_NODE',level,roots[i],roots[i+1]) for i in range(0,len(roots),2)]
        root = tagged('TAG_COLLECTION_ROOT',2,key,k,roots[0])
        collection_vectors.append({"domain":domain,"collection_key":word(key),"tree_key":word(tree_key),
                                   "shard_top_root":word(roots[0]),"collection_root":word(root)})
    vectors['collections'] = collection_vectors
    vectors['sealed_empty_root'] = word(tagged('TAG_SEALED_ROOT',2,0))
    for n in range(9):
        sponge(tuple(range(1,n+1)))
    # Tagged inputs across rate boundaries (total sponge lengths 3 and 6).
    for n in [2,3,5,6]:
        tagged('TAG_TOP_NODE',*range(1,n+1))
    catalog = json.loads((HERE/'adr010-catalog.json').read_text())
    for entry,name in zip(collection_vectors,['tribute','nod_item','nod_bucket','gem','gem_bucket']):
        catalog[name+'_collection_key'] = entry['collection_key']
        catalog[name+'_tree_key'] = entry['tree_key']
    catalog['tribute_tree_key'] = collection_vectors[0]['tree_key']
    empty = [0]*16
    for level in range(4):
        empty = [tagged('TAG_TOP_NODE',level,empty[i],empty[i+1]) for i in range(0,len(empty),2)]
    catalog['empty_shard_top'] = '0x'+word(empty[0])
    catalog['empty_tribute_collection_root'] = '0x'+word(tagged('TAG_COLLECTION_ROOT',2,int(catalog['tribute_collection_key'],16),16,empty[0]))
    empty_256 = [0]*256
    for level in range(8):
        empty_256 = [tagged('TAG_TOP_NODE',level,empty_256[i],empty_256[i+1]) for i in range(0,len(empty_256),2)]
    catalog['empty_shard_top_256'] = '0x'+word(empty_256[0])
    for domain,name in enumerate(['tribute','nod_item','nod_bucket','gem','gem_bucket'],1):
        k = 16 if domain == 1 else 256
        top = empty[0] if domain == 1 else empty_256[0]
        catalog['empty_'+name+'_collection_root'] = '0x'+word(tagged('TAG_COLLECTION_ROOT',2,int(catalog[name+'_collection_key'],16),k,top))
    catalog['empty_sealed_root'] = '0x'+vectors['sealed_empty_root']
    (HERE/'adr010-catalog.json').write_text(json.dumps(catalog,indent=2)+'\n')
    # Identity ordering affects the Lysis corpus; keep its independent economic reference.
    repo = HERE.parents[3]
    cases_path = repo/'crates/core/lysis/vectors/lysis-v1/cases.jsonl'
    cases = [json.loads(line) for line in cases_path.read_text().splitlines()]
    for case in cases:
        for tribute in case['input']['tributes']:
            day = tribute['worldwide_day']
            digest = sponge((int(tribute['owner'],16),day))
            tribute['tribute_id'] = day.to_bytes(4,'big').hex() + word(digest)[8:]
    cases_path.write_text(''.join(json.dumps(case,sort_keys=True,separators=(',',':'))+'\n' for case in cases))
    subprocess.run(['cargo','run','--locked','--offline','-p','outbe-lysis-v1-reference','--','--mode','write'],cwd=repo,check=True)
    manifest_path = cases_path.with_name('manifest.json')
    manifest = json.loads(manifest_path.read_text())
    manifest['case_file_sha256'] = hashlib.sha256(cases_path.read_bytes()).hexdigest()
    manifest_path.write_text(json.dumps(manifest,indent=2)+'\n')
    vectors['sponge'] = transcripts
    (HERE / 'ces2-noir-poseidon2.json').write_text(json.dumps(vectors,indent=2)+'\n')
    (HERE / 'adr009-sharding.json').write_text(json.dumps(shards,indent=2)+'\n')


with tempfile.TemporaryDirectory(prefix='ces2-noir-') as directory:
    REFERENCE = Path(directory)
    (REFERENCE/'src').mkdir()
    for path in ['Nargo.toml','src/main.nr']:
        (REFERENCE/path).write_text((HERE/'noir-reference'/path).read_text())
    generate()
print(f'Generated {len(transcripts)} independently evaluated Noir sponge vectors.')
