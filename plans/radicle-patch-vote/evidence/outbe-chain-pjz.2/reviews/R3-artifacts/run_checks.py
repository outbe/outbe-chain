import sys,os,json,subprocess,hashlib,time,platform
from pathlib import Path
lane=sys.argv[1]
root=Path.cwd();out=Path(__file__).resolve().parent
env=os.environ.copy();env.update(CARGO_PROFILE_TEST_DEBUG="0",CARGO_PROFILE_DEV_DEBUG="0",CARGO_INCREMENTAL="0",CARGO_BUILD_JOBS="4")
env["CARGO_TARGET_DIR"]="/private/tmp/outbe-radicle-p1-"+("target" if lane=="root" else "standalone")
env["DOCKER_HOST"]="unix:///tmp/outbe-p1-no-daemon.sock";env.pop("DOCKER_CONTEXT",None)
manifest=["--manifest-path","vendor/heartwood/Cargo.toml"]
if lane=="root":
 commands=[("metadata-root",["cargo","metadata","--offline","--locked","--format-version","1"]),("fmt-root",["cargo","fmt","--all","--","--check"]),("root-check",["cargo","check","--offline","--locked","-p","outbe-radicle-sidecar","-p","outbe-radicle-runtime","-p","outbe-keygen","-p","outbe-e2e-harness","--features","outbe-e2e-harness/ocomp-integration","--all-targets"]),("runtime-clippy",["cargo","clippy","--offline","--locked","-p","outbe-radicle-runtime","-p","outbe-radicle-sidecar","--all-targets","--","-D","warnings"]),("sidecar-tests",["cargo","test","--offline","--locked","-p","outbe-radicle-runtime","--test","sidecar","--","--nocapture"]),("harness-artifact-tests",["cargo","test","--offline","--locked","-p","outbe-e2e-harness","--lib","artifacts::tests"]),("harness-version-test",["cargo","test","--offline","--locked","-p","outbe-e2e-harness","--lib","world::localnet::radicle::tests::user_tools_must_match_the_chain_revision","--","--exact"])]
else:
 commands=[("metadata-heartwood",["cargo","metadata","--offline","--locked",*manifest,"--format-version","1"]),("fmt-heartwood",["cargo","fmt",*manifest,"--all","--","--check"]),("standalone-check",["cargo","check","--offline","--locked",*manifest,"-p","radicle-cli","-p","radicle-remote-helper","--bins"]),("heartwood-clippy",["cargo","clippy","--offline","--locked",*manifest,"-p","radicle","-p","radicle-node","-p","radicle-protocol","-p","radicle-cli","-p","radicle-remote-helper","--all-targets","--","-D","warnings"]),("heartwood-tests",["cargo","test","--offline","--locked",*manifest,"-p","radicle-protocol","-p","radicle-node","--lib"]),("radicle-tests",["cargo","test","--offline","--locked",*manifest,"-p","radicle","--lib"])]
records=[]
for name,argv in commands:
 start=time.time();r=subprocess.run(argv,cwd=root,env=env,capture_output=True,timeout=600);raw=r.stdout+r.stderr
 record=dict(name=name,argv=argv,cwd=str(root),env={k:env[k] for k in ["CARGO_TARGET_DIR","CARGO_PROFILE_TEST_DEBUG","CARGO_PROFILE_DEV_DEBUG","CARGO_INCREMENTAL","CARGO_BUILD_JOBS","DOCKER_HOST"]},DOCKER_CONTEXT="unset",exit_code=r.returncode,elapsed_s=time.time()-start,reviewed_sha=subprocess.check_output(["git","rev-parse","HEAD"],text=True).strip())
 if name.startswith("metadata") and r.returncode==0:
  d=json.loads(r.stdout);packages=[x for x in d["packages"] if x["name"].startswith("radicle")];bad=[x for x in d["packages"] if "outbe-heartwood" in (x.get("source") or "")];assert not bad
  assert all(x.get("source") is None and str(root/"vendor/heartwood") in x["manifest_path"] for x in packages)
  summary=dict(package_count=len(d["packages"]),radicle=[dict(name=x["name"],manifest_path=x["manifest_path"],source=x.get("source")) for x in packages],old_fork_dependencies=bad,full_metadata_sha256=hashlib.sha256(r.stdout).hexdigest())
  (out/(name+".summary.json")).write_text(json.dumps(summary,indent=2)+"\n");raw=r.stderr
 log=out/(name+".log");log.write_bytes(raw);record.update(log=str(log.relative_to(root)),log_sha256=hashlib.sha256(raw).hexdigest());records.append(record);(out/(lane+"-commands.json")).write_text(json.dumps(records,indent=2)+"\n")
 print(name,"exit",r.returncode,"seconds",round(record["elapsed_s"],2),flush=True)
 if r.returncode:sys.exit(r.returncode)
print(lane,"ALL PASS",platform.platform(),flush=True)
