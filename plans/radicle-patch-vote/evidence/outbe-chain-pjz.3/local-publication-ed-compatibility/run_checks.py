import datetime,hashlib,json,os,pathlib,platform,subprocess
root=pathlib.Path(__file__).resolve().parents[5]; out=pathlib.Path(__file__).resolve().parent
source=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip();assert '0dbd83c40ace71773a0e358809bd2f1876c18f37'==source
env=os.environ.copy();env.update(CARGO_TARGET_DIR='/private/tmp/outbe-radicle-p1-target',CARGO_PROFILE_TEST_DEBUG='0',CARGO_PROFILE_DEV_DEBUG='0',CARGO_INCREMENTAL='0',CARGO_BUILD_JOBS='4',CARGO_NET_OFFLINE='true',RUSTUP_TOOLCHAIN='1.96.0')
results={'scope':'P2 actual Ed compatibility after native BLS changes; not full P2 acceptance','source_sha':source,'cwd':str(root),'platform':platform.platform(),'environment':{k:env[k] for k in ['CARGO_TARGET_DIR','CARGO_PROFILE_TEST_DEBUG','CARGO_PROFILE_DEV_DEBUG','CARGO_INCREMENTAL','CARGO_BUILD_JOBS','CARGO_NET_OFFLINE','RUSTUP_TOOLCHAIN']},'commands':[]}
peer=['python3','plans/radicle-patch-vote/p1-peer-check.py','--rad','/private/tmp/outbe-radicle-p1-standalone/debug/rad','--helper','/private/tmp/outbe-radicle-p1-standalone/debug/git-remote-rad','--sidecar','/private/tmp/outbe-radicle-p1-target/debug/outbe-radicle']
commands=[('native-cli-build',['cargo','build','--offline','--locked','--manifest-path','vendor/heartwood/Cargo.toml','-p','radicle-cli','-p','radicle-remote-helper','--target-dir','/private/tmp/outbe-radicle-p1-standalone']),('sidecar-build',['cargo','build','--offline','--locked','-p','outbe-radicle-sidecar','--bin','outbe-radicle']),('current-peer',peer+['--evidence',str(out/'current-peer')]),('legacy-peer',peer+['--legacy-dir','/private/tmp/outbe-radicle-p1-legacy/debug','--evidence',str(out/'legacy-peer')])]
for label,args in commands:
 start=datetime.datetime.now(datetime.timezone.utc).isoformat();log=out/(label+'.log')
 with log.open('wb') as f:p=subprocess.run(args,cwd=root,env=env,stdout=f,stderr=subprocess.STDOUT)
 results['commands'].append({'argv':args,'started_at_utc':start,'ended_at_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'exit_code':p.returncode,'log':log.name,'sha256':hashlib.sha256(log.read_bytes()).hexdigest()})
 (out/'commands.json').write_text(json.dumps(results,indent=2)+'\n');print(label,'exit',p.returncode,flush=True)
 if p.returncode:raise SystemExit(p.returncode)
assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()==source
