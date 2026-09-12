#!/usr/bin/env python3
"""Paired baseline proving on the exact same private transitions as variant 2.

Controller never reads witnesses. Baseline prover alone reads each private file.
All published comparisons contain timings/sizes/public statements only.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys

V2=Path(__file__).resolve().parent
sys.path.insert(0,str(V2.parent))
from run_lifecycle import HERE,REPO,BIN,LIMITER,WALLET_LIMIT,read,write

def metric(run,stage):
    hits=list((run/"public/metrics").glob(f"*-{stage}.json"))
    if len(hits)!=1:raise ValueError(f"expected exactly one metric {stage}: {len(hits)}")
    return read(hits[0])

def main():
    p=argparse.ArgumentParser();p.add_argument("--run",type=Path,required=True);a=p.parse_args();run=a.run.resolve()
    report=read(run/"public/result.json")
    if not report["experimental_lifecycle_executed"]:raise ValueError("incomplete lifecycle")
    dest=run/"public/paired-comparison";dest.mkdir()
    controls=run/"control/paired-comparison";controls.mkdir()
    rows=[]
    for public in sorted((run/"public").glob("operation-*")):
        if not (public/"twisted-resources.json").exists():continue
        tag=public.name.removeprefix("operation-")
        statement=read(public/"statement.public.json");kind=statement["kind"]
        new=read(public/"twisted-resources.json");new_rss=metric(run,f"cold-twisted-{tag}")
        if kind in ("claim","mint","pledge"):
            # This exact Groth16 witness was already proved and measured by the
            # variant before the additional ciphertext layer. Do not remeasure.
            # There are two pledge calls; locate the actual job's output path.
            candidates=list((run/"public/metrics").glob(f"*-retained-cold-{kind}-proof.json"))
            baseline=None
            for q in candidates:
                m=read(q);job=read(Path(m["command"][-1]))
                if Path(job["out"])==public:baseline=m;break
            if baseline is None:raise ValueError("missing retained proof measurement")
            baseline_resources=read(public/"resources.json")
            extra=baseline["wall_seconds"]
        else:
            out=dest/tag;out.mkdir()
            job=write(controls/f"{tag}-prove.json",{"op":"prove","parameters":str(HERE/f"parameters/{kind}-v2"),"witness":str(run/f"private/wallet-operation-{tag}/transition.private.json"),"out":str(out)})
            measured=out/"cold.json"
            with (controls/f"{tag}.log").open("w") as log:
                subprocess.run([sys.executable,str(LIMITER),"--limit",str(WALLET_LIMIT),"--json",str(measured),"--",str(BIN/"poc-state"),job],cwd=REPO,stdout=log,stderr=subprocess.STDOUT,check=True)
            baseline=read(measured);baseline_resources=read(out/"resources.json")
            if not baseline["within_ram_limit"]:raise ValueError("baseline prover RAM gate failed")
            if read(out/"statement.public.json")!=statement:raise ValueError("not the exact same statement")
            verify=write(controls/f"{tag}-verify.json",{"op":"verify","parameters":str(HERE/f"parameters/{kind}-v2"),"statement":str(out/"statement.public.json"),"proof":str(out/"proof.bin"),"out":str(out/"verify.json")})
            subprocess.run([str(BIN/"poc-state"),verify],cwd=REPO,check=True)
            extra=0
        rows.append({"operation":tag,"kind":kind,"same_private_witness":True,"baseline_cold_seconds":baseline["wall_seconds"],"baseline_peak_rss_bytes":baseline["peak_rss_bytes"],"baseline_proof_bytes":baseline_resources["proof_bytes"],"variant_cold_seconds_including_retained_groth16":new_rss["wall_seconds"]+extra,"variant_peak_rss_bytes_sequential_processes":max(new_rss["peak_rss_bytes"],baseline["peak_rss_bytes"] if extra else 0),"variant_cipher_prover_cold_seconds":new_rss["wall_seconds"],"variant_cipher_prover_peak_rss_bytes":new_rss["peak_rss_bytes"],"variant_node_verify_ms":read(public/"twisted-verify.json")["verify_ms"],"variant_statement_cipher_proof_bundle_bytes":new["bundle_bytes"],"variant_money_proof_bytes":new["money_proof_bytes"],"variant_bridge_bytes":new["bridge_bytes"],"variant_bridges":new["bridge_count"],"retained_groth16":bool(extra)})
    output={"profile":{"count":report["distinct_tributes"],"su":report["su_per_offer"],"wallet_limit_bytes":WALLET_LIMIT,"comparison":"same witness/statement, separate cold processes; compile/setup excluded; claim/mint/pledge baseline is the already measured retained proof","timing_caveat":"single host observations; no significance, sustained TPS or phone claim"},"rows":rows,"history_stages":[x for x in read(run/"public/timings.json") if "framed_sent_bytes" in x],"source_snapshot_sha256":hashlib.sha256((run/"public/source-snapshot.json").read_bytes()).hexdigest()}
    write(dest/"comparison.json",output)
    print(json.dumps({"compared":len(rows),"report":str(dest/"comparison.json")}))
if __name__=="__main__":main()
