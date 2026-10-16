from pathlib import Path
import subprocess,json,hashlib,os,datetime,platform
root=Path.cwd();p=root/'plans/radicle-patch-vote/evidence/outbe-chain-pjz.1';out=p/'reviews/round5/R2-artifacts';manifest=p/'review-candidate-r5.json';expected='7c133b279ce218ce84f5b794d5ba49cbca0722c64f866372c7e95c8cd8d13582'
assert hashlib.sha256(manifest.read_bytes()).hexdigest()==expected
c=json.load(open(manifest));head=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip();assert head=='6dcea234af6d9018f1661232490a115f5d0cf77f'
for f,h in c['artifacts'].items():assert hashlib.sha256(Path(f).read_bytes()).hexdigest()==h,f
for f in c['source_paths']:assert subprocess.check_output(['git','show',c['reviewed_sha']+':'+f])==Path(f).read_bytes(),f
changed=subprocess.check_output(['git','diff','--name-only',c['reviewed_sha']+'..'+head],text=True).splitlines();assert all(f.startswith('plans/radicle-patch-vote/evidence/outbe-chain-pjz.1/') for f in changed)
parent=json.loads(subprocess.check_output(['bd','--readonly','show','outbe-chain-pjz.1','--json']))[0];assert hashlib.sha256(parent['description'].encode()).hexdigest()==c['spec_hash']
checks=[]
for old in json.load(open(p/'round5/commands.json'))['commands']:
 cmd=old['argv'];env=os.environ.copy();env.update(old['environment']);
 if old['name']=='design-check':env['CARGO_TARGET_DIR']='/private/tmp/outbe-radicle-p0-r2-round4'
 name=old['name'];stdout=out/(name+'.stdout.log');stderr=out/(name+'.stderr.log');start=datetime.datetime.now(datetime.timezone.utc).isoformat()
 with stdout.open('w') as a,stderr.open('w') as b:r=subprocess.run(cmd,cwd=root,env=env,stdout=a,stderr=b)
 checks.append(dict(name=name,argv=cmd,cwd=str(root),environment={k:env[k] for k in old['environment']},started_utc=start,finished_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),exit_code=r.returncode,stdout_sha256=hashlib.sha256(stdout.read_bytes()).hexdigest(),stderr_sha256=hashlib.sha256(stderr.read_bytes()).hexdigest()))
 result=dict(reviewer='/root/p0_review_2',source_sha=c['reviewed_sha'],evidence_sha=head,manifest_sha256=expected,spec_sha256=c['spec_hash'],artifact_hashes_verified=len(c['artifacts']),source_paths_verified=len(c['source_paths']),evidence_only_paths=changed,platform=platform.platform(),checks=checks)
 (out/'commands.json').write_text(json.dumps(result,indent=2)+'\n');print(name,'exit',r.returncode,flush=True)
 assert r.returncode==0,name
print('PASS all9 checks and immutable source/evidence/spec binding',flush=True)
