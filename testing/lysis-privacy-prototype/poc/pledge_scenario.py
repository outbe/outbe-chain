"""L -> T -> A -> L and offline A burn, on the core account's same cohort root."""
import json
from run_lifecycle import read, write

def run_pledges(run):
    owner=run.owner
    latest,latest_id=run.latest_private,run.latest_note
    for index,amount in ((1,6*10**18),(2,3*10**18)):
        tag=f"pledge-{index}"
        context={"chain":19280501,"op":"pledge","root":run.root,"input_ids":[latest_id],"position":f"position-{index}","collateral6":str(amount//10**12)}
        private,_,s=run.transition(tag,"pledge",{"old":str(latest),"amount":str(amount)},context)
        assert int(s["amount"])==int(context["collateral6"])*10**12
        liquid_id,ticket=f"pledge-liquid-{index}",f"pledge-ticket-{index}"
        run.commit_transition(tag,"pledge",s,context,[latest_id],[liquid_id,ticket],owner)
        # Authority consumes the ticket. Owner is offline for this transition.
        ids=[f"money-{tag}-note-1",f"money-{tag}-note-2"]
        consume=f"consume-{index}"
        context={"chain":19280501,"op":"consume-pledge","ticket":ticket,"root":run.root,"position":f"position-{index}"}
        run.cohort.bind_context(context)
        run.cohort.compute(consume,"neutral",{"public":"0"},ids,ids,context)
        with run.db:
            row=run.db.execute("SELECT asset,spent FROM note WHERE id=?",(ticket,)).fetchone()
            assert row==("pending-pledge",0)
            run.db.execute("UPDATE note SET asset='active-pledge' WHERE id=?",(ticket,))
            run.db.execute("INSERT INTO operation VALUES(?,?,?)",(consume,"authority-consume",json.dumps(context)))
            run.cohort.commit(consume,context)
        run.now+=60
        if index==1:
            context={"chain":19280501,"op":"release","root":run.root,"input_ids":[liquid_id,ticket],"position":"position-1"}
            release,_,s=run.transition("release-1","move",{"old":str(private/"note-1.private.json"),"incoming":str(private/"note-2.private.json")},context)
            run.commit_transition("release-1","move",s,context,[liquid_id,ticket],["released-liquid","released-empty"],owner)
            latest,latest_id=release/"note-2.private.json","released-liquid"
        else:
            context={"chain":19280501,"op":"forced-burn","root":run.root,"ticket":ticket,"position":"position-2","collateral6":str(amount//10**12)}
            run.cohort.bind_context(context)
            run.cohort.compute("forced-burn","out",{"public":str(amount)},ids,ids[:1],context)
            with run.db:
                row=run.db.execute("SELECT asset,spent FROM note WHERE id=?",(ticket,)).fetchone()
                assert row==("active-pledge",0)
                run.db.execute("UPDATE note SET spent=1 WHERE id=?",(ticket,))
                run.db.execute("INSERT INTO operation VALUES(?,?,?)",("forced-burn","authority-burn",json.dumps(context)))
                run.cohort.commit("forced-burn",context)
            run.now+=60
            latest,latest_id=private/"note-1.private.json",liquid_id
    # An old owner/committee context cannot be applied even when the liquid
    # note itself survived the external A burn unchanged.
    before=run.cohort.current()
    try:run.cohort.commit("pledge-2",run.cohort.pending["pledge-2"]["context"])
    except ValueError:run.failures.append("forced writer invalidates old account version")
    else:raise AssertionError("stale external-write context accepted")
    assert run.cohort.current()==before
    run.cohort.owner_query()
    write(run.public/"pledge.json",{"L_to_T":True,"T_to_A_owner_offline":True,"A_to_L_without_Fidelity_in":True,"forced_A_burn_owner_offline":True,"forced_out_exact_lifo":True,"same_core_cohort_root":True,"public_collateral_grid":"fixed6 * 10^12; experimental disclosure policy","private_account_balances":True})
