from pathlib import Path
import subprocess,json,os,datetime,hashlib,platform
root=Path.cwd();o=root/'plans/radicle-patch-vote/evidence/outbe-chain-pjz.6/reviews/R2-artifacts';p=root/'plans/radicle-patch-vote/evidence/outbe-chain-pjz.6/candidate'
c=json.loads((p/'commands.json').read_text());env=os.environ.copy();env.update(c['environment']);results=[]
checks=c['commands']+[dict(json.loads((p/'targeted-clippy-command.json').read_text()),name='targeted-clippy')]
for check in checks:
 start=datetime.datetime.now(datetime.timezone.utc).isoformat();print('START',check['name'],start,flush=True)
 r=subprocess.run(check['argv'],cwd=check['cwd'],env=env,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
 log=o/(check['name']+'.log');log.write_bytes(r.stdout)
 item={'name':check['name'],'argv':check['argv'],'cwd':check['cwd'],'environment':c['environment'],'exit_code':r.returncode,'started_at_utc':start,'ended_at_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'log':log.name,'log_sha256':hashlib.sha256(r.stdout).hexdigest()}
 if check['name']=='abi' and r.returncode==0:item['matches_export']=json.loads(r.stdout)==json.loads((root/'contracts/precompiles/abi-export/IRadicleRegistry.json').read_text())
 results.append(item);(o/'commands.json').write_text(json.dumps({'platform':platform.platform(),'checks':results},indent=2)+'\n');print('END',check['name'],'exit',r.returncode,flush=True)
