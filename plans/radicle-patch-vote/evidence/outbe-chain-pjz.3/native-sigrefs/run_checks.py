import subprocess,os,json,hashlib,datetime,platform
from pathlib import Path
p=Path('plans/radicle-patch-vote/evidence/outbe-chain-pjz.3/native-sigrefs');p.mkdir(parents=True,exist_ok=True)
source=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()
checks=[('native-tests',['cargo','test','--offline','--locked','--manifest-path','vendor/heartwood/Cargo.toml','-p','radicle','-p','radicle-cob','-p','radicle-crypto','-p','radicle-git-metadata','-p','radicle-node','-p','radicle-protocol','--lib'],'/private/tmp/outbe-radicle-p1-standalone'),('clippy',['cargo','clippy','--offline','--locked','--manifest-path','vendor/heartwood/Cargo.toml','--workspace','--all-targets','--','-D','warnings'],'/private/tmp/outbe-radicle-p1-standalone'),('root-consumers',['cargo','check','--offline','--locked','-p','outbe-radicle-sidecar','-p','outbe-radicle-runtime','-p','outbe-keygen','-p','outbe-e2e-harness','--features','outbe-e2e-harness/ocomp-integration','--all-targets'],'/private/tmp/outbe-radicle-p1-target')]
record={'task_id':'outbe-chain-pjz.3','scope':'Intermediate native crypto/COB/signed-refs backends; P2 incomplete','source_sha':source,'cwd':str(Path.cwd()),'platform':platform.platform(),'toolchain':{x:subprocess.check_output([x,'--version'],text=True).strip() for x in ['rustc','cargo']},'commands':[]}
for name,argv,target in checks:
 assert subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()==source
 overrides={'CARGO_TARGET_DIR':target,'CARGO_PROFILE_TEST_DEBUG':'0','CARGO_PROFILE_DEV_DEBUG':'0','CARGO_INCREMENTAL':'0','CARGO_BUILD_JOBS':'4'}
 log=p/f'{name}.log';entry={'name':name,'argv':argv,'cwd':str(Path.cwd()),'environment':overrides,'source_sha':source,'started_utc':datetime.datetime.now(datetime.UTC).isoformat(),'log':str(log)}
 with log.open('w') as f:entry['exit_code']=subprocess.run(argv,env=dict(os.environ,**overrides),stdout=f,stderr=subprocess.STDOUT).returncode
 entry['finished_utc']=datetime.datetime.now(datetime.UTC).isoformat();entry['log_sha256']=hashlib.sha256(log.read_bytes()).hexdigest();record['commands'].append(entry)
 (p/'commands.json').write_text(json.dumps(record,indent=2)+'\n');print(name,entry['exit_code'],flush=True)
assert subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()==source
raise SystemExit(int(any(x['exit_code'] for x in record['commands'])))
