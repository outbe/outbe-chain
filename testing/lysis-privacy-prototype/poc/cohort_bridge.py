"""Atomically connects each tested Gratis money writer to the same MPC state.

Committee certificates have the explicitly passive/honest-majority PoC trust
model. They are not public SNARKs or a malicious-MPC replacement.
"""
import hashlib
import json
from run_lifecycle import read, write, sha
from lifecycle_branches import export_note

class CohortBridge:
    def __init__(self,run,wallet,owner,members,registry):
        self.run,self.wallet,self.owner,self.members,self.registry=run,wallet,owner,members,registry
        self.key="cohort-"+owner
        self.pending={}
        empty={"active":[],"sold":[],"qualified":0,"version":0}
        empty["root"]=sha(empty)
        with run.db:run.db.execute("INSERT INTO state VALUES(?,?)",(self.key,json.dumps(empty)))

    def current(self):
        return json.loads(self.run.db.execute("SELECT value FROM state WHERE key=?",(self.key,)).fetchone()[0])

    def bind_context(self,context):
        state=self.current()
        context.update(cohort_root=state["root"],cohort_version=state["version"],candidate_timestamp=self.run.now)

    def prepare(self,tag,kind,private,statement,context):
        run=self.run;state=self.current();prefix="money-"+tag
        if kind=="claim":old_indices,new_indices=[0],[2];direction="in"
        elif kind=="move":old_indices,new_indices=[0,1],[2,3];direction="neutral"
        elif kind=="withdraw":old_indices,new_indices=[0],[1];direction="out"
        elif kind=="mint":old_indices,new_indices=[0],[2];direction="in"
        elif kind=="pledge":old_indices,new_indices=[0],[1,2];direction="neutral"
        else:raise ValueError("uncoupled money writer")
        indices=sorted(set(old_indices+new_indices+([1] if kind=="mint" else [])))
        note_ids={i:f"{prefix}-note-{i}" for i in indices}
        secrets=self.wallet/f"{prefix}-shares.private.json"
        run.vss("money-state-vss-witness",{"op":"wallet-secrets","wallet":str(self.wallet/"wallet.private.json"),"skip_nominal":True,"notes":[{"id":note_ids[i],"path":str(private/f"note-{i}.private.json")} for i in indices],"out":str(secrets)})
        bundle=run.public/f"{prefix}-vss.json"
        run.vss("money-state-vss-deal",{"op":"deal","dir":str(self.wallet),"secrets":str(secrets),"epoch":2,"registry":self.registry,"out":str(bundle)})
        ps=read(bundle)["polynomials"]
        for i in indices:
            assert [next(p for p in ps if p["id"]==f"{note_ids[i]}:{k}")["points"][0] for k in range(4)]==statement["notes"][i]
        for i,member in enumerate(self.members):
            run.vss("money-state-vss-accept",{"op":"accept","dir":str(member),"x":i+1,"epoch":2,"bundle":str(bundle),"out":str(run.public/f"{prefix}-receipt-{i}.json")})
        delta={"public":statement["amount"] if kind=="withdraw" else "0"}
        if kind=="claim":delta={"id":context["nod"],"limbs":False,"factor":str(int(statement["fraction"])*10**6)}
        if kind=="mint":delta={"id":note_ids[1],"factor":str(10**12)}
        self.compute(tag,direction,delta,[note_ids[i] for i in old_indices],[note_ids[i] for i in new_indices],context,{"domain":"wallet-statement","digest":sha(statement)})

    def compute(self,tag,direction,delta,old_ids,new_ids,context,request=None):
        run=self.run;state=self.current();prefix="money-"+tag
        if request is None:request={"domain":"authority","digest":sha({"tag":tag,"direction":direction,"delta":delta,"old":old_ids,"new":new_ids,"context":context})}
        now=context["candidate_timestamp"]
        next_active=[dict(r) for r in state["active"]]
        new_sold=[]
        if direction=="in":next_active.append({"at":now})
        if direction=="out":new_sold=[{"at":r["at"],"sold_at":now} for r in state["active"]]
        ages=sorted(set([0]+[max(0,now-r["at"]) for r in next_active+state["sold"]+new_sold]+[max(0,now-r["sold_at"]) for r in state["sold"]+new_sold]))
        decay_values=run.math(prefix+"-decay",{"op":"decay","ages":ages})["values"]
        decay=dict(zip(ages,map(int,decay_values)))
        def active(r):return {**r,"decay":str(decay[max(0,now-r["at"])])}
        def sold(r):return {**r,"held":str(decay[max(0,now-r["at"])]-decay[max(0,now-r["sold_at"])])}
        plan={"op":"update","members":list(map(str,self.members)),"epoch":2,"context":context,"direction":direction,"delta":delta,"old_balance":old_ids,"new_balance":new_ids,"active":[active(r) for r in state["active"]],"next_active":[active(r) for r in next_active],"sold":[sold(r) for r in state["sold"]],"new_sold":[sold(r) for r in new_sold],"qualified_after":state["qualified"] or (now if direction=="in" else 0)}
        plan["request"]=request
        result=run.mpc(prefix,plan)
        assert result["money_cohort_conservation"]
        outputs=[]
        for label,rows in (("active",next_active),("sold",new_sold)):
            for i,row in enumerate(rows):
                name=f"{label}-{i}"
                recovered,polys=export_note(run,prefix,name,self.members,2,self.wallet)
                row["id"]=f"{prefix}-{name}"
                row["output_tag"]=prefix
                row["output_name"]=name
                row["polynomial_hash"]=hashlib.sha256(json.dumps(read(polys),separators=(",", ":")).encode()).hexdigest()
                outputs.append({"name":name,"polynomials":polys})
                for k,member in enumerate(self.members):
                    run.vss("persist-next-cohort-shares",{"op":"install-mpc-output","dir":str(member),"polynomials":polys,"shares":str(member/f"{prefix}-{name}.private.json"),"out":str(run.public/prefix/f"{name}-installed-{k}.json")})
        next_state={"active":next_active,"sold":state["sold"]+new_sold,"qualified":state["qualified"] or (now if direction=="in" else 0),"version":state["version"]+1}
        next_state["root"]=sha(next_state)
        hashes=[hashlib.sha256(json.dumps(read(o["polynomials"]),separators=(",", ":")).encode()).hexdigest() for o in outputs]
        body={"operation":"update","context":context,"request":request,"next_state":next_state,"polynomial_hashes":hashes}
        bodypath=write(run.public/prefix/"certificate-body.json",body)
        if tag=="claim":
            for fault in ("at","qualified","output-id","request","cross-operation"):
                bad=json.loads(json.dumps(body))
                if fault=="at":bad["next_state"]["active"][0]["at"]+=1
                elif fault=="qualified":bad["next_state"]["qualified"]+=1
                elif fault=="output-id":bad["next_state"]["active"][0]["id"]="unrelated-output"
                elif fault=="request":bad["request"]["digest"]="00"*32
                else:bad["operation"]="expiry"
                del bad["next_state"]["root"]
                bad["next_state"]["root"]=sha(bad["next_state"])
                badpath=write(run.control/f"bad-certificate-{fault}.json",bad)
                run.vss(f"reject-MPC-metadata-{fault}",{"op":"certify-mpc","dir":str(self.members[0]),"body":badpath,"report":str(run.public/prefix/"party-0.json"),"outputs":[{"polynomials":o["polynomials"],"shares":str(self.members[0]/f"{prefix}-{o['name']}.private.json")} for o in outputs],"out":str(run.control/f"forbidden-signature-{fault}.json")},expect_fail=True)
        for i,member in enumerate(self.members):
            out=run.public/prefix/f"certificate-{i}.json"
            run.vss("certify-money-and-cohorts",{"op":"certify-mpc","dir":str(member),"body":bodypath,"report":str(run.public/prefix/f"party-{i}.json"),"outputs":[{"polynomials":o["polynomials"],"shares":str(member/f"{prefix}-{o['name']}.private.json")} for o in outputs],"out":str(out)})
            run.vss("verify-money-cohort-certificate",{"op":"verify-json","identity":str(member/"identity.json"),"signed":str(out)})
        self.pending[tag]=body

    def commit(self,tag,context,statement=None):
        body=self.pending[tag];current=self.current()
        expected={"domain":"wallet-statement","digest":sha(statement)} if statement is not None else body["request"]
        if body["request"]!=expected or (statement is None and body["request"]["domain"]!="authority"):
            raise ValueError("cohort certificate does not bind this monetary statement/authority domain")
        if body["context"]!=context or context["candidate_timestamp"]!=self.run.now or context["cohort_root"]!=current["root"] or context["cohort_version"]!=current["version"]:
            raise ValueError("stale money/cohort root, version or exact execution timestamp")
        # Invoked inside the SAME SQLite transaction as all monetary writes.
        self.run.db.execute("UPDATE state SET value=? WHERE key=?",(json.dumps(body["next_state"]),self.key))

    def owner_query(self):
        from pathlib import Path
        from run_lifecycle import HERE
        current=self.current();now=self.run.now+86400
        spec={"op":"owner-query","now":now,"qualified":current["qualified"],"maximum":"526583689924471619584","active":[],"sold":[],"private_dir":str(self.wallet/"exact-fidelity-query"),"out":str(self.run.public/"owner-query.json")}
        for label in ("active","sold"):
            for r in current[label]:
                # export_note used this exact owner-private recovery filename.
                prefix=r["output_tag"]
                name=r["output_name"]
                spec[label].append({"path":str(self.wallet/f"recovered-{prefix}-{name}.private.json"),"at":r["at"],**({"sold_at":r["sold_at"]} if label=="sold" else {})})
        job=write(self.run.control/"owner-query-job.json",spec)
        self.run.command("owner-exact-fidelity-query",["python3",HERE/"wallet_oracle.py",job])
