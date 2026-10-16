import json,subprocess,hashlib,os
from pathlib import Path
root=Path.cwd();base=root/"plans/radicle-patch-vote/evidence/outbe-chain-pjz.2";out=Path(__file__).resolve().parent
up=Path("/private/tmp/outbe-heartwood-p1-upstream-7e1cb4");fork=Path("/Users/sakor/.cargo/git/checkouts/outbe-heartwood-43c88058d57bbf76/b76a178")
ancestor="16de8f1950825672fe48b901dafa0d009f612b04";stable="7e1cb406bb935c3de008d2171e8961f57b4336ea";prior="0a322cf3ceafcc4bb8a23cae26fe22c7626409f4";head="eb834f31ccd027d7af604ba1393498491cc133e6"
def git(repo,*args):
 e=os.environ.copy();e["GIT_NO_LAZY_FETCH"]="1";return subprocess.check_output(["git","-C",str(repo),*args],env=e)
def tree(repo,ref,prefix=""):
 args=["ls-tree","-r","-z",ref]+([prefix] if prefix else []);d={}
 for row in git(repo,*args).split(b"\0"):
  if not row:continue
  left,name=row.split(b"\t",1);mode,typ,oid=left.decode().split();name=name.decode();name=name[len(prefix)+1:] if prefix else name;d[name]=(mode,typ,oid)
 return d
def raw(p):return os.readlink(p).encode() if p.is_symlink() else p.read_bytes()
assert git(root,"rev-parse","HEAD").decode().strip()==head
assert git(up,"rev-parse","releases/1.10.3^{}").decode().strip()==stable
assert git(fork,"rev-list","--parents","-n","1","HEAD").decode().split()==["b76a17801329291153585ed31db61ee3c658046e",ancestor]
subprocess.run(["git","-C",str(up),"merge-base","--is-ancestor",ancestor,stable],check=True)
manifest=json.loads((base/"review-candidate.json").read_text())
for name,h in manifest["artifacts"].items():assert hashlib.sha256(raw(root/name)).hexdigest()==h,name
spec=json.loads(subprocess.check_output(["bd","--readonly","show","outbe-chain-pjz.2","--json"]))[0];assert hashlib.sha256(spec["description"].encode()).hexdigest()==manifest["spec_hash"]
v=tree(root,head,"vendor/heartwood");u=tree(up,stable);a=tree(up,ancestor);old=tree(root,prior,"vendor/heartwood")
assert set(u)<=set(v) and set(v)-set(u)=={"UPSTREAM.md"}
vm=json.loads((base/"vendor-tree.json").read_text())["files"];assert len(vm)==len(v)==817
for x in vm:
 p=x["path"];b=raw(root/"vendor/heartwood"/p);assert hashlib.sha256(b).hexdigest()==x["sha256"],p;assert (x["mode"],"blob",x["blob_oid"])==v[p],p;assert x["upstream_blob_oid"]==(u[p][2] if p in u else None),p
retained={p for p in v if v[p]!=u.get(p)};r=json.loads((base/"retained-delta.json").read_text());assert retained=={x["path"] for x in r} and len(retained)==14
for x in r:
 p=x["path"];assert (x["mode"],"blob",x["blob_oid"])==v[p];assert hashlib.sha256(raw(root/"vendor/heartwood"/p)).hexdigest()==x["sha256"];assert x["upstream_blob_oid"]==(u[p][2] if p in u else None)
transfer=json.loads((base/"upstream-transfer.json").read_text());changes={p for p in set(a)|set(u) if a.get(p)!=u.get(p)};assert changes=={x["path"] for x in transfer["upstream_changes"]} and len(changes)==67
old_delta={p for p in set(a)|set(old) if a.get(p)!=old.get(p)};assert old_delta=={x["path"] for x in transfer["prior_local_delta"]}
contract=json.loads((base/"retained-contract-check.json").read_text())
for p in contract["byte_identical_to_pre_update"]:assert v[p]==old[p],p
for x in json.loads((base/"unchanged-callers.json").read_text()):
 b=raw(root/x["path"]);assert hashlib.sha256(b).hexdigest()==x["sha256"];assert b==git(root,"show",prior+":"+x["path"])
for x in json.loads((base/"wire-source-compatibility.json").read_text()):
 b=raw(root/"vendor/heartwood"/x["path"]);assert hashlib.sha256(b).hexdigest()==x["sha256"];assert v[x["path"]]==a[x["path"]]
changed=json.loads((base/"changed-files.json").read_text());paths=set(git(root,"diff","--name-only",prior,head).decode().splitlines());assert paths=={x["path"] for x in changed}
for x in changed:assert hashlib.sha256(raw(root/x["path"])).hexdigest()==x["sha256"]
record=dict(reviewed_sha=head,spec_hash=manifest["spec_hash"],candidate_manifest_sha256=hashlib.sha256((base/"review-candidate.json").read_bytes()).hexdigest(),artifact_hashes_verified=len(manifest["artifacts"]),vendor_files=817,stable_files=len(u),upstream_changed_paths=67,retained_delta=sorted(retained),old_delta_paths=len(old_delta),unchanged_retained_contract_files=contract["byte_identical_to_pre_update"],changed_files=len(changed),fork_parent=ancestor,stable_tag=stable,ancestor_to_stable=True,all_checks="PASS")
(out/"source-audit.json").write_text(json.dumps(record,indent=2)+"\n");print(json.dumps(record,indent=2))
