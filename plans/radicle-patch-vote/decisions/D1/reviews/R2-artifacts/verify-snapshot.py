from pathlib import Path
import hashlib,json,subprocess
root=Path.cwd()
p=root/'plans/radicle-patch-vote/decisions/D1/snapshot.json'
h=lambda b:hashlib.sha256(b).hexdigest()
expected='22783b25124ef5bfb6dffa29d667ba748c0da5c06aa216b55a96494c16c5bb75'
assert h(p.read_bytes())==expected
s=json.loads(p.read_text())
assert subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()==s['base_head']
result={'snapshot_sha256':expected,'base_head':s['base_head'],'checks':{}}
for kind in ['sources','artifacts']:
 result['checks'][kind]={}
 for path,digest in s[kind].items():
  got=h((root/path).read_bytes())
  result['checks'][kind][path]={'expected':digest,'actual':got,'match':got==digest}
  assert got==digest,(path,got,digest)
print(json.dumps(result,indent=2))
