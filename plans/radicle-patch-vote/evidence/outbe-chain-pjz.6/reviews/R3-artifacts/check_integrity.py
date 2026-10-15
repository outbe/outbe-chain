from pathlib import Path
import json,subprocess,hashlib,datetime
r=Path.cwd();c=r/'plans/radicle-patch-vote/evidence/outbe-chain-pjz.6/candidate';o=c.parent/'reviews/R3-artifacts';h=lambda b:hashlib.sha256(b).hexdigest();head=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip();assert head=='91644eb78148e7b81cd8e46478dfb0466d321e27';assert not subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],text=True)
assert h((c/'README.md').read_bytes())=='6955f1bbd1406a6afbcf0d8f0f95dfb83f710bc81c05d5938d6ca6a4149b8c72';assert h((c/'artifact-hashes.json').read_bytes())=='697180a235c606fd225d9be65dbc84b58db8a3cc38e0ab965650757f00bd4e54'
a=json.loads((c/'artifact-hashes.json').read_text());s=json.loads((c/'source-hashes.json').read_text());
for p,want in a.items():assert h((c/p).read_bytes())==want,p
for p,want in s.items():
 b=(r/p).read_bytes();assert h(b)==want,p;assert b==subprocess.check_output(['git','show',head+':'+p]),p
base='8fe62b5dd0e625cd65297479cfa687e3e648ad41';changed=subprocess.check_output(['git','diff','--name-only',base,head],text=True).splitlines();assert set(changed)==set(s)
sp=r/'plans/radicle-patch-vote/task-specs.json';assert h(sp.read_bytes())=='5d26f9ef602867a7ed86ed9954d32f4379ec5fb867fa925208db9aa36a9b0412';spec=next(x for x in json.loads(sp.read_text()) if x['key']=='P5');assert spec==json.loads((c/'spec.json').read_text())
parent=json.loads(subprocess.check_output(['bd','--readonly','show','outbe-chain-pjz.6','--json']))[0];assert all(x in parent['description'] for k in ('changes','ac','tests') for x in spec[k]);(o/'parent.json').write_text(json.dumps(parent,ensure_ascii=False,indent=2)+'\n')
res={'timestamp':datetime.datetime.now(datetime.timezone.utc).isoformat(),'head':head,'base':base,'source_hashes':s,'artifact_hashes':a,'artifact_index_sha256':h((c/'artifact-hashes.json').read_bytes()),'whole_spec_sha256':h(sp.read_bytes()),'task_spec_sha256':h((c/'spec.json').read_bytes()),'parent_description_sha256':h(parent['description'].encode()),'changed_files':changed};(o/'integrity.json').write_text(json.dumps(res,indent=2)+'\n');print('PASS HEAD/source-clean,27changed source hashes and15artifacts,whole/task spec/live parent agreement');print('parent description SHA256',res['parent_description_sha256'])
