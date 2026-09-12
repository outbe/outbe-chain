"""Core pledge writers plus separate forced-out and funded Intex fixtures.

Expiry consumes the actual remaining rights from the shared lifecycle database.
The extra wallet-0 forced-out fixture is an independent arithmetic oracle check.
"""
import json
import hashlib
from pathlib import Path
from run_lifecycle import read, write, sha, HERE, BIN

def certify_outputs(run,tag,context,outputs,members):
    hashes=[hashlib.sha256(json.dumps(read(p),separators=(",", ":")).encode()).hexdigest() for _,p in outputs]
    body=write(run.public/tag/"certificate-body.json",{"operation":tag,"context":context,"polynomial_hashes":hashes})
    for i,member in enumerate(members):
        out=run.public/tag/f"certificate-{i}.json"
        run.vss("certify-private-computation",{"op":"certify-mpc","dir":str(member),"body":body,"report":str(run.public/tag/f"party-{i}.json"),"outputs":[{"polynomials":p,"shares":str(member/f"{tag}-{name}.private.json")} for name,p in outputs],"out":str(out)})
        run.vss("verify-private-computation-certificate",{"op":"verify-json","identity":str(member/"identity.json"),"signed":str(out)})

def export_note(run, tag, name, members, epoch, wallet):
    public = run.public / tag
    evaluations=[]
    for i,member in enumerate(members):
        out=public/f"{name}-evaluation-{i}.json"
        run.vss("commit-local-mpc-share",{"op":"commit-share","input":str(member/f"{tag}-{name}.private.json"),"out":str(out)})
        evaluations.append(str(out))
    polys=public/f"{name}-polynomials.json"
    run.vss("bind-output-sharing",{"op":"interpolate-outputs","evaluations":evaluations,"epoch":epoch,"id":f"{tag}-{name}","out":str(polys)})
    packets=[]
    for i,member in enumerate(members):
        out=public/f"{name}-recovery-{i}.json"
        run.vss("encrypt-owner-recovery",{"op":"export-output","shares":str(member/f"{tag}-{name}.private.json"),"polynomials":str(polys),"owner":str(wallet/"identity.json"),"out":str(out)})
        packets.append(str(out))
    recovered=wallet/f"recovered-{tag}-{name}.private.json"
    run.vss("offline-owner-recovery",{"op":"recover-note","dir":str(wallet),"polynomials":str(polys),"packets":packets[:2],"out":str(recovered)})
    return str(recovered), str(polys)

