import json,hashlib,subprocess,os,platform,datetime
from pathlib import Path
r=Path.cwd();o=r/'plans/radicle-patch-vote/decisions/D1/reviews/R3-artifacts';s=r/'plans/radicle-patch-vote/decisions/D1/snapshot.json';raw=s.read_bytes();assert hashlib.sha256(raw).hexdigest()=='22783b25124ef5bfb6dffa29d667ba748c0da5c06aa216b55a96494c16c5bb75';x=json.loads(raw)
assert subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()==x['base_head']
verified=[]
for group in ('sources','artifacts'):
 for p,h in x[group].items():
  got=hashlib.sha256((r/p).read_bytes()).hexdigest();assert got==h,p;verified.append({'group':group,'path':p,'sha256':got})
data={'reviewer':'/root/p0_review_3','snapshot_sha256':hashlib.sha256(raw).hexdigest(),'base_head':x['base_head'],'platform':platform.platform(),'toolchain':{c:subprocess.check_output([c,'--version'],text=True).strip() for c in ('cargo','rustc','git','python3')},'verified':verified,'cwd':str(r),'argv':x['repro_command'],'environment':x['environment'],'started_utc':datetime.datetime.now(datetime.timezone.utc).isoformat()}
(o/'integrity-start.json').write_text(json.dumps(data,indent=2)+'\n')
with (o/'repro.log').open('wb') as f:p=subprocess.run(x['repro_command'],env=os.environ|x['environment'],stdout=f,stderr=subprocess.STDOUT)
data.update(exit_code=p.returncode,finished_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),log_sha256=hashlib.sha256((o/'repro.log').read_bytes()).hexdigest());(o/'repro-command.json').write_text(json.dumps(data,indent=2)+'\n');print('repro exit',p.returncode);print((o/'repro.log').read_text())
