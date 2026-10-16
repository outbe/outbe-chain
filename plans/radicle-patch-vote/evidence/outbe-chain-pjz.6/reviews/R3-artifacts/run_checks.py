import subprocess,os,json,hashlib,datetime,platform
from pathlib import Path
r=Path.cwd();c=r/'plans/radicle-patch-vote/evidence/outbe-chain-pjz.6/candidate';o=c.parent/'reviews/R3-artifacts';src=json.loads((c/'commands.json').read_text());checks=src['commands']+[dict(name='targeted-clippy',**{k:v for k,v in json.loads((c/'targeted-clippy-command.json').read_text()).items() if k in ('argv','cwd')})];now=lambda:datetime.datetime.now(datetime.timezone.utc).isoformat();res={'platform':platform.platform(),'cwd':str(r),'environment':src['environment'],'toolchain':{x:subprocess.check_output([x,'--version'],text=True).strip() for x in ('cargo','rustc','forge','python3')},'commands':[]}
for cmd in checks:
 name=cmd['name'];rec={'name':name,'argv':cmd['argv'],'cwd':cmd['cwd'],'started':now()}
 with (o/(name+'.log')).open('wb') as f:p=subprocess.run(cmd['argv'],cwd=cmd['cwd'],env=os.environ|src['environment'],stdout=f,stderr=subprocess.STDOUT)
 rec.update(exit_code=p.returncode,finished=now(),log=name+'.log',sha256=hashlib.sha256((o/(name+'.log')).read_bytes()).hexdigest());res['commands'].append(rec);(o/'commands.json').write_text(json.dumps(res,indent=2)+'\n');print(name,p.returncode,flush=True)
 if name=='abi' and p.returncode==0:
  assert json.loads((o/'abi.log').read_text())==json.loads((r/'contracts/precompiles/abi-export/IRadicleRegistry.json').read_text());print('ABI export equals forge generated JSON',flush=True)
