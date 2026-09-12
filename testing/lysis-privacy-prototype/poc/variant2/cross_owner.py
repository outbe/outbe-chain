"""Experimental transfer policy: sender Out, recipient In at atomic acceptance.

Production Gratis transfer is disabled. This is an explicitly new PoC scenario,
with independent wallet processes and the existing passive history committee.
"""
import json
from run_lifecycle import read,write,sha,WALLET_LIMIT
from cohort_bridge import CohortBridge

def history(run,bridge,tag,private,statement,context,amount_index,direction):
    wallet=bridge.wallet;members=bridge.members
    indices=[0,2,amount_index]
    ids={i:f"money-{tag}-note-{i}" for i in indices}
    secrets=wallet/f"{tag}-vss.private.json"
    run.vss("cross-wallet-vss-witness",{"op":"wallet-secrets","wallet":str(wallet/"wallet.private.json"),"skip_nominal":True,"notes":[{"id":ids[i],"path":str(private/f"note-{i}.private.json")} for i in indices],"out":str(secrets)})
    bundle=run.public/f"{tag}-vss.json"
    run.vss("cross-wallet-vss-deal",{"op":"deal","dir":str(wallet),"secrets":str(secrets),"epoch":2,"registry":bridge.registry,"out":str(bundle)})
    polynomials=read(bundle)["polynomials"]
    for i in indices:
        assert [next(p for p in polynomials if p["id"]==f"{ids[i]}:{k}")["points"][0] for k in range(4)]==statement["notes"][i]
    for i,member in enumerate(members):
        run.vss("cross-holder-admission",{"op":"accept","dir":str(member),"x":i+1,"epoch":2,"bundle":str(bundle),"out":str(run.public/f"{tag}-receipt-{i}.json")})
    bridge.compute(tag,direction,{"id":ids[amount_index],"factor":"1"},[ids[0]],[ids[2]],context,{"domain":"wallet-statement","digest":sha(statement)})

