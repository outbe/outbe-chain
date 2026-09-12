#!/usr/bin/env python3
"""Cold paired comparison; private numeric witnesses stay in wallet processes."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys

HERE = Path(__file__).resolve().parent
BASE = HERE.parents[1]
REPO = BASE.parents[2]
BIN = BASE / "target/release"
LIMITER = BASE.parent / "measurements/run_with_ram_limit.py"
LIMIT = 512_000_000

def read(p):
    if ".private." in str(p):
        raise ValueError("controller cannot read private numeric witnesses")
    return json.loads(Path(p).read_text())

def write(p, value):
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n")
    return str(p)

def metric(run, stage):
    hits = list((run / "public/metrics").glob(f"*-{stage}.json"))
    if len(hits) != 1:
        raise ValueError(f"metric {stage}: {len(hits)}")
    return read(hits[0])

def main():
    p = argparse.ArgumentParser()
    p.add_argument("--run", type=Path, required=True)
    a = p.parse_args()
    run = a.run.resolve()
    result = read(run / "public/result.json")
    if not result["experimental_lifecycle_executed"]:
        raise ValueError("incomplete lifecycle")
    dest = run / "public/paired-baseline"
    control = run / "control/paired-baseline"
    private = run / "private/paired-baseline"
    for d in (dest, control, private):
        d.mkdir()
    rows = []
    for pub in sorted((run / "public").glob("operation-*")):
        if not (pub / "twisted-resources.json").exists():
            continue
        tag = pub.name.removeprefix("operation-")
        statement = read(pub / "statement.public.json")
        kind = statement["kind"]
        out = dest / tag
        out.mkdir()
        converted = private / f"{tag}.private.json"
        subprocess.run([str(BIN / "outbe-private-backend-comparison"), "transition",
                        str(run / f"private/wallet-operation-{tag}/transition.private.json"),
                        str(converted)], cwd=REPO, check=True, stdout=subprocess.DEVNULL)
        job = write(control / f"{tag}-prove.json", {
            "op": "prove", "parameters": str(BASE / f"parameters/{kind}-v2"),
            "witness": str(converted), "out": str(out)})
        with (control / f"{tag}.log").open("w") as log:
            subprocess.run([sys.executable, str(LIMITER), "--limit", str(LIMIT),
                            "--json", str(out / "cold.json"), "--", str(BIN / "poc-state"), job],
                           cwd=REPO, check=True, stdout=log, stderr=subprocess.STDOUT)
        verify = write(control / f"{tag}-verify.json", {
            "op": "verify", "parameters": str(BASE / f"parameters/{kind}-v2"),
            "statement": str(out / "statement.public.json"), "proof": str(out / "proof.bin"),
            "out": str(out / "verify.json")})
        subprocess.run([str(BIN / "poc-state"), verify], cwd=REPO, check=True)
        converted_public = read(out / "statement.public.json")
        for k in ("kind", "context", "fraction", "price", "amount"):
            if converted_public[k] != statement[k]:
                raise ValueError("adapter altered public context or numeric coefficient")
        old, oldres = read(out / "cold.json"), read(out / "resources.json")
        new, newres = metric(run, f"cold-twisted-{tag}"), read(pub / "twisted-resources.json")
        verified = read(pub / "twisted-verify.json")
        rows.append({"operation": tag, "kind": kind, "same_numeric_witness": True,
                     "curve_randomness_regenerated": True,
                     "baseline_cold_seconds": old["wall_seconds"],
                     "baseline_peak_rss_bytes": old["peak_rss_bytes"],
                     "baseline_proof_bytes": oldres["proof_bytes"],
                     "baseline_public_statement_bytes": (out / "statement.public.json").stat().st_size,
                     "baseline_node_prepared_verify_ms": read(out / "verify.json")["verify_ms"],
                     "ristretto_cold_seconds": new["wall_seconds"],
                     "ristretto_peak_rss_bytes": new["peak_rss_bytes"],
                     "ristretto_node_cold_verify_ms": verified["cold_verify_ms"],
                     "ristretto_node_prepared_verify_ms": verified["verify_ms"],
                     "ristretto_bundle_bytes": newres["bundle_bytes"],
                     "ristretto_resources": newres,
                     "ristretto_verify_components": verified["components"]})
    # Repeat the baseline source proof on the very same canonical L2 fields
    # and numeric source, with a fresh Baby-Jubjub opening.
    source = dest / "source"
    source.mkdir()
    converted = private / "source.private.json"
    subprocess.run([str(BIN / "outbe-private-backend-comparison"), "source",
                    str(run / "private/wallet-0/wallet.private.json"), str(converted)],
                   cwd=REPO, check=True, stdout=subprocess.DEVNULL)
    with (control / "source.log").open("w") as log:
        subprocess.run([sys.executable, str(LIMITER), "--limit", str(LIMIT),
                        "--json", str(source / "cold.json"), "--",
                        str(BIN / "outbe-private-lifecycle-poc"), "prove",
                        str(BASE / f'parameters/native{result["su_per_offer"]}'),
                        str(converted), str(source)], cwd=REPO, check=True,
                       stdout=log, stderr=subprocess.STDOUT)
        subprocess.run([str(BIN / "outbe-private-lifecycle-poc"), "verify",
                        str(BASE / f'parameters/native{result["su_per_offer"]}'), str(source)],
                       cwd=REPO, check=True, stdout=log, stderr=subprocess.STDOUT)
    write(dest / "comparison.json", {"profile": {
        "count": result["distinct_tributes"], "su": result["su_per_offer"],
        "wallet_limit_bytes": LIMIT,
        "comparison": "same numeric witnesses and context; fresh randomness for each curve; cold separate processes; prepared node verification excludes generator/VK preparation",
        "caveat": "single host observations; no phone or sustained TPS measurement"},
        "rows": rows,
        "source": {"same_numeric_witness":True,
                   "baseline_cold":read(source / "cold.json"),
                   "baseline_prove":read(source / "prove.json"),
                   "baseline_verify":read(source / "verify.json"),
                   "ristretto_cold":metric(run,f'cold-link-{result["su_per_offer"]}'),
                   "ristretto_prove":read(run / "public/cold-link/prove.json")},
        "source_snapshot_sha256": hashlib.sha256((run / "public/source-snapshot.json").read_bytes()).hexdigest()})
    print(json.dumps({"compared": len(rows), "report": str(dest / "comparison.json")}))

if __name__ == "__main__":
    main()
