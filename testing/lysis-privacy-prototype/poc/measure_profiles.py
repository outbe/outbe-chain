#!/usr/bin/env python3
"""Cold SU growth and independent host prover processes; no network TPS claim."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time
from run_lifecycle import HERE, REPO, BIN, L2, LIMITER, WALLET_LIMIT, write, read

def measured(command, out, limit=WALLET_LIMIT, env=None):
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.with_suffix(".driver.log").open("w") as log:
        code=subprocess.call([sys.executable,str(LIMITER),"--limit",str(limit),"--json",str(out),"--",*map(str,command)],cwd=REPO,env=env,stdout=log,stderr=subprocess.STDOUT)
    result=read(out)
    if code and result["within_ram_limit"]:raise RuntimeError("inconsistent child status")
    return result

def growth(out):
    rows=[]
    env=dict(os.environ,BB_CRS_PATH=str(Path.home()/".bb-crs/bn254_g1.dat"))
    for su in (1,16,32,64,128,256):
        print(f"SU growth: {su}",flush=True)
        directory=out/f"su-{su}";directory.mkdir()
        parameters=HERE/f"parameters/native{su}"
        if not (parameters/"pk.bin").exists():
            setup=measured([BIN/"outbe-private-lifecycle-poc","setup",parameters,su],directory/"setup-rss.json",2_000_000_000,env)
            if not setup["within_ram_limit"]:raise RuntimeError("server setup failed")
        wallet=directory/"wallet"
        subprocess.run([str(BIN/"outbe-private-lifecycle-poc"),"wallet",str(wallet),str(su),"900"],check=True,stdout=subprocess.DEVNULL)
        l2=measured([L2,wallet/"wallet.private.json",directory/"l2"],directory/"l2-rss.json",2_000_000_000,env)
        if not l2["within_ram_limit"]:raise RuntimeError("source L2 proof failed")
        proof=measured([BIN/"outbe-private-lifecycle-poc","prove",parameters,wallet/"wallet.private.json",directory/"link"],directory/"link-rss.json",env=env)
        row={"su":su,"parameters":read(parameters/"setup.json"),"cold":proof,"source_l2":l2}
        if proof["within_ram_limit"]:
            row["proof"]=read(directory/"link/prove.json")
            subprocess.run([str(BIN/"outbe-private-lifecycle-poc"),"verify",str(parameters),str(directory/"link"),"10"],check=True,stdout=subprocess.DEVNULL)
            row["verify"]=read(directory/"link/verify.json")
        rows.append(row)
        write(out/"growth.json",{"protocol_su_maximum":None,"measured_profiles":rows,"stop_at_first_cold_wallet_failure":True})
        if not proof["within_ram_limit"]:
            if proof["peak_rss_bytes"]<=WALLET_LIMIT:raise RuntimeError("proof failed for a reason other than memory")
            break

def parallel(out, lifecycle):
    source=read(lifecycle/"control/link-manifest.json")["rows"][:32]
    if len(source)!=32:raise ValueError("needs 32 distinct admitted wallets")
    parameters=HERE/"parameters/native32"
    env=dict(os.environ,RAYON_NUM_THREADS="2")
    results=[]
    for concurrency in (1,2):
        directory=out/f"processes-{concurrency}";directory.mkdir()
        children=[];logs=[];start=time.perf_counter()
        for index in range(concurrency):
            rows=[{"wallet":r["wallet"],"out":str(directory/f"proof-{k}")} for k,r in enumerate(source) if k%concurrency==index]
            manifest=write(directory/f"manifest-{index}.json",{"rows":rows,"report":str(directory/f"batch-{index}.json"),"verify_report":str(directory/f"verify-{index}.json")})
            log=(directory/f"child-{index}.log").open("w");logs.append(log)
            command=[sys.executable,str(LIMITER),"--limit",str(WALLET_LIMIT),"--json",str(directory/f"rss-{index}.json"),"--",str(BIN/"outbe-private-lifecycle-poc"),"batch-prove",str(parameters),manifest]
            children.append(subprocess.Popen(command,cwd=REPO,env=env,stdout=log,stderr=subprocess.STDOUT))
        codes=[p.wait() for p in children]
        for log in logs:log.close()
        if any(codes):raise RuntimeError("parallel proof child failed")
        elapsed=time.perf_counter()-start
        batches=[read(directory/f"batch-{i}.json") for i in range(concurrency)]
        peaks=[read(directory/f"rss-{i}.json") for i in range(concurrency)]
        results.append({"processes":concurrency,"rayon_threads_each":2,"distinct_proofs":len(source),"wall_seconds_including_each_parameter_load":elapsed,"proofs_per_second_with_load":len(source)/elapsed,"batch_metrics":batches,"memory":peaks,"sum_of_process_peak_rss_upper_bound":sum(p["peak_rss_bytes"] for p in peaks)})
        print(f"{concurrency} processes: {elapsed:.3f}s",flush=True)
    write(out/"parallel.json",{"scope":"32 distinct real P_link proofs, same inputs in each comparison; independent cold processes then warm batches; generation self-verifies each proof; no admission or consensus TPS","runs":results})

def main():
    p=argparse.ArgumentParser();p.add_argument("mode",choices=("growth","parallel"));p.add_argument("--out",required=True,type=Path);p.add_argument("--lifecycle",type=Path)
    args=p.parse_args();out=args.out.resolve();out.mkdir(parents=True,exist_ok=False)
    if args.mode=="growth":growth(out)
    else:parallel(out,args.lifecycle.resolve())

if __name__=="__main__":main()
