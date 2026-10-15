import datetime,hashlib,json,os,pathlib,platform,subprocess,sys
root=pathlib.Path(__file__).resolve().parents[5]
out=pathlib.Path(__file__).resolve().parent
group=sys.argv[1]
source=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()
assert '0cc376bb8af2f17ea25d7c3b3db76f09c21a3268'==source,source
assert not subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],cwd=root,text=True)
env=os.environ.copy()
env.update(CARGO_TARGET_DIR='/private/tmp/outbe-radicle-p1-'+('standalone' if group=='native' else 'target'),CARGO_PROFILE_TEST_DEBUG='0',CARGO_PROFILE_DEV_DEBUG='0',CARGO_INCREMENTAL='0',CARGO_BUILD_JOBS='4')
env.update(CARGO_NET_OFFLINE='true',RUSTUP_TOOLCHAIN='1.96.0')
commands={
 'native':[
 ('native-tests',['cargo','test','--offline','--locked','--manifest-path','vendor/heartwood/Cargo.toml','-p','radicle','-p','radicle-cob','-p','radicle-crypto','-p','radicle-git-metadata','-p','radicle-node','-p','radicle-protocol','--lib','--','--nocapture']),
 ('native-clippy',['cargo','clippy','--offline','--locked','--manifest-path','vendor/heartwood/Cargo.toml','--workspace','--all-targets','--','-D','warnings']),
 ('native-format',['cargo','fmt','--manifest-path','vendor/heartwood/Cargo.toml','--all','--','--check'])],
 'root':[
 ('root-consumers',['cargo','check','--offline','--locked','-p','outbe-radicle-sidecar','-p','outbe-radicle-runtime','-p','outbe-keygen','-p','outbe-e2e-harness','--features','outbe-e2e-harness/ocomp-integration','--all-targets']),
 ('sidecar-tests',['cargo','test','--offline','--locked','-p','outbe-radicle-runtime','--test','sidecar','--','--nocapture'])]
}[group]
result={'scope':'Intermediate P2 actual receiver patch cache write/reopen/rebuild, not P2 acceptance','source_sha':source,'cwd':str(root),'platform':platform.platform(),'toolchain':{x:subprocess.check_output([x,'--version'],cwd=root,text=True).strip() for x in ['rustc','cargo']},'environment':{k:env[k] for k in ['CARGO_TARGET_DIR','CARGO_PROFILE_TEST_DEBUG','CARGO_PROFILE_DEV_DEBUG','CARGO_INCREMENTAL','CARGO_BUILD_JOBS','CARGO_NET_OFFLINE','RUSTUP_TOOLCHAIN']},'commands':[]}
for label,args in commands:
 start=datetime.datetime.now(datetime.timezone.utc).isoformat();log=out/(label+'.log')
 with log.open('wb') as f: p=subprocess.run(args,cwd=root,env=env,stdout=f,stderr=subprocess.STDOUT)
 row={'argv':args,'started_at_utc':start,'ended_at_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'exit_code':p.returncode,'log':log.name,'sha256':hashlib.sha256(log.read_bytes()).hexdigest()}
 result['commands'].append(row)
 (out/(group+'-commands.json')).write_text(json.dumps(result,indent=2)+'\n')
 print(label,'exit',p.returncode,flush=True)
 if p.returncode: sys.exit(p.returncode)
assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()==source
assert not subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],cwd=root,text=True)
