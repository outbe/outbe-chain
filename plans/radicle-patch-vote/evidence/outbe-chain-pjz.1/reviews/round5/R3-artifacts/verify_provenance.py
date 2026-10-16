import subprocess,json,hashlib,datetime
from pathlib import Path
r=Path.cwd();o=r/'plans/radicle-patch-vote/evidence/outbe-chain-pjz.1/reviews/round5/R3-artifacts';git=lambda *a:subprocess.check_output(['git',*a]);h=lambda b:hashlib.sha256(b).hexdigest()
s='4139bd9ef23c3320b6ac599c65e6123824131e07';e='6dcea234af6d9018f1661232490a115f5d0cf77f';sp='a0e36f17d4d4f8410f8e6c67668bab71857306569df154b8237b49bf6b172232';p='plans/radicle-patch-vote/evidence/outbe-chain-pjz.1/review-candidate-r5.json'
a=json.loads((r/p).read_bytes());assert h((r/p).read_bytes())=='7c133b279ce218ce84f5b794d5ba49cbca0722c64f866372c7e95c8cd8d13582'
assert git('rev-parse','HEAD').decode().strip()==e;assert git('rev-parse',e+'^').decode().strip()==s
changed=git('diff','--name-status',s,e).decode();assert all(line.split('\t')[-1].startswith('plans/radicle-patch-vote/evidence/outbe-chain-pjz.1/') for line in changed.splitlines())
rows=[]
for p,want in a['artifacts'].items():
 b=(r/p).read_bytes();assert h(b)==want,p;assert b==git('show',e+':'+p),p
 if p in a['source_paths']:assert b==git('show',s+':'+p),p
 rows.append({'path':p,'sha256':h(b)})
assert len(rows)==38
snap=[]
for i in range(1,10):
 bead=json.loads(subprocess.check_output(['bd','--readonly','show',f'outbe-chain-pjz.{i}','--json'],text=True))[0];snap.append(bead)
assert h(snap[0]['description'].encode())==sp
(o/'live-tasks.json').write_text(json.dumps(snap,ensure_ascii=False,indent=2)+'\n')
for i,task in enumerate(json.loads((r/'plans/radicle-patch-vote/task-specs.json').read_text())):
 for key in ('changes','tests','ac'):
  missing=[x for x in task[key] if x not in snap[i]['description']]
  assert not missing,(task['key'],key,missing)
res={'timestamp_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'source_sha':s,'evidence_sha':e,'spec_hash':sp,'manifest_sha256':h((r/p).read_bytes()) if False else '7c133b279ce218ce84f5b794d5ba49cbca0722c64f866372c7e95c8cd8d13582','artifacts':rows,'evidence_only_delta':changed.splitlines(),'source_paths_unchanged':len(a['source_paths']),'all_task_changes_tests_ac_present_in_live_descriptions':True}
(o/'provenance.json').write_text(json.dumps(res,indent=2)+'\n');print('PASS source/evidence pair,38 hashes,17 unchanged source paths,live P0 spec and P0-P8 task-map agreement');print(changed)
