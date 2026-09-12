#!/usr/bin/env python3
"""Prepare the exact cached Bulletproofs 5.0.0 copy; never edit the registry copy."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil

base = Path(__file__).resolve().parent
parser = argparse.ArgumentParser()
parser.add_argument("--source", type=Path, help="Explicit extracted bulletproofs-5.0.0 source")
args = parser.parse_args()
cargo_home = Path(os.environ.get("CARGO_HOME", str(Path.home() / ".cargo")))
candidates = [args.source] if args.source else sorted((cargo_home / "registry/src").glob("*/bulletproofs-5.0.0"))
if not candidates:
    raise SystemExit("Cache missing: supply --source /path/to/extracted/bulletproofs-5.0.0")
source = candidates[0].resolve()
dest = base / "vendor/bulletproofs"
original = (source / "src/r1cs/proof.rs").read_text()
old = "Scalar::from_canonical_bytes(read32!()).ok_or(R1CSError::FormatError)?"
new = "Option::<Scalar>::from(Scalar::from_canonical_bytes(read32!())).ok_or(R1CSError::FormatError)?"
assert original.count(old) == 3, "Source differs from the expected 5.0.0 decoder"
patched = original.replace(old, new)
if dest.exists():
    assert (dest / "src/r1cs/proof.rs").read_text() == patched, "Existing vendor differs; inspect manually"
else:
    shutil.copytree(source, dest)
    (dest / "src/r1cs/proof.rs").write_text(patched)
checksum_file = source / ".cargo-checksum.json"
checksum = json.loads(checksum_file.read_text()).get("package") if checksum_file.exists() else None
print(json.dumps({"version": "5.0.0", "package_checksum": checksum,
                  "original_decoder_sha256": hashlib.sha256(original.encode()).hexdigest(),
                  "patched_decoder_sha256": hashlib.sha256(patched.encode()).hexdigest(),
                  "compatibility_edits": 3}, indent=2))

