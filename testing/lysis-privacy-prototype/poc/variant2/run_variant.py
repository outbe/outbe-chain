#!/usr/bin/env python3
"""Second backend; imports unchanged lifecycle/VSS/MPC consumers from baseline."""
import argparse
import hashlib
import json
from pathlib import Path
import sys

V2=Path(__file__).resolve().parent
sys.path.insert(0,str(V2.parent))
from run_lifecycle import Run,read,write,sha,size,HERE,REPO,WALLET_LIMIT

class TwistedRun(Run):
    def __init__(self,*args):
        super().__init__(*args)
        self.cipher_registry=self.public/"cipher-registry.json"
        write(self.cipher_registry,{})
        self.cipher_keys={}
        self.verified_bundles={}
        self.source_paths+=sorted(list((V2/"src").rglob("*.rs"))+list(V2.glob("*.py"))+[V2/"Cargo.toml",V2/"Cargo.lock"])
        self.source_snapshot={str(p.relative_to(REPO)):hashlib.sha256(p.read_bytes()).hexdigest() for p in self.source_paths}
        write(self.public/"source-snapshot.json",self.source_snapshot)

    def twisted(self,name,body,**kw):
        job=write(self.control/f"twisted-{self.sequence+1:05}-{name}.json",body)
        return self.command(name,[V2/"target/release/outbe-twisted-poc",job],**kw)

    def cipher_key(self,owner):
        if owner not in self.cipher_keys:
            wallet=self.wallets_by_owner[owner]
            secret=wallet/"twisted-key.private.json"; public=wallet/"twisted-key.public.json"
            self.twisted("twisted-keygen",{"op":"keygen","secret":str(secret),"public":str(public)})
            context={"chain":19280501,"purpose":"register-encryption-key","owner":owner,"epoch":0,"key":read(public)}
            body=write(self.public/f"cipher-key-{owner}.json",context)
            signed=str(self.public/f"cipher-key-{owner}-signature.json")
            if read(wallet/"identity.json")!=read(self.public/"source-registry.json")["wallet_owners"][owner]:raise ValueError("unregistered owner")
            self.vss("sign-cipher-key",{"op":"sign-json","dir":str(wallet),"body":body,"out":signed})
            self.vss("verify-cipher-key",{"op":"verify-json","identity":str(wallet/"identity.json"),"signed":signed})
            self.cipher_keys[owner]=(secret,public)
        return self.cipher_keys[owner]

    def prove_prepared(self,tag,kind,private,public,context,overrides=None):
        owner=context["owner"];secret,key=self.cipher_key(owner)
        parameters=HERE/"parameters"
        economics=""
        if kind in ("claim","mint","pledge"):
            profile=parameters/f"{kind}-v2"
            if not (profile/"pk.bin").exists():
                self.job("poc-state",f"setup-{kind}",{"op":"setup","kind":kind,"dir":str(profile)},limit=2_000_000_000)
            self.job("poc-state",f"retained-cold-{kind}-proof",{"op":"prove","parameters":str(profile),"witness":str(private/"transition.private.json"),"out":str(public)},limit=WALLET_LIMIT)
            economics=str(public/"proof.bin")
        frozen=public/"registry-before.json"
        write(frozen,read(self.cipher_registry))
        self.twisted(f"cold-twisted-{tag}",{"op":"prove","witness":str(private/"transition.private.json"),"registry":str(frozen),"secret":str(secret),"parameters":str(parameters),"economics":economics,"out":str(public),**({"overrides":str(overrides)} if overrides else {})},limit=WALLET_LIMIT)
        next_registry=public/"next-registry.json"
        self.twisted(f"verify-twisted-{tag}",{"op":"verify","proof":str(public/"twisted.bin"),"registry":str(frozen),"key":str(key),"parameters":str(parameters),"next_registry":str(next_registry),"out":str(public/"twisted-verify.json")})
        checked=read(public/"twisted-verify.json")
        statement=read(public/"statement.public.json")
        if checked["statement"]!=statement or checked["ciphers"]!=read(public/"ciphertexts.json"):raise ValueError("node proof/public artifact mismatch")
        write(self.cipher_registry,read(next_registry))
        wallet=self.wallets_by_owner[owner]
        self.vss("wallet-sign-statement",{"op":"sign-json","dir":str(wallet),"body":str(public/"statement.public.json"),"out":str(public/"owner-signature.json")})
        self.vss("verify-registered-wallet-signature",{"op":"verify-json","identity":str(wallet/"identity.json"),"signed":str(public/"owner-signature.json")})
        self.verified_statements[sha(statement)]=owner
        self.verified_bundles[sha(statement)]=checked["bundle_hash"]
        return statement

    def transition(self,tag,kind,prepare,context):
        coupled=self.cohort is not None and tag in ("claim","payment","receive","withdraw","promis","pledge-1","release-1","pledge-2")
        outputs={"claim":["gratis-1","payment-1","escrow-1"],"payment":["gratis-2","incoming-1"],"receive":["gratis-3","empty-change"],"withdraw":["gratis-4"],"promis":["gratis-5"],"pledge-1":["pledge-liquid-1","pledge-ticket-1"],"release-1":["released-liquid","released-empty"],"pledge-2":["pledge-liquid-2","pledge-ticket-2"],"intex-cashout":["intex-payout-change"]}
        context.update(operation_id=tag,output_ids=outputs[tag])
        if coupled:context["owner"]=self.cohort.owner
        if "owner" not in context:raise ValueError("missing registered owner")
        _,key=self.cipher_key(context["owner"])
        context.update(encryption_key=read(key),encryption_key_epoch=0)
        if coupled:self.cohort.bind_context(context)
        private=self.private/f"wallet-operation-{tag}";public=self.public/f"operation-{tag}"
        self.job("poc-state","wallet-prepare",{"op":"prepare","kind":kind,"out":str(private),"context":context,**prepare})
        statement=self.prove_prepared(tag,kind,private,public,context)
        if coupled:self.cohort.prepare(tag,kind,private,statement,context)
        return private,public,statement

    def core_claim(self,wallet,nod):
        super().core_claim(wallet,nod)
        # A separate fresh process has only its encryption key and PUBLIC
        # ciphertexts, not historical encryption openings or balance witnesses.
        secret,_=self.cipher_key(nod["owner"])
        self.twisted("cold-key-only-recovery",{"op":"recover","secret":str(secret),"ciphers":str(self.public/"operation-promis/ciphertexts.json"),"private_out":str(wallet/"key-only-recovered.private.json"),"out":str(self.public/"key-only-recovery.json")},limit=WALLET_LIMIT)
        from cross_owner import run_cross_owner
        run_cross_owner(self)

    def finish(self,*args):
        super().finish(*args)
        report=read(self.public/"result.json")
        report.update(backend="twisted-elgamal-bulletproofs-sigma-with-baby-bridge",same_baseline_vss_mpc=True,retained_economics_groth16=["claim","mint","pledge"],wallet_operations={p.parent.name:read(p) for p in self.public.glob("operation-*/twisted-resources.json")},cipher_registry_bytes=self.cipher_registry.stat().st_size,cipher_registry_entries=len(read(self.cipher_registry)),encryption_key_count=len(self.cipher_keys))
        write(self.public/"result.json",report)

def main():
    p=argparse.ArgumentParser();p.add_argument("--count",type=int,default=4);p.add_argument("--su",type=int,default=32);p.add_argument("--out",type=Path,required=True)
    a=p.parse_args()
    if not 3<=a.count<=4096 or not 1<=a.su<=1024:raise ValueError("experimental profile requires a third owner separate from the genesis-history fixture")
    TwistedRun(a.out.resolve(),a.count,a.su).run()
if __name__=="__main__":main()