def private_branches(run,wallets,offers,members,registry,polynomials,mpc_common,account,now,old,young,sold_at,qualified):
    from pledge_scenario import run_pledges
    run_pledges(run)
    print("separate private committee snapshots: forced Out, owner recovery, exact Intex floor",flush=True)
    debit=10**18
    forced=run.mpc("forced-out",{"op":"forced-out",**mpc_common,"debit18":str(debit),"active":[{"id":r["id"],"decay":r["decay"],"held_after_sale":r["decay"]} for r in account["active"]]})
    recoveries={}
    for i in range(2):
        for label in ("active","sold"):
            name=f"{label}-{i}"
            recoveries[name],_=export_note(run,"forced-out",name,members,2,wallets[0])
    # Payout H stays secret. Opening the two amounts is unnecessary: every
    # holder supplies its own saved nominal shares to exact secret division.
    pool=10**21+17
    intex_context={**mpc_common["context"],"purpose":"funded-intex-round","pool18":str(pool),"rights":[o["nft_hash"] for o in offers[:2]]}
    result=run.mpc("intex",{"op":"intex",**mpc_common,"context":intex_context,"rights":[o["nft_hash"] for o in offers[:2]],"pool18":str(pool)})
    payouts=[]
    payout_polys=[]
    for i in range(2):
        recovered,polys=export_note(run,"intex",f"payout-{i}",members,2,wallets[i])
        payouts.append(recovered)
        payout_polys.append(polys)
    remainder,remainder_polys=export_note(run,"intex","round-remainder",members,2,wallets[0])
    certify_outputs(run,"intex",intex_context,[(f"payout-{i}",p) for i,p in enumerate(payout_polys)]+[("round-remainder",remainder_polys)],members)
    with run.db:
        run.db.execute("INSERT INTO operation VALUES(?,?,?)",("intex-funded-round","private-coen-backed-credit",json.dumps(intex_context)))
        for i,polys in enumerate(payout_polys):
            run.note(f"intex-payout-{i}","coen-backed",offers[i]["derived_owner"],[p["points"][0] for p in read(polys)])
        run.note("intex-burned-remainder","coen-backed-burned","pool",[p["points"][0] for p in read(remainder_polys)])
    context={"chain":19280501,"op":"intex-cashout","root":run.root,"input_ids":["intex-payout-0"],"recipient":"intex-public-recipient","owner":offers[0]["derived_owner"]}
    _,_,s=run.transition("intex-cashout","withdraw",{"old":payouts[0],"amount":"1"},context)
    run.commit_transition("intex-cashout","withdraw",s,context,["intex-payout-0"],["intex-payout-change"],offers[0]["derived_owner"],asset="coen-backed")
    # Keep forfeit amount shared. Returning it as a public number would add a
    # disclosure beyond the approved S/S_l policy. This PoC creates a private
    # budget carry note; the current production public budget adapter is a gate.
    nods=read(run.public/"nods.json")
    remaining=[n for n in nods if not run.db.execute("SELECT spent FROM nod WHERE id=?",(n["source"],)).fetchone()[0] and n["called"]]
    run.now=max(n["deadline"] for n in nods)+1
    assert all(n["deadline"]<run.now for n in remaining)
    expiry_context={"purpose":"closed-expiry-batch","root":run.root,"execution_timestamp":run.now,"deadline":max(n["deadline"] for n in nods),"unclaimed":[n["source"] for n in remaining],"fractions":[str(n["fraction"]) for n in remaining]}
    run.mpc("expiry",{"op":"expiry",**mpc_common,"context":expiry_context,"rights":[{"id":n["source"],"factor":str(n["fraction"]*10**6)} for n in remaining],"reserved18":read(run.public/"lysis.json")["reserved18"]})
    evaluations=[]
    for i,member in enumerate(members):
        out=run.public/"expiry"/f"returned-limit-evaluation-{i}.json"
        run.vss("commit-private-expiry-share",{"op":"commit-share","input":str(member/"expiry-returned-limit.private.json"),"out":str(out)})
        evaluations.append(str(out))
    expiry_polys=str(run.public/"expiry/returned-limit-polynomials.json")
    run.vss("bind-private-budget-carry",{"op":"interpolate-outputs","evaluations":evaluations,"epoch":2,"id":"expiry-returned-limit","out":expiry_polys})
    certify_outputs(run,"expiry",expiry_context,[("returned-limit",expiry_polys)],members)
    for i,member in enumerate(members):
        run.vss("persist-private-returned-limit",{"op":"install-mpc-output","dir":str(member),"polynomials":expiry_polys,"shares":str(member/"expiry-returned-limit.private.json"),"out":str(run.public/"expiry"/f"installed-{i}.json")})
    with run.db:
        run.db.execute("INSERT INTO operation VALUES(?,?,?)",("expiry","private-budget-carry",json.dumps(expiry_context)))
        for n in remaining:run.db.execute("UPDATE nod SET spent=2 WHERE id=?",(n["source"],))
        run.note("returned-private-limit","limit-private","budget",[p["points"][0] for p in read(expiry_polys)])
    # Certificate plumbing is measured, but these signatures alone do not
    # prove correct computation to arbitrary noncommittee verifiers.
    body={"old_context":mpc_common["context"],"forced_new_roots":[sha(read(run.public/"forced-out"/f"{label}-{i}-polynomials.json")) for i in range(2) for label in ("active","sold")],"candidate_timestamp":now,"account_version":1}
    bodypath=write(run.public/"forced-certificate-body.json",body)
    for i,member in enumerate(members):
        out=run.public/f"forced-certificate-{i}.json"
        run.vss("committee-sign-candidate",{"op":"sign-json","dir":str(member),"body":bodypath,"out":str(out)})
        run.vss("committee-verify-candidate",{"op":"verify-json","identity":str(member/"identity.json"),"signed":str(out)})
    state={"version":1,"timestamp":now,"root":mpc_common["context"]["root"]}
    def finalize(candidate, expected):
        if candidate["account_version"]!=expected["version"] or candidate["candidate_timestamp"]!=expected["timestamp"]:
            raise ValueError("stale committee candidate before monetary commit")
        expected["version"]+=1
    try:finalize(body,{**state,"timestamp":now+1})
    except ValueError:run.failures.append("stale exact-time committee candidate")
    else:raise AssertionError("stale timestamp accepted")
    finalize(body,state)
    # Test-only oracle is a different actor; no secrets return to coordinator.
    oracle={"wallets":[str(w/"wallet.private.json") for w in wallets],"aggregate":str(run.public/"aggregate.json"),"genesis":{name:str(wallets[0]/f"{name}.private.json") for name in ("old-active","young-active","sold")},"forced_recovered":recoveries,"debit":str(debit),"pool":str(pool),"payouts":payouts,"remainder":remainder,"fidelity":{"now":now,"old":old,"young":young,"sold_at":sold_at,"qualified":qualified,"maximum":account["maximum"],"league":read(run.public/"late-fidelity/party-0.json")["leagues"][0]},"private_dir":str(run.private/"test-oracle"),"out":str(run.public/"oracle.json")}
    oracle["expiry"]={"wallet_indexes":[i for i,n in enumerate(nods) if n in remaining],"fractions":[str(n["fraction"]) for n in remaining],"shares":[str(member/"expiry-returned-limit.private.json") for member in members[:2]]}
    path=write(run.control/"oracle-job.json",oracle)
    run.command("private-fixture-oracle",["python3",HERE/"wallet_oracle.py",path])
    write(run.public/"branch-coverage.json",{
        "main_money_and_fidelity":{"same_root":True,"atomic_commit":True,"stale_time_rollback":True},
        "forced_out":{"private_lifo":True,"owner_recovery":True,"exact_timestamp_rejection":True,"core_account_forced_burn":True},
        "intex":{"private_denominator":True,"independent_floor":True,"owner_recovery":True,"backed_private_note_and_coen_cashout":True,"funding_profile":"explicit local genesis pool"},
        "pledge":read(run.public/"pledge.json"),
        "r18":read(run.public/"owner-query.json"),
        "r14":{"expired_rights_closed":True,"private_returned_limit_commitment":True,"public_F_not_disclosed":True,"production_public_budget_adapter":"unimplemented policy gate; reusing the private returned limit is not tested"},
        "experimental_lifecycle_executed":True,"full_production_protocol_pass":False})
