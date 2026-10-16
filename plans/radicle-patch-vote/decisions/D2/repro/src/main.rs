use radicle::git::raw::{Repository, Signature, Time};
use std::{fs, path::Path};

fn transaction_failure(removed_lock: usize) -> serde_json::Value {
    let temp = tempfile::tempdir().unwrap();
    let repo = Repository::init_bare(temp.path()).unwrap();
    let tree_id = repo.treebuilder(None).unwrap().write().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let actor = Signature::new("Atomicity fixture", "fixture@example.test", &Time::new(1700000000,0)).unwrap();
    let old = repo.commit(None, &actor, &actor, "old", &tree, &[]).unwrap();
    let parent = repo.find_commit(old).unwrap();
    let new = repo.commit(None, &actor, &actor, "new", &tree, &[&parent]).unwrap();
    let names = ["refs/heads/first", "refs/heads/second"];
    for name in names { repo.reference(name, old, false, "initial").unwrap(); }
    let mut tx = repo.transaction().unwrap();
    for name in names { tx.lock_ref(name).unwrap(); tx.set_target(name, new, Some(&actor), "candidate").unwrap(); }
    // Filesystem fault injection after successful prepare/locking, before commit.
    fs::remove_file(repo.path().join(format!("{}.lock", names[removed_lock]))).unwrap();
    let result = tx.commit();
    assert!(result.is_err());
    let current: Vec<_> = names.iter().map(|name| repo.refname_to_id(name).unwrap()).collect();
    serde_json::json!({"removed_lock":removed_lock,"error":result.unwrap_err().to_string(),"old":old.to_string(),"new":new.to_string(),"actual":current.iter().map(ToString::to_string).collect::<Vec<_>>(),"partial":current.iter().any(|id|*id==new)&&current.iter().any(|id|*id==old)})
}

fn write_json_atomic(path: &Path, value: &serde_json::Value) {
    let temp=path.with_extension("tmp");
    fs::write(&temp,serde_json::to_vec(value).unwrap()).unwrap();
    fs::File::open(&temp).unwrap().sync_all().unwrap();
    fs::rename(&temp,path).unwrap();
    fs::File::open(path.parent().unwrap()).unwrap().sync_all().unwrap();
}

fn ready_snapshot(root: &Path) -> Option<serde_json::Value> {
    let lease=fs::OpenOptions::new().read(true).write(true).open(root.join("publication.lock")).unwrap();
    lease.try_lock_shared().ok()?;
    if root.join("pending.json").exists() {return None;}
    let published:serde_json::Value=serde_json::from_slice(&fs::read(root.join("published.json")).unwrap()).unwrap();
    let actual:Vec<_>=["first","second"].iter().map(|name|fs::read_to_string(root.join(name)).unwrap()).collect();
    if published.get("refs") != Some(&serde_json::json!(actual)) {return None;}
    Some(published)
}

fn journal_model() -> serde_json::Value {
    let dir=tempfile::tempdir().unwrap();let root=dir.path();
    let lock=root.join("publication.lock");
    let reader=fs::OpenOptions::new().read(true).write(true).create_new(true).open(&lock).unwrap();
    let writer=fs::OpenOptions::new().read(true).write(true).open(&lock).unwrap();
    reader.lock_shared().unwrap();
    assert!(writer.try_lock().is_err(),"writer must not mutate a live exported snapshot");
    reader.unlock().unwrap();
    writer.lock().unwrap();
    let refs=[root.join("first"),root.join("second")];
    for path in &refs {fs::write(path,b"old").unwrap();}
    let published=root.join("published.json");let pending=root.join("pending.json");let archive=root.join("receipt-1.json");
    let previous=serde_json::json!({"sequence":1,"refs":["old","old"]});
    let target=serde_json::json!({"sequence":2,"refs":["new","new"]});
    write_json_atomic(&archive,&previous);write_json_atomic(&published,&previous);write_json_atomic(&pending,&target);
    assert!(reader.try_lock_shared().is_err(),"reader cannot enter while writer owns lease");
    fs::write(&refs[0],b"new").unwrap(); // crash/fault after the first physical write
    writer.unlock().unwrap();
    let actual:Vec<_>=refs.iter().map(|p|fs::read_to_string(p).unwrap()).collect();
    let archived:serde_json::Value=serde_json::from_slice(&fs::read(&published).unwrap()).unwrap();
    assert_eq!(archived,previous);assert_ne!(actual,vec!["old","old"]);assert_ne!(actual,vec!["new","new"]);
    assert!(pending.exists(),"read/export gate remains closed while pending");
    assert!(ready_snapshot(root).is_none());
    // Restart: reacquire exclusive lease, validate the accepted target, then resume it.
    let restart=fs::OpenOptions::new().read(true).write(true).open(&lock).unwrap();restart.lock().unwrap();
    let recovered:serde_json::Value=serde_json::from_slice(&fs::read(&pending).unwrap()).unwrap();
    assert_eq!(recovered,target); // fixture stand-in only, NOT an Outbe receipt verifier
    for path in &refs {fs::write(path,b"new").unwrap();fs::File::open(path).unwrap().sync_all().unwrap();}
    write_json_atomic(&published,&target);fs::remove_file(&pending).unwrap();fs::File::open(root).unwrap().sync_all().unwrap();restart.unlock().unwrap();
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&fs::read(&archive).unwrap()).unwrap(),previous);
    assert_eq!(ready_snapshot(root),Some(target));
    assert!(!pending.exists());assert_eq!(fs::read_to_string(&refs[0]).unwrap(),"new");assert_eq!(fs::read_to_string(&refs[1]).unwrap(),"new");
    serde_json::json!({"model_only":true,"archive_preserved":true,"physical_partial_detected":actual,"read_export_requires_pending_gate":true,"read_lease_blocks_writer":true,"restart_roll_forward":true,"final_sequence":2,"real_chain_policy_not_implemented":true})
}

fn main(){
 let cases:Vec<_>=(0..2).map(transaction_failure).collect();assert!(cases.iter().any(|v|v.get("partial").and_then(serde_json::Value::as_bool)==Some(true)),"expected actual Git transaction partial commit in one fault position");
 println!("{}",serde_json::to_string_pretty(&serde_json::json!({"meaning":"COUNTEREXAMPLE_REPRODUCED_AND_REDUCED_MECHANISM_MODEL_ONLY","git2_transaction_faults":cases,"journal_model":journal_model(),"not_a_vote_bypass_or_full_fetch_exploit":true})).unwrap());
}