def run_cross_owner(run):
    owner_a=run.owner
    genesis_owner=next(iter(run.wallets_by_owner))
    owner_b=next(o for o in run.wallets_by_owner if o not in (owner_a,genesis_owner))
    wallet_a=run.wallets_by_owner[owner_a];wallet_b=run.wallets_by_owner[owner_b]
    sk_a,pk_a=run.cipher_key(owner_a);sk_b,pk_b=run.cipher_key(owner_b)
    a=run.cohort;b=CohortBridge(run,wallet_b,owner_b,run.current_members,run.current_registry)
    old_a=run.latest_note;old_b="cross-recipient-zero"
    zero=wallet_b/"cross-zero.private.json";zero_pub=wallet_b/"cross-zero.public.json"
    run.job("poc-state","recipient-zero",{"op":"note","value":"0","canonical_zero":True,"out":str(zero),"public":str(zero_pub)})
    with run.db:run.note(old_b,"gratis",owner_b,read(zero_pub))
    transfer={"chain":19280501,"id":"cross-owner-transfer","sender":owner_a,"recipient":owner_b,"sender_key":read(pk_a),"recipient_key":read(pk_b),"key_epoch":0,"root":run.root,"timestamp":run.now,"sender_input":old_a,"recipient_input":old_b,"sender_output":"cross-sender-balance","recipient_output":"cross-recipient-balance"}
    ca={"chain":19280501,"op":"cross-send","operation_id":"cross-send","owner":owner_a,"input_ids":[old_a],"output_ids":[transfer["sender_output"]],"transfer":transfer,"encryption_key":read(pk_a),"encryption_key_epoch":0}
    cb={"chain":19280501,"op":"cross-receive","operation_id":"cross-receive","owner":owner_b,"input_ids":[old_b],"output_ids":[transfer["recipient_output"]],"transfer":transfer,"encryption_key":read(pk_b),"encryption_key_epoch":0}
    a.bind_context(ca);b.bind_context(cb)
    pa=run.private/"wallet-operation-cross-send";pb=run.private/"wallet-operation-cross-receive"
    ua=run.public/"operation-cross-send";ub=run.public/"operation-cross-receive"
    run.job("poc-state","sender-prepare-private-debit",{"op":"prepare","kind":"move","out":str(pa),"old":str(run.latest_private),"divisor":7,"context":ca})
    dual=run.public/"dual-transfer.json";oa=run.public/"sender-overrides.json";ob=run.public/"receiver-overrides.json"
    run.twisted("sender-encrypt-dual-amount",{"op":"dual","secret":str(sk_a),"recipient_key":str(pk_b),"amount_note":str(pa/"note-3.private.json"),"context":transfer,"overrides":str(oa),"out":str(dual)},limit=WALLET_LIMIT)
    sa=run.prove_prepared("cross-send","move",pa,ua,ca,oa)
    # Receiver reads only its DK, its own old note, and a PUBLIC transfer.
    incoming=wallet_b/"cross-incoming.private.json"
    run.twisted("recipient-key-only-amount-recovery",{"op":"receive-input","dual":str(dual),"secret":str(sk_b),"context":transfer,"private_out":str(incoming),"overrides":str(ob)},limit=WALLET_LIMIT)
    run.job("poc-state","recipient-prepare-private-credit",{"op":"prepare","kind":"move","out":str(pb),"old":str(zero),"incoming":str(incoming),"context":cb})
    sb=run.prove_prepared("cross-receive","move",pb,ub,cb,ob)
    run.twisted("verify-cross-owner-amount-link",{"op":"verify-dual","dual":str(dual),"sender_proof":str(ua/"twisted.bin"),"receiver_proof":str(ub/"twisted.bin"),"sender_receipt":run.verified_bundles[sha(sa)],"receiver_receipt":run.verified_bundles[sha(sb)],"context":transfer,"out":str(run.public/"cross-owner-link.json")})
    link=read(run.public/"cross-owner-link.json")
    assert link["sender_statement"]==sa and link["receiver_statement"]==sb
    history(run,a,"cross-send",pa,sa,ca,3,"out")
    history(run,b,"cross-receive",pb,sb,cb,1,"in")

    def commit(ac,bc):
        if ac!=ca or bc!=cb or ac["transfer"]!=transfer or bc["transfer"]!=transfer:raise ValueError("cross-owner context binding")
        if run.verified_statements.get(sha(sa))!=owner_a or run.verified_statements.get(sha(sb))!=owner_b:raise ValueError("missing independently verified owner proofs")
        import hashlib
        field=21888242871839275222246405745257275088548364400416034343698204186575808495617
        for c,s in ((ac,sa),(bc,sb)):
            expected=int.from_bytes(hashlib.sha256(json.dumps(c,sort_keys=True,separators=(",",":")).encode()).digest(),"big")%field
            if int(s["context"],16)!=expected:raise ValueError("proof does not bind exact transfer/cohort context")
        with run.db:
            if run.db.execute("SELECT 1 FROM operation WHERE id=?",(transfer["id"],)).fetchone():raise ValueError("transfer replay")
            for old,owner,s in ((old_a,owner_a,sa),(old_b,owner_b,sb)):
                row=run.db.execute("SELECT asset,owner,commitments,spent FROM note WHERE id=?",(old,)).fetchone()
                if not row or row[0]!="gratis" or row[1]!=owner or json.loads(row[2])!=s["notes"][0] or row[3]:raise ValueError("stale/foreign transfer input")
                run.db.execute("UPDATE note SET spent=1 WHERE id=?",(old,))
            run.note(transfer["sender_output"],"gratis",owner_a,sa["notes"][2])
            run.note(transfer["recipient_output"],"gratis",owner_b,sb["notes"][2])
            a.commit("cross-send",ac,sa);b.commit("cross-receive",bc,sb)
            run.db.execute("INSERT INTO operation VALUES(?,?,?)",(transfer["id"],"experimental-cross-owner-transfer",json.dumps(transfer)))

    before=(a.current(),b.current(),list(run.db.execute("SELECT id,spent FROM note ORDER BY id")))
    run.now+=1
    try:commit(ca,cb)
    except ValueError:run.failures.append("cross-owner exact timestamp rollback")
    else:raise AssertionError("stale cross-owner acceptance")
    run.now-=1
    assert before==(a.current(),b.current(),list(run.db.execute("SELECT id,spent FROM note ORDER BY id")))
    commit(ca,cb)
    after=(a.current(),b.current(),list(run.db.execute("SELECT id,spent FROM note ORDER BY id")))
    try:commit(ca,cb)
    except ValueError:run.failures.append("cross-owner replay")
    else:raise AssertionError("repeated transfer")
    assert after==(a.current(),b.current(),list(run.db.execute("SELECT id,spent FROM note ORDER BY id")))
    run.now+=60
    run.latest_private=pa/"note-2.private.json";run.latest_note=transfer["sender_output"]
    for owner,wallet,sk,public in ((owner_a,wallet_a,sk_a,ua),(owner_b,wallet_b,sk_b,ub)):
        run.twisted("cold-cross-owner-key-only-recovery",{"op":"recover","secret":str(sk),"ciphers":str(public/"ciphertexts.json"),"private_out":str(wallet/"cross-key-recovered.private.json"),"out":str(public/"key-recovery.json")},limit=WALLET_LIMIT)
    write(run.public/"cross-owner.json",{"executed":True,"independent_wallet_processes":True,"one_prover_never_receives_both_owner_keys":True,"amount_link_verified":True,"same_transaction_two_balances_and_two_cohort_roots":True,"stale_timestamp_rollback":True,"replay_rejected":True,"key_only_balance_recovery":True,"recipient_acceptance_required":True,"policy":"experimental sender Out / recipient In at atomic acceptance; production Gratis transfers remain disabled","sender":owner_a,"recipient":owner_b})
