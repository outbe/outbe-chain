#!/usr/bin/env python3
"""Verify complete source packets and reject missing/reordered/rebound parts."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

HERE = Path(__file__).resolve().parents[1]

def main():
    p = argparse.ArgumentParser()
    p.add_argument("--source", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    a = p.parse_args()
    results = []
    with tempfile.TemporaryDirectory(prefix="ristretto-source-packets-") as d:
        for case in ("valid", "missing_part", "trailing_byte", "reordered_parts", "changed_digest", "changed_binding", "changed_owner"):
            target = Path(d) / case
            target.mkdir()
            for name in ("offer.public.json", "opening.public.json", "p_link.bin"):
                shutil.copyfile(a.source / name, target / name)
            proof = target / "p_link.bin"
            raw = proof.read_bytes()
            if case == "missing_part": proof.write_bytes(raw[:-128])
            if case == "trailing_byte": proof.write_bytes(raw + b"\0")
            if case == "reordered_parts": proof.write_bytes(raw[128:256] + raw[:128] + raw[256:])
            if case in ("changed_binding", "changed_owner"):
                path = target / "offer.public.json"
                data = json.loads(path.read_text())
                field = "opening_binding" if case == "changed_binding" else "derived_owner"
                data[field] = f'{(int(data[field], 16) ^ 1):064x}'
                path.write_text(json.dumps(data))
            if case == "changed_digest":
                path = target / "opening.public.json"
                data = json.loads(path.read_text())
                data["digests"][6] = f'{(int(data["digests"][6], 16) ^ 1):064x}'
                path.write_text(json.dumps(data))
            r = subprocess.run([str(HERE / "target/release/outbe-ristretto-lifecycle-poc"),
                                "verify", str(HERE / "parameters/native32"), str(target)],
                               capture_output=True, text=True)
            accepted = r.returncode == 0
            if accepted != (case == "valid"):
                raise AssertionError(f"unexpected result {case}: {r.stderr[-300:]}")
            results.append({"case": case, "accepted": accepted, "expected": True})
    a.out.parent.mkdir(parents=True, exist_ok=True)
    a.out.write_text(json.dumps({"passed": True, "cases": results}, indent=2) + "\n")
    print(json.dumps({"passed": True, "cases": len(results)}))

if __name__ == "__main__":
    main()
