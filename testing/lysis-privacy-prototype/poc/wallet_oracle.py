#!/usr/bin/env python3
"""TEST ONLY: private correctness oracle, not a node or protocol aggregator.

It may read fixtures from several wallets. It emits booleans only. Protocol
execution has completed without sending these openings to public actors.
"""
import json
import os
from pathlib import Path
import subprocess
import sys

def load(path):return json.loads(Path(path).read_text())
def value(path):return int(load(path)["value"])
def main():
    j=load(sys.argv[1]);private=Path(j["private_dir"]);private.mkdir()
    if j.get("op")=="owner-query":
        q={"op":"fidelity","now":j["now"],"qualified":j["qualified"],"maximum":j["maximum"],"active":[{"value":str(value(r["path"])),"at":r["at"]} for r in j["active"]],"sold":[{"value":str(value(r["path"])),"at":r["at"],"sold_at":r["sold_at"]} for r in j["sold"]],"out":str(private/"fidelity.private.json")}
        qp=private/"query.private.json"
        fd=os.open(qp,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
        with os.fdopen(fd,"w") as out:json.dump(q,out)
        binary=Path(__file__).resolve().parent/"target/release/poc-math"
        subprocess.run([str(binary),str(qp)],check=True)
        assert load(q["out"])["valid"]
        os.chmod(q["out"],0o600)
        Path(j["out"]).write_text(json.dumps({"exact_rcfi_available_privately_to_owner":True,"current_rust_kernel":True,"active_slots":len(j["active"]),"sold_slots":len(j["sold"]),"source":"owner recovered witnesses bound to final commitments"}))
        return
    wallets=[load(p) for p in j["wallets"]];nominals=[int(w["nominal"]) for w in wallets]
    aggregate=load(j["aggregate"])["sums"]
    assert sum(nominals)==int(aggregate["S"])
    league=j["fidelity"]["league"]
    if league==1:assert int(aggregate["league-1"])==sum(nominals)
    else:
        assert int(aggregate[f"league-{league}"])==nominals[0]
        assert int(aggregate["league-1"])==sum(nominals[1:])
    before=[value(j["genesis"][name]) for name in ("old-active","young-active")]
    active=[value(j["forced_recovered"][f"active-{i}"]) for i in range(2)]
    sold=[value(j["forced_recovered"][f"sold-{i}"]) for i in range(2)]
    remaining=int(j["debit"])
    for i in reversed(range(2)):
        take=min(before[i],remaining);remaining-=take
        assert sold[i]==take and active[i]==before[i]-take
    assert remaining==0
    pool=int(j["pool"]);denominator=sum(nominals[:2]);payouts=[value(p) for p in j["payouts"]]
    assert payouts==[pool*n//denominator for n in nominals[:2]]
    assert sum(payouts)+value(j["remainder"])==pool
    expiry=j["expiry"]
    left,right=[load(p) for p in expiry["shares"]]
    q=2736030358979909402780800718157159386076813972158567259200215660948447373041
    limbs=[(2*int(a["y"])-int(b["y"]))%q for a,b in zip(left,right)]
    assert all(0<=v<2**64 for v in limbs)
    assert sum(v*2**(64*k) for k,v in enumerate(limbs))==sum(nominals[i]*int(f)*10**6 for i,f in zip(expiry["wallet_indexes"],expiry["fractions"]))
    f=j["fidelity"]
    q={"op":"fidelity","now":f["now"],"qualified":f["qualified"],"maximum":f["maximum"],"active":[{"value":str(before[0]),"at":f["old"]},{"value":str(before[1]),"at":f["young"]}],"sold":[{"value":str(value(j["genesis"]["sold"])),"at":f["old"],"sold_at":f["sold_at"]}],"out":str(private/"reference.private.json")}
    qp=private/"reference-job.private.json"
    fd=os.open(qp,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
    with os.fdopen(fd,"w") as out:json.dump(q,out)
    binary=Path(__file__).resolve().parent/"target/release/poc-math"
    subprocess.run([str(binary),str(qp)],check=True)
    ref=load(q["out"])
    assert ref["valid"] and ref["league"]==f["league"]
    Path(j["out"]).write_text(json.dumps({"aggregate_matches_private_fixture":True,"league_matches_current_rust":True,"lifo_matches_private_fixture":True,"intex_exact_floor_and_conservation":True,"expiry_matches_private_unclaimed_sum":True,"role":"test oracle only; no amounts returned"}))
if __name__=="__main__":main()
