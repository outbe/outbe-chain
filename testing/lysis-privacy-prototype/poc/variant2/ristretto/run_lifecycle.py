#!/usr/bin/env python3
"""Reproducible host PoC. Controller reads public artifacts only.

Each wallet/prover/holder is an explicitly separate subprocess. Local filesystem
paths do not imply isolation against the host administrator. This is a private
protocol/performance experiment, not a production node or consensus benchmark.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import struct
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent
BASE = HERE.parents[1]
REPO = BASE.parents[2]
BIN = HERE / "target/release"
L2 = REPO / "target/release/outbe-poc-l2-source"
LIMITER = BASE.parent / "measurements/run_with_ram_limit.py"
WALLET_LIMIT = 512_000_000

def read(path):
    if ".private." in str(path):
        raise AssertionError("public controller must not read private actor files")
    return json.loads(Path(path).read_text())

def write(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n")
    return str(path)

def sha(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()

def size(path):
    return sum(p.stat().st_size for p in Path(path).rglob("*") if p.is_file())

class Run:
    def __init__(self, out, count, su):
        self.out, self.count, self.su = out, count, su
        if out.exists():
            raise RuntimeError("Use a fresh --out directory; private witnesses are never overwritten")
        self.public = out / "public"
        self.private = out / "private"
        self.control = out / "control"
        for p in (self.public, self.private, self.control):
            p.mkdir(parents=True)
        self.sequence = 0
        self.timings = []
        self.failures = []
        self.cohort = None
        self.verified_statements = {}
        self.source_paths=sorted(list(HERE.glob("*.py"))+list((HERE/"src").rglob("*.rs"))+list((BASE/"source-helper/src").rglob("*.rs"))+list((BASE.parent/"measurements/p-link/src").rglob("*.rs"))+[HERE/"Cargo.lock",HERE/"Cargo.toml",BASE/"source-helper/Cargo.lock",BASE/"source-helper/Cargo.toml",REPO/"crates/core/lysis/src/algorithm.rs",REPO/"crates/core/fidelity-math/src/lib.rs"])
        self.source_snapshot={str(p.relative_to(REPO)):hashlib.sha256(p.read_bytes()).hexdigest() for p in self.source_paths}
        write(self.public/"source-snapshot.json",self.source_snapshot)
        self.env = dict(os.environ, BB_CRS_PATH=str(Path.home()/".bb-crs/bn254_g1.dat"))
        self.db = sqlite3.connect(self.public / "runtime.sqlite")
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.execute("PRAGMA synchronous=FULL")
        self.db.executescript("""
          CREATE TABLE tribute(id TEXT PRIMARY KEY,owner TEXT UNIQUE,body TEXT,epoch INTEGER);
          CREATE TABLE nod(id TEXT PRIMARY KEY,body TEXT,spent INTEGER DEFAULT 0);
          CREATE TABLE note(id TEXT PRIMARY KEY,asset TEXT,owner TEXT,commitments TEXT,spent INTEGER DEFAULT 0);
          CREATE TABLE operation(id TEXT PRIMARY KEY,kind TEXT,context TEXT);
          CREATE TABLE coen(id TEXT PRIMARY KEY,amount TEXT,owner TEXT);
          CREATE TABLE state(key TEXT PRIMARY KEY,value TEXT);
        """)
        self.db.execute("INSERT INTO state VALUES('day','offering')")
        self.db.commit()

    def command(self, name, command, limit=None, expect_fail=False):
        self.sequence += 1
        log = self.control / f"{self.sequence:05}-{name}.log"
        metrics = self.public / "metrics" / f"{self.sequence:05}-{name}.json"
        metrics.parent.mkdir(exist_ok=True)
        if limit:
            command = [sys.executable, str(LIMITER), "--limit", str(limit), "--json", str(metrics), "--", *map(str, command)]
        started = time.perf_counter()
        with log.open("w") as f:
            p = subprocess.run(list(map(str, command)), stdout=f, stderr=subprocess.STDOUT, env=self.env, cwd=REPO)
        elapsed = time.perf_counter()-started
        self.timings.append({"stage": name, "seconds": elapsed, "exit_code": p.returncode})
        if expect_fail:
            if p.returncode == 0:
                raise AssertionError(f"negative test accepted: {name}")
            self.failures.append(name)
        elif p.returncode:
            raise RuntimeError(f"{name} failed; inspect {log}:\n{log.read_text()[-3000:]}")
        if limit and not expect_fail and not read(metrics)["within_ram_limit"]:
            raise RuntimeError(f"{name} exceeded its memory limit")
        return metrics

    def job(self, binary, name, body, **kw):
        filename = write(self.control / f"job-{self.sequence+1:05}-{name}.json", body)
        return self.command(name, [(BASE/"target/release" if binary=="poc-math" else BIN)/binary, filename], **kw)

    def vss(self, name, body, **kw):
        return self.job("poc-vss", name, body, **kw)

    def math(self, name, body):
        out = self.public / f"{name}.json"
        self.job("poc-math", name, {**body, "out": str(out)})
        return read(out)

    def identity(self, directory, name):
        self.vss("keygen", {"op": "keygen", "dir": str(directory), "id": name})
        return read(directory / "identity.json")

    def committee(self, epoch):
        members = [self.private/f"epoch-{epoch}/party-{i}" for i in range(3)]
        registry = [self.identity(p, f"epoch-{epoch}-party-{i}") for i, p in enumerate(members)]
        path = write(self.public / f"registry-{epoch}.json", registry)
        return members, path

    def rotate(self, epoch, old, old_registry, polynomials):
        print(f"rotation {epoch}: {len(polynomials)} private VSS records", flush=True)
        members, registry = self.committee(epoch)
        old_poly = write(self.public / f"prior-{epoch}.json", polynomials)
        contributions = []
        for i in (0, 1):
            out = self.public / f"handoff-{epoch}-dealer-{i}.json"
            self.vss("reshare-dealer", {"op": "rotate-deal", "dir": str(old[i]), "x": i+1, "selected": [1,2], "epoch": epoch, "registry": registry, "out": str(out)})
            contributions.append(str(out))
        receipts = []
        for i, member in enumerate(members):
            out = self.public / f"handoff-{epoch}-receipt-{i}.json"
            p_out = self.public / f"polynomials-{epoch}-{i}.json"
            self.vss("reshare-recipient", {"op": "rotate-accept", "dir": str(member), "x": i+1, "selected": [1,2], "epoch": epoch, "old_registry": old_registry, "prior_polynomials": old_poly, "bundles": contributions, "out": str(out), "polynomials_out": str(p_out)})
            self.vss("verify-handoff-receipt",{"op":"verify-json","identity":str(member/"identity.json"),"signed":str(out)})
            receipts.append(read(out))
        assert len({r["body"]["coverage"] for r in receipts}) == 1
        return members, registry, read(self.public/f"polynomials-{epoch}-0.json")

    def mpc(self, tag, plan):
        out = self.public / tag
        out.mkdir()
        plan = {**plan, "out": str(out), "tag": tag}
        plan_path = write(self.control / f"{tag}-plan.json", plan)
        processes = []
        logs = []
        start = time.perf_counter()
        # Separate processes instead of one plaintext controller implementation.
        for i in range(3):
            log = (self.control/f"{tag}-party-{i}.log").open("w")
            logs.append(log)
            processes.append(subprocess.Popen([str(BASE/".venv-mpc/bin/python"), str(HERE/"mpc_worker.py"), plan_path, "-M3", "-T1", "-I", str(i), "--base-port", "17430"], cwd=REPO, env=self.env, stdout=log, stderr=subprocess.STDOUT))
        try:
            deadline = time.monotonic()+600
            while any(p.poll() is None for p in processes):
                if any(p.poll() not in (None,0) for p in processes):
                    raise RuntimeError(f"MPC {tag} process failed: inspect {self.control}/{tag}-party-*.log")
                if time.monotonic()>deadline:
                    raise TimeoutError(f"MPC {tag} ten-minute deadline")
                time.sleep(.2)
            if any(p.returncode for p in processes):
                raise RuntimeError(f"MPC {tag} failed")
        finally:
            for p in processes:
                if p.poll() is None:
                    p.terminate()
                    p.wait(timeout=10)
            for log in logs:
                log.close()
        rows = [read(out/f"party-{i}.json") for i in range(3)]
        assert len({sha({k:v for k,v in r.items() if k not in ("pid","elapsed_seconds","cpu_seconds","peak_rss_bytes","framed_sent")}) for r in rows}) == 1
        self.timings.append({"stage":tag,"seconds":time.perf_counter()-start,"framed_sent_bytes":sum(sum(r["framed_sent"].values()) for r in rows),"max_party_rss":max(r["peak_rss_bytes"] for r in rows)})
        return rows[0]

    def transition(self,*args,**kwargs):
        raise NotImplementedError("Use the complete Ristretto backend entry point run_variant.py")

    def note(self, id, asset, owner, points):
        self.db.execute("INSERT INTO note VALUES(?,?,?,?,0)", (id,asset,owner,json.dumps(points)))

    def commit_transition(self, tag, kind, statement, context, input_ids, output_ids, owner, asset="gratis", nod=None):
        # The full-context digest binds IDs, version, roots, terms and recipients
        # to the proof. Monetary proof verification has already succeeded in a node-only
        # process, before entering the atomic state transaction.
        if statement["kind"]!=kind or context.get("operation_id")!=tag or context.get("owner")!=owner:
            raise ValueError("operation kind/id/owner binding")
        if self.verified_statements.get(sha(statement))!=owner:
            raise ValueError("statement has no exact proof and registered-owner signature receipt")
        if context.get("input_ids")!=input_ids or context.get("output_ids")!=output_ids or len(set(input_ids))!=len(input_ids) or len(set(output_ids))!=len(output_ids):
            raise ValueError("noncanonical input/output identity set")
        allowed={"claim":("claim",2,3,"gratis"),"private-payment":("move",1,2,"gratis"),"receive":("move",2,2,"gratis"),"release":("move",2,2,"gratis"),"withdraw":("withdraw",1,1,"gratis"),"promis-convert":("mint",2,1,"gratis"),"pledge":("pledge",1,2,"gratis"),"intex-cashout":("withdraw",1,1,"coen-backed")}
        if context.get("op") not in allowed or allowed[context["op"]]!=(kind,len(input_ids),len(output_ids),asset):
            raise ValueError("operation asset/arity contract")
        needs_cohort=asset=="gratis"
        if needs_cohort and (self.cohort is None or self.cohort.owner!=owner or tag not in self.cohort.pending):
            raise ValueError("mandatory money/cohort certificate missing")
        field_mod = 21888242871839275222246405745257275088548364400416034343698204186575808495617
        rust_context = json.dumps(context, sort_keys=True, separators=(",", ":"))
        expected = int.from_bytes(hashlib.sha256(rust_context.encode()).digest(), "big") % field_mod
        assert int(statement["context"],16)==expected
        with self.db:
            if self.db.execute("SELECT 1 FROM operation WHERE id=?",(tag,)).fetchone():
                raise ValueError("operation replay")
            for i, id in enumerate(input_ids):
                r=self.db.execute("SELECT asset,owner,commitments,spent FROM note WHERE id=?",(id,)).fetchone()
                if not r or r[3] or r[1]!=owner or json.loads(r[2])!=statement["notes"][i]:
                    raise ValueError("stale/foreign note or commitment mismatch")
                expected_asset = ("payment-eur" if kind=="claim" else "promis") if kind in ("claim","mint") and i==1 else asset
                if context.get("op")=="release" and i==1:expected_asset="active-pledge"
                if r[0]!=expected_asset:
                    raise ValueError("asset mismatch")
            if kind=="claim":
                r=self.db.execute("SELECT body,spent FROM nod WHERE id=?",(nod,)).fetchone()
                if not r or r[1]:
                    raise ValueError("Nod already spent/unknown")
                n=json.loads(r[0])
                if n["owner"]!=owner or not n["called"] or n["deadline"]<self.now or statement["source"]!=n["commitment"] or int(statement["fraction"])!=n["fraction"] or statement["price"]!=n["price"]:
                    raise ValueError("claim terms/commitment mismatch")
                self.db.execute("UPDATE nod SET spent=1 WHERE id=?",(nod,))
                start=2
            elif kind=="move":
                if len(input_ids)==1 and statement["notes"][1]!=self.zero_points:
                    raise ValueError("unbacked private incoming note")
                start=2
            elif kind=="mint":
                start=2
            else:
                start=1
            for id in input_ids:
                self.db.execute("UPDATE note SET spent=1 WHERE id=?",(id,))
            for i,id in enumerate(output_ids):
                a = ("gratis","payment-eur","escrow-eur")[i] if kind=="claim" else (("gratis","pending-pledge")[i] if kind=="pledge" else asset)
                self.note(id,a,owner,statement["notes"][start+i])
            self.db.execute("INSERT INTO operation VALUES(?,?,?)",(tag,kind,json.dumps(context)))
            if kind=="withdraw":
                self.db.execute("INSERT INTO coen VALUES(?,?,?)",(tag,statement["amount"],context["recipient"]))
            if needs_cohort:
                self.cohort.commit(tag,context,statement)
        self.now+=60

    def source_prove(self, stage, parameters, manifest, report):
        # A fresh native actor for each mandatory part releases all allocator
        # arenas before the next PK. No verifier accepts a partial packet.
        parts=[];metrics=[]
        for part in range(13):
            output=self.public/f"{stage}-part-{part:02}.json"
            m=self.command(f"{stage}-part-{part:02}",[BIN/"outbe-ristretto-lifecycle-poc","batch-prove-part",parameters,manifest,part,output],limit=WALLET_LIMIT)
            metrics.append(read(m));parts.extend(read(output)["parts"])
            print(f"{stage}: completed part {part+1}/13",flush=True)
        summary={**read(output),"parts":parts,"batch_ms":sum(x["batch_ms"] for x in [read(self.public/f"{stage}-part-{i:02}.json") for i in range(13)]),"execution":"13 sequential fresh native processes"}
        write(report,summary)
        write(self.public/"metrics"/f"{self.sequence:05}-{stage}.json",{"exit_code":0,"ram_limit_bytes":WALLET_LIMIT,"peak_rss_bytes":max(m["peak_rss_bytes"] for m in metrics),"within_ram_limit":all(m["within_ram_limit"] for m in metrics),"wall_seconds":sum(m["wall_seconds"] for m in metrics),"measurement":"sum of 13 fresh native child walls; maximum of their high-water RSS; orchestration excluded","part_count":13})

    def run(self):
        print(f"fresh lifecycle: {self.count} distinct offers, {self.su} SU each",flush=True)
        parameters=HERE/f"parameters/native{self.su}"
        if not (parameters/"setup.json").exists():
            self.command("link-setup",[BIN/"outbe-ristretto-lifecycle-poc","setup",parameters,self.su],limit=2_000_000_000)
        wallets=[]; l2_rows=[]; link_rows=[]
        for i in range(self.count):
            wallet=self.private/f"wallet-{i}"; self.command("wallet",[BIN/"outbe-ristretto-lifecycle-poc","wallet",wallet,self.su,i+1000])
            self.identity(wallet,f"wallet-{i}")
            l2out=self.public/f"tribute-{i}/l2";linkout=self.public/f"tribute-{i}/link"
            wallets.append(wallet)
            l2_rows.append({"wallet":str(wallet/"wallet.private.json"),"out":str(l2out)})
            link_rows.append({"wallet":str(wallet/"wallet.private.json"),"out":str(linkout)})
        l2_manifest=write(self.control/"l2-manifest.json",{"rows":l2_rows,"report":str(self.public/"l2-generate.json"),"verify_report":str(self.public/"l2-verify.json")})
        link_manifest=write(self.control/"link-manifest.json",{"rows":link_rows,"report":str(self.public/"link-generate.json"),"verify_report":str(self.public/"link-verify.json")})
        self.command("real-l2-generation",[L2,"batch",l2_manifest],limit=2_000_000_000)
        for w,row in zip(wallets,l2_rows):
            self.command("wallet-bind-source",[BIN/"outbe-ristretto-lifecycle-poc","bind-wallet",w/"wallet.private.json",Path(row["out"])/"offer.public.json"])
        print("real P_L2 ready; proving distinct Ristretto P_link batch",flush=True)
        self.source_prove("warm-distinct-link",parameters,link_manifest,self.public/"link-generate.json")
        # Immutable verifier-owned copy: do not verify wallet-owned P_L2 bytes
        # and later use a separately mutable header as an admission statement.
        frozen_l2=[]
        for i,row in enumerate(l2_rows):
            out=self.control/f"node-l2-input-{i}";out.mkdir()
            raw=(Path(row["out"])/"p_l2.bin").read_bytes()
            (out/"p_l2.bin").write_bytes(raw)
            frozen_l2.append({"out":str(out)})
        frozen_manifest=write(self.control/"node-l2-manifest.json",{"rows":frozen_l2,"verify_report":str(self.public/"l2-verify.json")})
        self.command("node-l2-verify",[L2,"verify-batch",frozen_manifest],limit=2_000_000_000)
        self.command("node-link-verify",[BIN/"outbe-ristretto-lifecycle-poc","batch-verify",parameters,link_manifest],limit=2_000_000_000)
        # Also measure a fresh wallet with the full source+P_L2-bound public data.
        cold_manifest=write(self.control/"cold-link-manifest.json",{"rows":[{"wallet":str(wallets[0]/"wallet.private.json"),"out":str(self.public/"cold-link")}]})
        self.source_prove(f"cold-link-{self.su}",parameters,cold_manifest,self.public/"cold-link/prove.json")
        offers=[read(Path(row["out"])/"offer.public.json") for row in link_rows]
        receipts=read(self.public/"link-verify.json")["verified_rows"]
        if len(receipts)!=len(offers):raise ValueError("source receipt count")
        for o,row,receipt in zip(offers,link_rows,receipts):
            out=Path(row["out"])
            if receipt["out"]!=str(out) or receipt["offer_hash"]!=sha(o) or receipt["proof_sha256"]!=hashlib.sha256((out/"p_link.bin").read_bytes()).hexdigest() or receipt["opening_digests"]!=read(out/"opening.public.json")["digests"]:
                raise ValueError("source artifacts differ from exact verifier receipt")

        self.wallets_by_owner={o["derived_owner"]:w for o,w in zip(offers,wallets)}
        self.identity(self.private/"source-issuer","registered-test-L2-issuer")
        source_body=write(self.public/"source-registry.json",{"day":offers[0]["day"],"wallet_owners":{o["derived_owner"]:read(w/"identity.json") for o,w in zip(offers,wallets)},"roots":[o["merkle_root"] for o in offers],"oracle":{"issuance_vwap":offers[0]["issuance_vwap"],"reference_vwap":offers[0]["reference_vwap"],"reference_scurve":offers[0]["reference_scurve"]},"profile":"local authenticated L2 fixture adapter; production BLS registry not ported"})
        self.vss("source-sign",{"op":"sign-json","dir":str(self.private/"source-issuer"),"body":source_body,"out":str(self.public/"source-certificate.json")})
        self.vss("source-verify",{"op":"verify-json","identity":str(self.private/"source-issuer/identity.json"),"signed":str(self.public/"source-certificate.json")})
        for i,o in enumerate(offers):
            raw=(Path(frozen_l2[i]["out"])/"p_l2.bin").read_bytes()
            if raw!=(Path(l2_rows[i]["out"])/"p_l2.bin").read_bytes():raise ValueError("public P_L2 artifact differs from verified bytes")
            assert len(raw)==8900 and int.from_bytes(raw[:4],"big")==4
            assert [raw[4+k*32:36+k*32].hex() for k in range(4)]==[o[k] for k in ("derived_owner","nft_hash","binding_hash","merkle_root")]
        # Three private genesis cohort notes exercise nontrivial late Fidelity.
        genesis=[]
        for name in ("old-active","young-active","sold"):
            path=wallets[0]/f"{name}.private.json";pub=wallets[0]/f"{name}.public.json"
            self.job("poc-state","genesis-note",{"op":"note","random_bits":80,"out":str(path),"public":str(pub)})
            genesis.append({"id":name,"path":str(path)})
        members,registry=self.committee(0);epoch=0;polynomials=[];seen_su=set();receipts=[]
        admission_start=time.perf_counter()
        for i,(wallet,o) in enumerate(zip(wallets,offers)):
            if i==self.count//2:
                members,registry,polynomials=self.rotate(1,members,registry,sorted(polynomials,key=lambda p:p["id"]));epoch=1
            ids=o["source_ids"][:o["source_count"]]
            assert len(ids)==self.su and len(set(ids))==len(ids) and not seen_su.intersection(ids)
            seen_su.update(ids)
            self.vss("wallet-vss-witness",{"op":"wallet-secrets","wallet":str(wallet/"wallet.private.json"),"notes":genesis if i==0 else [],"out":str(wallet/"vss.private.json")})
            bundle=self.public/f"tribute-{i}/vss.json"
            self.vss("wallet-vss-deal",{"op":"deal","dir":str(wallet),"secrets":str(wallet/"vss.private.json"),"epoch":epoch,"registry":registry,"out":str(bundle)})
            b=read(bundle); assert b["polynomials"][0]["points"][0]==o["commitment"]
            if i==0:
                for n in genesis:
                    expected=read(wallet/f"{n['id']}.public.json")
                    actual=[next(p for p in b["polynomials"] if p["id"]==f"{n['id']}:{k}")["points"][0] for k in range(4)]
                    assert actual==expected
            receipts=[]
            for k,member in enumerate(members):
                out=self.public/f"tribute-{i}/receipt-{k}.json"
                self.vss("holder-verify-persist",{"op":"accept","dir":str(member),"bundle":str(bundle),"x":k+1,"epoch":epoch,"out":str(out)})
                self.vss("verify-admission-receipt",{"op":"verify-json","identity":str(member/"identity.json"),"signed":str(out)})
                receipts.append(read(out))
            assert len({r["body"]["coverage"] for r in receipts})==1
            all_ps=sorted(polynomials+b["polynomials"],key=lambda p:p["id"])
            expected=hashlib.sha256(json.dumps([{"id":p["id"],"epoch":p["epoch"],"points":p["points"]} for p in all_ps],separators=(",", ":")).encode()).hexdigest()
            assert all(r["body"]["coverage"]==expected and r["body"]["records"]==len(all_ps) and r["body"]["epoch"]==epoch for r in receipts)
            with self.db:
                self.db.execute("INSERT INTO tribute VALUES(?,?,?,?)",(o["nft_hash"],o["derived_owner"],json.dumps(o),epoch))
            polynomials.extend(b["polynomials"])
        self.timings.append({"stage":"admission_and_first_rotation","seconds":time.perf_counter()-admission_start,"distinct_records":self.count})
        members,registry,polynomials=self.rotate(2,members,registry,sorted(polynomials,key=lambda p:p["id"]))
        epoch=2
        root=sha([o["nft_hash"] for o in offers]);self.root=root
        # Freeze and replay state are durable public controller transitions.
        with self.db:self.db.execute("UPDATE state SET value='closed' WHERE key='day'")
        groups={"S":[o["nft_hash"] for o in offers]}
        common={"epoch":epoch,"root":root,"groups":groups}
        self.vss("reject-preclose-opening",{"op":"aggregate","dir":str(members[0]),"closed":False,**common,"out":str(self.public/"forbidden.json")},expect_fail=True)
        # Exact T(age) from current Rust kernel, public timestamp profile.
        now=2_000_000_000;self.now=now;old=now-400*86400;young=now-50*86400;sold_at=now-20*86400;qualified=old
        ages=[now-old,now-young,now-sold_at,now-qualified,800*86400]
        decay=list(map(int,self.math("decay",{"op":"decay","ages":ages})["values"]))
        account={"active":[{"id":"old-active","decay":str(decay[0])},{"id":"young-active","decay":str(decay[1])}],"sold":[{"id":"sold","held":str(decay[0]-decay[2])}],"qualified":qualified,"age":str(decay[3]),"maximum":str(decay[4])}
        mpc_common={"members":list(map(str,members)),"epoch":epoch,"context":{"root":root,"timestamp":now,"state_version":1}}
        fidelity=self.mpc("late-fidelity",{"op":"fidelity","accounts":[account],**mpc_common})
        leagues=[fidelity["leagues"][0]]+[1]*(self.count-1)
        for league in sorted(set(leagues)):groups[f"league-{league}"]=[o["nft_hash"] for o,l in zip(offers,leagues) if l==league]
        common["groups"]=groups
        opening=[]
        for i in (0,1):
            out=self.public/f"aggregate-{i}.json";opening.append(str(out))
            self.vss("aggregate-share",{"op":"aggregate","dir":str(members[i]),"closed":True,**common,"out":str(out)})
        polyfile=write(self.public/"final-polynomials.json",polynomials)
        self.vss("verify-open-aggregates",{"op":"open","polynomials":polyfile,"registry":registry,"shares":opening,**common,"out":str(self.public/"aggregate.json")})
        sums=read(self.public/"aggregate.json")["sums"]
        league_keys=[f"league-{l}" for l in sorted(set(leagues))]
        assert sum(int(sums[k]) for k in league_keys)==int(sums["S"])
        budget=int(sums["S"])*250_000*1_000_000
        start=time.perf_counter()
        math=self.math("lysis",{"op":"lysis","sums":[sums[k] for k in league_keys],"counts":[len(groups[k]) for k in league_keys],"budget18":str(budget)})
        fractions={l:int(f) for l,f in zip(sorted(set(leagues)),math["fractions"])}
        assert int(math["reserved18"])+int(math["unused18"])==budget
        lysisroot=sha({"root":root,"fractions":fractions,"budget18":str(budget)})
        nods=[];chunks=[]
        for start_index in range(0,self.count,256):
            tick=time.perf_counter();blob=bytearray()
            for i in range(start_index,min(start_index+256,self.count)):
                o=offers[i];l=leagues[i];f=fractions[l]
                price=int(o["reference_vwap"]);floor=max(int(o["reference_scurve"]),price)*108//100
                n={"version":1,"source":o["nft_hash"],"owner":o["derived_owner"],"day":o["day"],"league":l,"commitment":o["commitment"],"fraction":f,"price":str(price),"floor":str(floor),"reference_currency":o["reference_currency"],"deadline":now+86400,"root":root,"lysisroot":lysisroot,"called":f>0,"kind":"Nod" if f>0 else "NoEntitlement"}
                row=struct.pack(">H32s32sQH32s32s32s32sHQ32s32s",1,bytes.fromhex(n["source"]),bytes.fromhex(n["owner"]),n["day"],l,bytes.fromhex(n["commitment"]),f.to_bytes(32,"big"),price.to_bytes(32,"big"),floor.to_bytes(32,"big"),n["reference_currency"],n["deadline"],bytes.fromhex(root),bytes.fromhex(lysisroot))
                assert len(row)==278;blob.extend(row);nods.append(n)
            chunk=self.public/f"nod-shard-{start_index//256}.bin"
            with chunk.open("wb") as output:output.write(blob);output.flush();os.fsync(output.fileno())
            with self.db:
                for n in nods[start_index:]:self.db.execute("INSERT INTO nod(id,body) VALUES(?,?)",(n["source"],json.dumps(n)))
            chunks.append({"records":len(blob)//278,"bytes":len(blob),"elapsed_ms":(time.perf_counter()-tick)*1000.,"hash":hashlib.sha256(blob).hexdigest()})
        write(self.public/"nods.json",nods)
        write(self.public/"worker.json",{"shards":chunks,"elapsed_with_coefficient_kernel_ms":(time.perf_counter()-start)*1000.,"scope":"PoC descriptor construction, validation of admitted metadata, file fsync, SQLite commit; no production OCOMP/consensus"})
        print("verified aggregate and Nod descriptors ready; owner claims and private state proofs",flush=True)
        claim_index=next(i for i,n in enumerate(nods) if i>0 and n["called"])
        self.claim_index=claim_index
        self.current_members,self.current_registry=members,registry
        self.core_claim(wallets[claim_index],nods[claim_index])
        self.private_branches(wallets,offers,members,registry,polynomials,mpc_common,account,now,old,young,sold_at,qualified)
        self.finish(wallets,offers,members,polynomials,math,chunks)

    def core_claim(self,wallet,nod):
        owner=nod["owner"]
        from cohort_bridge import CohortBridge
        self.cohort=CohortBridge(self,wallet,owner,self.current_members,self.current_registry)
        self.job("poc-state","canonical-zero",{"op":"note","value":"0","canonical_zero":True,"out":str(wallet/"zero.private.json"),"public":str(wallet/"zero.public.json")})
        self.zero_points=read(wallet/"zero.public.json")
        for name,value in (("gratis-genesis",0),("payment-genesis",2**200)):
            self.job("poc-state","funding-note",{"op":"note","value":str(value),"out":str(wallet/f"{name}.private.json"),"public":str(wallet/f"{name}.public.json")})
            self.note(name,"gratis" if name.startswith("gratis") else "payment-eur",owner,read(wallet/f"{name}.public.json"))
        self.db.commit()
        context={"chain":19280501,"op":"claim","nod":nod["source"],"root":self.root,"input_ids":["gratis-genesis","payment-genesis"],"version":0,"deadline":nod["deadline"],"called":True}
        priv,pub,statement=self.transition("claim","claim",{"old":str(wallet/"gratis-genesis.private.json"),"wallet":str(wallet/"wallet.private.json"),"payment":str(wallet/"payment-genesis.private.json"),"fraction":str(nod["fraction"]),"price":nod["price"]},context)
        for fault in ("renamed-operation","duplicate-input","missing-input","missing-cohort-certificate","wrong-money-certificate"):
            args=["claim","claim",statement,context,["gratis-genesis","payment-genesis"],["gratis-1","payment-1","escrow-1"],owner]
            saved=None
            if fault=="renamed-operation":args[0]="renamed"
            elif fault=="duplicate-input":args[4]=["gratis-genesis","gratis-genesis"]
            elif fault=="missing-input":args[4]=["gratis-genesis"]
            elif fault=="missing-cohort-certificate":saved=self.cohort.pending.pop("claim")
            else:
                saved=self.cohort.pending["claim"]
                self.cohort.pending["claim"]={**saved,"request":{"domain":"wallet-statement","digest":"00"*32}}
            try:self.commit_transition(*args,nod=nod["source"])
            except ValueError:self.failures.append(fault)
            else:raise AssertionError(f"consumer accepted {fault}")
            finally:
                if saved is not None:self.cohort.pending["claim"]=saved
        self.now+=1
        try:self.commit_transition("claim","claim",statement,context,["gratis-genesis","payment-genesis"],["gratis-1","payment-1","escrow-1"],owner,nod=nod["source"])
        except ValueError:self.failures.append("stale timestamp rolls back money and cohort writes")
        else:raise AssertionError("stale candidate committed")
        self.now-=1
        assert self.db.execute("SELECT spent FROM nod WHERE id=?",(nod["source"],)).fetchone()==(0,)
        assert self.db.execute("SELECT COUNT(*) FROM operation WHERE id='claim'").fetchone()==(0,)
        self.commit_transition("claim","claim",statement,context,["gratis-genesis","payment-genesis"],["gratis-1","payment-1","escrow-1"],owner,nod=nod["source"])
        try:self.commit_transition("claim","claim",statement,context,["gratis-genesis","payment-genesis"],["duplicate-g","duplicate-p","duplicate-e"],owner,nod=nod["source"])
        except ValueError:self.failures.append("atomic Nod claim replay")
        else:raise AssertionError("replayed claim")
        # Private payment creates a pending incoming note. Receiver's old balance
        # is not supplied to the sender. This PoC uses one recovery identity for
        # self-transfer to keep ownership-key adapters out of the arithmetic test.
        context={"chain":19280501,"op":"private-payment","root":self.root,"input_ids":["gratis-1"],"version":1,"recipient":owner}
        moved,_,s=self.transition("payment","move",{"old":str(priv/"note-2.private.json"),"divisor":3},context)
        self.commit_transition("payment","move",s,context,["gratis-1"],["gratis-2","incoming-1"],owner)
        context={"chain":19280501,"op":"receive","root":self.root,"input_ids":["gratis-2","incoming-1"],"version":2}
        merged,_,s=self.transition("receive","move",{"old":str(moved/"note-2.private.json"),"incoming":str(moved/"note-3.private.json")},context)
        self.commit_transition("receive","move",s,context,["gratis-2","incoming-1"],["gratis-3","empty-change"],owner)
        context={"chain":19280501,"op":"withdraw","root":self.root,"input_ids":["gratis-3"],"version":3,"recipient":"public-coen-recipient"}
        withdrawn,_,s=self.transition("withdraw","withdraw",{"old":str(merged/"note-2.private.json"),"amount":str(10**18)},context)
        self.commit_transition("withdraw","withdraw",s,context,["gratis-3"],["gratis-4"],owner)
        # Exact fixed6 Promis burn -> fixed18 mint; the experimental source
        # registry authorizes one public burn ticket, consumed atomically.
        self.job("poc-state","private-promis-source",{"op":"note","random_bits":64,"out":str(wallet/"promis.private.json"),"public":str(wallet/"promis.public.json")})
        self.note("registered-promis-source","promis",owner,read(wallet/"promis.public.json"));self.db.commit()
        context={"chain":19280501,"op":"promis-convert","root":self.root,"input_ids":["gratis-4","registered-promis-source"],"version":4}
        minted,_,s=self.transition("promis","mint",{"old":str(withdrawn/"note-1.private.json"),"burn_note":str(wallet/"promis.private.json")},context)
        assert s["amount"]=="0"
        self.commit_transition("promis","mint",s,context,["gratis-4","registered-promis-source"],["gratis-5"],owner)
        self.latest_private=minted/"note-2.private.json"
        self.latest_note="gratis-5"
        self.owner=owner
        write(self.public/"core-cycle.json",{"real_p_l2":True,"real_p_link":True,"nod_claim":True,"private_payment_pending_and_receive":True,"public_coen_withdrawal":True,"promis_6_to_18":True,"proofs_verified":5,"payment_profile":"private asset notes backed by explicit experimental genesis funding; no production bridge adapter","promis_profile":"committed private test source consumed once; upstream Gem/Intex origins not ported"})

    def private_branches(self,*args):
        # Filled by the companion scenario module so the core controller never
        # acquires private amount/opening values for correctness comparisons.
        from lifecycle_branches import private_branches
        return private_branches(self,*args)

    def finish(self,wallets,offers,members,polynomials,math,chunks):
        if self.source_snapshot!={str(p.relative_to(REPO)):hashlib.sha256(p.read_bytes()).hexdigest() for p in self.source_paths}:raise RuntimeError("source changed during run; results are exploratory only")
        self.db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        write(self.public/"timings.json",self.timings)
        sizes=[size(self.public/f"tribute-{i}") for i in range(self.count)]
        report={"experimental_lifecycle_executed":True,"full_production_protocol_pass":False,"distinct_tributes":self.count,"claim_index":self.claim_index,"su_per_offer":self.su,"su_protocol_maximum":None,"host":os.uname().machine,"wallet_limit_bytes":WALLET_LIMIT,"tribute_artifact_bytes":{"min":min(sizes),"max":max(sizes),"mean":sum(sizes)/len(sizes)},"nod_record_bytes":278,"node_shards":chunks,"private_storage":{"wallet_0":size(wallets[0]),"committee_current_each":[size(m) for m in members],"role_directory_bytes":{p.name:size(p) for p in self.private.iterdir() if p.is_dir()},"retention":"all epochs, prepared operations, recovery and test oracle retained; not pruned"},"public_storage_bytes":size(self.public),"negative_scenarios":self.failures,"security":"experimental passive honest-majority MPC; single-party Groth16 setup; local transport/process experiment; not production consensus","scale_projections_not_executed":{"billion_tributes_payload_gb":sum(sizes)/len(sizes),"billion_nod_gb":278,"billion_min_nominal_local_share_gb":64,"billion_min_nominal_polynomial_points_gb":64},"reserved18":math["reserved18"],"unused18":math["unused18"]}
        write(self.public/"result.json",report)
        print(json.dumps({"completed":True,"result":str(self.public/"result.json"),"distinct_tributes":self.count}),flush=True)

def main():
    p=argparse.ArgumentParser();p.add_argument("--count",type=int,default=8);p.add_argument("--su",type=int,default=32);p.add_argument("--out",type=Path,required=True)
    args=p.parse_args()
    if not 2<=args.count<=4096 or not 1<=args.su<=1024:raise ValueError("experimental workload bounds; no protocol maximum implied")
    Run(args.out.resolve(),args.count,args.su).run()

if __name__=="__main__":main()
