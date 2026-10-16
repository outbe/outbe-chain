from pathlib import Path
import subprocess,json,hashlib,os
root=Path.cwd();p=root/'plans/radicle-patch-vote/evidence/outbe-chain-pjz.2';out=p/'reviews/R2-artifacts';sdk=Path('/private/tmp/outbe-heartwood-p1-upstream-7e1cb4');legacy=Path('/Users/sakor/.cargo/git/checkouts/outbe-heartwood-43c88058d57bbf76/b76a178')
base='0a322cf3ceafcc4bb8a23cae26fe22c7626409f4';ancestor='16de8f1950825672fe48b901dafa0d009f612b04';stable='7e1cb406bb935c3de008d2171e8961f57b4336ea'
def git(repo,*args):return subprocess.check_output(['git','-C',str(repo),*args])
def tree(repo,ref):
 result={}
 for row in git(repo,'ls-tree','-rz',ref).split(b'\0'):
  if row:
   meta,name=row.split(b'\t',1);mode,kind,oid=meta.decode().split();result[name.decode()]=(mode,oid)
 return result
assert git(sdk,'rev-parse','releases/1.10.3^{}').decode().strip()==stable
subprocess.run(['git','-C',str(sdk),'merge-base','--is-ancestor',ancestor,stable],check=True)
assert git(legacy,'show','-s','--format=%P','HEAD').decode().strip()==ancestor
v=tree(root,'HEAD:vendor/heartwood');s=tree(sdk,stable);a=tree(sdk,ancestor);prior=tree(root,base+':vendor/heartwood')
assert len(v)==817 and set(v)-set(s)=={'UPSTREAM.md'} and not set(s)-set(v)
rows=json.load(open(p/'vendor-tree.json'))['files'];assert set(r['path'] for r in rows)==set(v)
for row in rows:
 path=row['path'];f=root/'vendor/heartwood'/path;raw=os.readlink(f).encode() if f.is_symlink() else f.read_bytes()
 assert hashlib.sha256(raw).hexdigest()==row['sha256'],path
 assert v[path]==(row['mode'],row['blob_oid']),path
 assert hashlib.sha1(b'blob '+str(len(raw)).encode()+b'\0'+raw).hexdigest()==row['blob_oid'],path
 assert row['upstream_blob_oid']==s.get(path,(None,None))[1],path
 if row['mode']=='100755':assert f.stat().st_mode&0o111,path
retained={path for path in set(v)|set(s) if v.get(path)!=s.get(path)}
assert retained=={r['path'] for r in json.load(open(p/'retained-delta.json'))} and len(retained)==14
changed={path for path in set(a)|set(s) if a.get(path)!=s.get(path)}
transfer=json.load(open(p/'upstream-transfer.json'));assert changed=={r['path'] for r in transfer['upstream_changes']} and len(changed)==67
for row in transfer['upstream_changes']:
 assert a.get(row['path'])==(tuple(row['base']) if row['base'] else None)
 assert s.get(row['path'])==(tuple(row['upstream']) if row['upstream'] else None)
prior_delta={path for path in set(a)|set(prior) if a.get(path)!=prior.get(path)}
intersect=prior_delta&changed;assert intersect=={'crates/radicle-cli/Cargo.toml','crates/radicle/src/node/config.rs'}
for f in json.load(open(p/'retained-contract-check.json'))['byte_identical_to_pre_update']:assert v[f]==prior[f],f
for row in json.load(open(p/'unchanged-callers.json')):
 f=row['path'];raw=(root/f).read_bytes();assert hashlib.sha256(raw).hexdigest()==row['sha256'];assert raw==git(root,'show',base+':'+f)
actual=set(git(root,'diff','--name-only',base+'..HEAD').decode().splitlines());listed=json.load(open(p/'changed-files.json'));assert actual=={r['path'] for r in listed}
for row in listed:assert hashlib.sha256((root/row['path']).read_bytes()).hexdigest()==row['sha256']
for row in json.load(open(p/'wire-source-compatibility.json')):
 raw=(root/'vendor/heartwood'/row['path']).read_bytes();assert raw==git(sdk,'show',ancestor+':'+row['path']);assert hashlib.sha256(raw).hexdigest()==row['sha256']
result=dict(result='PASS',vendor_files=len(v),upstream_files=len(s),upstream_changed=len(changed),retained_count=len(retained),retained=sorted(retained),overlap=sorted(intersect),actual_changed_files=len(actual),source_sha=git(root,'rev-parse','HEAD').decode().strip())
(out/'source-verification.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result,indent=2))
