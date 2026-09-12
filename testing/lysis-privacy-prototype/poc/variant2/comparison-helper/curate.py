#!/usr/bin/env python3
"""Copy an allowlist of public evidence and account exact wire prefixes.

Never copies a private actor directory, witness, key, or decrypted balance.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import struct

BASE = Path(__file__).resolve().parents[2]
R = BASE / "variant2/ristretto"
REPO = BASE.parents[2]

def read(p): return json.loads(Path(p).read_text())
def write(p, v):
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(json.dumps(v, indent=2, sort_keys=True) + "\n")
def string(s):
    b = s.encode()
    return struct.pack("<Q", len(b)) + b
def vec(items):
    return struct.pack("<Q", len(items)) + b"".join(items)
def public_wire(v):
    return (string(v["kind"]) + string(v["context"]) +
            vec([vec([string(x) for x in row]) for row in v["notes"]]) +
            b"".join(string(v[k]) for k in ("source", "fraction", "price", "amount")))
def cipher_wire(v):
    return bytes(v["key"]) + vec([bytes(x) for x in v["c"]]) + vec([bytes(x) for x in v["d"]])

def main():
    p = argparse.ArgumentParser()
    p.add_argument("--run", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    a = p.parse_args()
    run, out = a.run.resolve(), a.out.resolve()
    pub = run / "public"
    report = read(pub / "result.json")
    if not report["experimental_lifecycle_executed"]: raise ValueError("incomplete run")
    out.mkdir(parents=True)
    snapshot = read(pub / "source-snapshot.json")
    if any(hashlib.sha256((REPO / k).read_bytes()).hexdigest() != h for k,h in snapshot.items()):
        raise ValueError("runtime changed since completed run")
    copied = []
    def copy(src, dst):
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src, dst)
        copied.append(str(dst.relative_to(out)))
    for name in ("result.json", "timings.json", "source-snapshot.json", "aggregate.json",
                 "core-cycle.json", "cross-owner.json", "key-only-recovery.json", "oracle.json",
                 "pledge.json", "worker.json", "l2-verify.json", "link-generate.json", "link-verify.json"):
        copy(pub / name, out / name)
    for q in sorted((pub / "metrics").glob("*.json")):
        copy(q, out / "metrics" / q.name)
    for q in sorted((pub / "paired-baseline").rglob("*.json")):
        copy(q, out / q.relative_to(pub))
    operations = []
    for op in sorted(pub.glob("operation-*")):
        if not (op / "twisted.bin").exists(): continue
        s = read(op / "statement.public.json")
        cs = read(op / "ciphertexts.json")
        prefix = public_wire(s) + vec([cipher_wire(c) for c in cs])
        raw = (op / "twisted.bin").read_bytes()
        if not raw.startswith(prefix): raise ValueError("wire accounting differs from actual Bundle prefix")
        operations.append({"operation": op.name.removeprefix("operation-"),
                           "bundle_bytes": len(raw), "statement_wire_bytes": len(public_wire(s)),
                           "primary_ciphertexts_wire_bytes": len(prefix)-len(public_wire(s)),
                           "proofs_and_auxiliary_objects_wire_bytes": len(raw)-len(prefix),
                           "baseline_equivalent_statement_plus_proof_bytes": len(public_wire(s))+8+128,
                           "note_commitments_raw_bytes": len(s["notes"])*128})
        for name in ("statement.public.json", "twisted-resources.json", "twisted-verify.json"):
            copy(op / name, out / op.name / name)
    # Public samples sufficient for a separate verifier process.
    for name in ("offer.public.json", "opening.public.json", "p_link.bin"):
        copy(pub / "tribute-1/link" / name, out / "source-sample" / name)
    copy(pub / "tribute-1/l2/p_l2.bin", out / "source-sample/p_l2.bin")
    for name in ("twisted.bin", "statement.public.json", "registry-before.json", "ciphertexts.json"):
        copy(pub / "operation-withdraw" / name, out / "withdraw-sample" / name)
    key = bytes(read(pub / "operation-withdraw/ciphertexts.json")[0]["key"]).hex()
    write(out / "withdraw-sample/key.public.json", key)
    parameter_files = []
    for q in sorted((R / "parameters/native32").rglob("*.bin")):
        parameter_files.append({"path":str(q.relative_to(R)),"bytes":q.stat().st_size,
                                "sha256":hashlib.sha256(q.read_bytes()).hexdigest()})
        if q.name == "vk.bin": copy(q, out / "source-parameters" / q.parent.name / q.name)
    write(out / "parameters.json", {"files":parameter_files,
          "pk_disk_bytes":sum(x["bytes"] for x in parameter_files if x["path"].endswith("pk.bin")),
          "vk_disk_bytes":sum(x["bytes"] for x in parameter_files if x["path"].endswith("vk.bin")),
          "setup":"single-party test ceremony; 13 distinct configured VKs"})
    write(out / "storage.json", {"operations":operations,
          "cipher_raw_bytes":1024,"cipher_with_key_and_lengths_bytes":1072,
          "note_raw_bytes":128,"source_proof_bytes":1664,
          "source_opening_digest_raw_bytes":13*32,"source_opening_binding_raw_bytes":32,
          "cipher_registry_json_bytes":(pub/"cipher-registry.json").stat().st_size,
          "cipher_registry_entries":len(read(pub/"cipher-registry.json")),
          "private_storage_report":report["private_storage"],
          "note":"Proof/auxiliary bytes include proof framing, fresh commitments and positivity auxiliary ciphertexts; baseline tuple size is an equivalent codec, not deployed wire."})
    old = read(R / "preserved-source-snapshot.json")
    if any(hashlib.sha256((REPO/k).read_bytes()).hexdigest()!=h for k,h in old.items()):
        raise ValueError("baseline/hybrid implementation changed")
    write(out / "verification.json", {"completed":True,"runtime_files_match":len(snapshot),
          "preserved_baseline_and_hybrid_source_files":len(old),"public_allowlist_only":True,
          "full_production_protocol_pass":False})
    manifest = {str(q.relative_to(out)):hashlib.sha256(q.read_bytes()).hexdigest()
                for q in sorted(out.rglob("*")) if q.is_file() and q.name!="MANIFEST.json"}
    write(out / "MANIFEST.json", manifest)
    print(json.dumps({"evidence_files":len(manifest),"out":str(out)}))

if __name__ == "__main__": main()
