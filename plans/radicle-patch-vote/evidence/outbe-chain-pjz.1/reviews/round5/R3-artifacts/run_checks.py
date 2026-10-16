import subprocess,json,os,platform,hashlib,datetime
from pathlib import Path
root=Path.cwd(); out=root/'plans/radicle-patch-vote/evidence/outbe-chain-pjz.1/reviews/round5/R3-artifacts'
now=lambda:datetime.datetime.now(datetime.timezone.utc).isoformat()
checks=[('source-verify',['python3','plans/radicle-patch-vote/p0-check/audit_sources.py'],{}),('design-check',['cargo','run','--offline','--locked','--quiet','--manifest-path','plans/radicle-patch-vote/p0-check/Cargo.toml'],{'CARGO_TARGET_DIR':'/private/tmp/outbe-radicle-p0-r3-round5','CARGO_BUILD_JOBS':'4'}),('verify-vectors',['python3','plans/radicle-patch-vote/p0-check/verify_vectors.py'],{}),('descriptor-check',['python3','plans/radicle-patch-vote/p0-check/descriptor_check.py'],{}),('schedule-check',['python3','plans/radicle-patch-vote/p0-check/schedule_check.py'],{}),('format',['rustfmt','--edition','2021','--check',*[f'plans/radicle-patch-vote/p0-check/src/{x}.rs' for x in ('main','publication','native')]],{}),('cycles',['bd','--readonly','dep','cycles','--json'],{})]
env={'CARGO_TARGET_DIR':'/private/tmp/outbe-radicle-p1-standalone','CARGO_PROFILE_TEST_DEBUG':'0','CARGO_PROFILE_DEV_DEBUG':'0','CARGO_INCREMENTAL':'0','CARGO_BUILD_JOBS':'4'}
base=['cargo','test','--offline','--locked','--manifest-path','vendor/heartwood/Cargo.toml','-p','radicle-protocol','--lib']
checks.extend([('production-wire',base+['wire::frame::test::native_namespace_frames_preserve_signed_encoding_and_transport_identity','--','--exact','--nocapture'],env),('protocol-regression',base,env)])
data={'cwd':str(root),'platform':platform.platform(),'started_utc':now(),'head':subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip(),'toolchain':{x:subprocess.check_output([x,'--version'],text=True).strip() for x in ('rustc','cargo','python3','git')},'commands':[]}
for name,argv,delta in checks:
 record={'name':name,'argv':argv,'cwd':str(root),'environment':delta,'started_utc':now()}
 with (out/(name+'.log')).open('wb') as f:
  p=subprocess.run(argv,env=os.environ|delta,stdout=f,stderr=subprocess.STDOUT)
 record.update(exit_code=p.returncode,finished_utc=now(),log=name+'.log',sha256=hashlib.sha256((out/(name+'.log')).read_bytes()).hexdigest())
 data['commands'].append(record);(out/'commands.json').write_text(json.dumps(data,indent=2)+'\n');print(name,p.returncode,flush=True)
print('ALL PASS' if all(x['exit_code']==0 for x in data['commands']) else 'FAIL',flush=True)
