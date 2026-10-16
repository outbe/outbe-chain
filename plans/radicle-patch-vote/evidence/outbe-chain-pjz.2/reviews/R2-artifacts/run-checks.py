import sys,json,subprocess,os,datetime,hashlib,pathlib
root=pathlib.Path.cwd();bundle=root/'plans/radicle-patch-vote/evidence/outbe-chain-pjz.2';out=bundle/'reviews/R2-artifacts';group=sys.argv[1]
files=['final/root-commands.json','final/root-resumed-commands.json'] if group=='root' else ['final/heartwood-commands.json']
records=[]
for file in files:
 for old in json.load(open(bundle/file)):
  if old['name']=='harness-artifact-tests':continue
  env=os.environ.copy();env.update(old.get('env',{}))
  for key in old.get('unset_env',[]):env.pop(key,None)
  cmd=old['argv'];name=old['name'];stdout=out/(name+'.stdout.log');stderr=out/(name+'.stderr.log');start=datetime.datetime.now(datetime.timezone.utc).isoformat()
  with stdout.open('w') as a,stderr.open('w') as b: result=subprocess.run(cmd,cwd=root,env=env,stdout=a,stderr=b)
  row=dict(name=name,argv=cmd,cwd=str(root),start_utc=start,end_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),env=old.get('env',{}),unset_env=old.get('unset_env',[]),exit_code=result.returncode,stdout_sha256=hashlib.sha256(stdout.read_bytes()).hexdigest(),stderr_sha256=hashlib.sha256(stderr.read_bytes()).hexdigest())
  if name.startswith('metadata-') and result.returncode==0:
   meta=json.load(open(stdout));local=[p for p in meta['packages'] if '/vendor/heartwood/' in p['manifest_path']];assert local and all(p.get('source') is None for p in local)
   assert not any('outbe-heartwood' in str(p.get('source','')) for p in meta['packages']);row['local_heartwood_packages']=len(local);row['old_fork_sources']=0
  records.append(row);(out/(group+'-commands.json')).write_text(json.dumps(records,indent=2)+'\n');print(name,'exit',result.returncode,flush=True)
  if result.returncode:sys.exit(result.returncode)
